//! Controllers and keyboard to the GamePad the title sees.
use super::*;

impl ShocoraApp {
    pub(super) fn poll_input(&mut self, ctx: &egui::Context) {
        let changes = self.controllers.poll();
        self.log.extend(changes);
        let keyboard = ctx.input(|input| {
            self.config
                .key_bindings
                .gamepad(|name| egui::Key::from_name(name).is_some_and(|key| input.key_down(key)))
        });
        let keyboard = merge(keyboard, self.window_keys);
        let mut pad = merge(keyboard, self.controllers.gamepad(&self.config.pad_bindings));
        pad.touch = self.touch.or(self.window_touch);
        self.add_mic_and_motion(ctx, &mut pad);
        // The host's clock is not a guest's to change: its buttons all go to the title.
        if !matches!(self.netplay, Netplay::Guest(_)) {
            let button = pad.held.contains(VpadButtons::STICK_L);
            pad.held.remove(VpadButtons::STICK_L);
            let pressed = button && !self.speed_button_down;
            self.speed_button_down = button;
            let tab = !ctx.egui_wants_keyboard_input()
                && ctx.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(
                            event,
                            egui::Event::Key {
                                key: egui::Key::Tab,
                                pressed: true,
                                repeat: false,
                                ..
                            }
                        )
                    })
                });
            if pressed || tab {
                self.next_speed();
            }
        }
        if pad != self.last_gamepad {
            self.last_gamepad = pad;
            match &mut self.netplay {
                // A guest's controller belongs to the host's second player.
                Netplay::Guest(guest) => guest.set_pad(pad),
                // Who plays what depends on the guest, too.
                Netplay::Host(hosting) => hosting.set_pad(pad),
                Netplay::Off => self.console.send(ConsoleCommand::GamePad(pad)),
            }
        }
    }

    pub(super) fn next_speed(&mut self) {
        self.speed = self.speed.next();
        self.console.send(ConsoleCommand::Speed(self.speed));
    }
}
