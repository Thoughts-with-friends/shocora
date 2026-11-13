//! Netplay as the window shows it: hosting, joining and their state.
use super::*;

impl ShocoraApp {
    fn start_hosting(&mut self) {
        self.netplay = Netplay::Off;
        let Some(commands) = self.console.commands() else { return };
        let config = match HostConfig::new(
            &self.config.netplay_address,
            DEFAULT_PORT,
            &self.netplay_password,
        ) {
            Ok(config) => config,
            Err(error) => {
                self.netplay_error = Some(error);
                return;
            }
        };
        self.netplay_network = config.network();
        let address = config.address;
        let [tv, gamepad] = self.presenter.stream_taps();
        match netplay::host(config, commands, tv, gamepad, self.last_gamepad) {
            Ok(hosting) => {
                self.console.send(ConsoleCommand::AudioTap(Some(hosting.host.audio_sink())));
                self.netplay = Netplay::Host(hosting);
                self.netplay_error = None;
                self.save_config();
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrNotAvailable => {
                self.netplay_error = Some(trf("netplay.not_this_pc", &[("address", &address)]));
            }
            Err(error) => {
                self.netplay_error = Some(format!("port {DEFAULT_PORT}: {error}"));
            }
        }
    }

    fn join(&mut self, ctx: &egui::Context) {
        self.stop_netplay();
        match netplay::join(&self.config.netplay_host, &self.netplay_password, ctx.clone()) {
            Ok(guest) => {
                self.netplay = Netplay::Guest(guest);
                self.netplay_error = None;
                self.save_config();
            }
            Err(error) => self.netplay_error = Some(error.to_string()),
        }
    }

    fn stop_netplay(&mut self) {
        if matches!(self.netplay, Netplay::Host(_)) {
            self.console.send(ConsoleCommand::AudioTap(None));
        }
        self.netplay = Netplay::Off;
    }

    /// What only applies while linked: the first player's rights on the host, and where the
    /// buttons go on a guest.
    pub(super) fn netplay_controls(&mut self, ui: &mut egui::Ui) {
        match &mut self.netplay {
            Netplay::Off => {}
            Netplay::Host(hosting) => {
                let guest_here = matches!(hosting.host.state(), LinkState::Connected { .. });
                let player1 = hosting.player1();
                let label =
                    if player1 { tr("netplay.take_player1") } else { tr("netplay.give_player1") };
                if ui
                    .add_enabled(guest_here, egui::Button::new(label))
                    .on_hover_text(tr("netplay.give_player1_hint"))
                    .clicked()
                {
                    hosting.set_player1(!player1);
                }
            }
            Netplay::Guest(guest) => {
                if guest.status().is_some_and(|status| status.player1) {
                    ui.label(tr("netplay.you_are_player1"));
                    if ui
                        .button(tr("netplay.return_player1"))
                        .on_hover_text(tr("netplay.return_player1_hint"))
                        .clicked()
                    {
                        guest.return_player1();
                    }
                } else {
                    let mut to_gamepad = guest.to_gamepad();
                    if ui
                        .checkbox(&mut to_gamepad, tr("netplay.to_gamepad"))
                        .on_hover_text(tr("netplay.to_gamepad_hint"))
                        .changed()
                    {
                        guest.set_to_gamepad(to_gamepad);
                    }
                }
            }
        }
    }

    pub(super) fn netplay_settings(&mut self, ui: &mut egui::Ui) {
        match self.netplay {
            Netplay::Off => {
                ui.label(tr("netplay.password"));
                ui.add(egui::TextEdit::singleline(&mut self.netplay_password).password(true));
                ui.separator();
                ui.label(tr("netplay.own_address"));
                ui.text_edit_singleline(&mut self.config.netplay_address)
                    .on_hover_text(tr("netplay.own_address_hint"));
                let ready = !self.config.netplay_address.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new(tr("netplay.start_host")))
                    .on_hover_text(tr("netplay.start_host_hint"))
                    .clicked()
                {
                    self.start_hosting();
                }
                ui.separator();
                ui.label(tr("netplay.host_address"));
                ui.text_edit_singleline(&mut self.config.netplay_host);
                let ready = !self.config.netplay_host.trim().is_empty();
                if ui.add_enabled(ready, egui::Button::new(tr("netplay.join"))).clicked() {
                    self.join(ui.ctx());
                }
            }
            Netplay::Host(_) | Netplay::Guest(_) => {
                if ui.button(tr("netplay.disconnect")).clicked() {
                    self.stop_netplay();
                    return;
                }
                self.netplay_controls(ui);
            }
        }
        if let Some(status) = self.netplay_status() {
            ui.separator();
            ui.label(status);
        }
    }
}

impl ShocoraApp {
    /// The guest's screens: what the host sends, laid out like the host's.
    pub(super) fn guest_screen(&mut self, ui: &mut egui::Ui) {
        let Netplay::Guest(guest) = &mut self.netplay else { return };
        let views = guest.views(ui.ctx());
        if views.iter().all(Option::is_none) {
            self.touch = None;
            ui.centered_and_justified(|ui| ui.label(tr("netplay.waiting_picture")));
            return;
        }
        let shown = [true, !self.config.gamepad_window];
        let (touch, double_clicked) = show_screens(ui, views, shown);
        self.touch = touch;
        if double_clicked {
            toggle_fullscreen(ui.ctx());
        }
    }

    pub(super) fn netplay_status(&self) -> Option<String> {
        let link = |state: &LinkState| match state {
            LinkState::Waiting => tr("netplay.waiting").to_owned(),
            LinkState::Connecting => tr("netplay.connecting").to_owned(),
            LinkState::Unreachable(_) => tr("netplay.unreachable").to_owned(),
            LinkState::Refused(Refusal::WrongPassword) => tr("netplay.wrong_password").to_owned(),
            LinkState::Refused(Refusal::Network) => tr("netplay.other_network").to_owned(),
            LinkState::Connected { peer, round_trip: Some(time) } => {
                format!("{peer} ping {} ms", time.as_millis())
            }
            LinkState::Connected { peer, round_trip: None } => peer.clone(),
        };
        match &self.netplay {
            Netplay::Off => self.netplay_error.as_ref().map(|error| format!("2P: {error}")),
            Netplay::Host(hosting) => Some(trf(
                "netplay.hosting",
                &[
                    ("network", &self.netplay_network),
                    ("port", &hosting.host.port()),
                    ("state", &link(&hosting.host.state())),
                ],
            )),
            Netplay::Guest(guest) => Some(format!("2P: {}", link(&guest.client.state()))),
        }
    }
}

impl ShocoraApp {
    /// What a guest's status bar shows: the host's state, which the guest cannot change.
    pub(super) fn guest_status_bar(&mut self, ui: &mut egui::Ui) {
        let Netplay::Guest(guest) = &self.netplay else { return };
        let linked = matches!(guest.client.state(), LinkState::Connected { .. });
        let status = guest.status().filter(|_| linked).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(state_text(status.state));
            ui.separator();
            ui.label(fps_text(status.fps, status.rate));
            ui.separator();
            ui.label(trf("status.speed", &[("speed", &status.speed)]));
            ui.separator();
            ui.label(if status.frame_generation {
                tr("status.framegen_on")
            } else {
                tr("status.framegen_off")
            });
            ui.separator();
            ui.label(if status.xbrz == 0 {
                tr("status.xbrz_off").to_owned()
            } else {
                format!("xBRZ {}x", status.xbrz)
            });
            ui.separator();
            ui.label(if status.title.is_empty() { tr("status.no_title") } else { &status.title });
            ui.separator();
            #[allow(clippy::cast_precision_loss)]
            let millions = format!("{:.1}", status.instructions as f64 / 1e6);
            ui.label(trf("status.instructions", &[("millions", &millions)]));
            if let Some(link) = self.netplay_status() {
                ui.separator();
                ui.label(link);
            }
            self.netplay_controls(ui);
        });
    }
}
