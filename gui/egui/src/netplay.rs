//! The second player, as this window sees it: hosting one, or being one.
use std::sync::{Arc, Mutex, PoisonError, mpsc::Sender};

use eframe::egui;
use shocora_gui_common::{
    audio::{AudioOutput, Volume},
    console::ConsoleCommand,
    input::merge,
};
use shocora_netplay::{
    Client, ClientEvents, Display, Frame, GuestInput, Host, HostConfig, HostEvents, HostStatus,
    protocol,
};
use shocora_render::{Screen, StreamTap};
use wiiu_emu::GamePadState;

use crate::presenter::ScreenView;

/// The `WPAD` channel the guest's controller sits on: the first Pro Controller.
const GUEST_CHANNEL: usize = 0;

/// The window's role.
pub enum Netplay {
    Off,
    Host(Hosting),
    Guest(Guest),
}

/// Who gets which controller, from what the two players hold.
#[derive(Debug, Default, Clone, PartialEq)]
struct PadMixer {
    host: GamePadState,
    guest: Option<GuestInput>,
    /// The guest plays as the first player, and the host's own input goes nowhere.
    player1: bool,
}

impl PadMixer {
    /// What the title reads from the GamePad. The guest can always touch its screen.
    fn gamepad(&self) -> GamePadState {
        let Some(guest) = self.guest else { return self.host };
        if self.player1 {
            guest.pad
        } else if guest.to_gamepad {
            merge(self.host, guest.pad)
        } else {
            GamePadState { touch: self.host.touch.or(guest.pad.touch), ..self.host }
        }
    }

    /// What the title reads from the guest's Pro Controller. It stays plugged in while the
    /// buttons go to the GamePad, or titles would drop the second player.
    fn pro_controller(&self) -> Option<GamePadState> {
        let guest = self.guest.filter(|_| !self.player1)?;
        Some(if guest.to_gamepad {
            GamePadState::default()
        } else {
            GamePadState { touch: None, ..guest.pad }
        })
    }
}

/// What the window and the host's thread share.
struct HostControl {
    commands: Sender<ConsoleCommand>,
    /// The mixer and what it last told the emulator.
    mixer: Mutex<(PadMixer, GamePadState, Option<GamePadState>)>,
    status: Mutex<HostStatus>,
}

impl HostControl {
    /// Changes the mixer and tells the emulator what that changed for it.
    fn mix(&self, change: impl FnOnce(&mut PadMixer)) {
        let mut guard = self.mixer.lock().unwrap_or_else(PoisonError::into_inner);
        let (mixer, gamepad, pro) = &mut *guard;
        change(mixer);
        let new = mixer.gamepad();
        if new != *gamepad {
            *gamepad = new;
            let _ = self.commands.send(ConsoleCommand::GamePad(new));
        }
        let new = mixer.pro_controller();
        if new != *pro {
            *pro = new;
            let _ = self.commands.send(ConsoleCommand::ProController(GUEST_CHANNEL, new));
        }
        drop(guard);
    }

    fn player1(&self) -> bool {
        self.mixer.lock().unwrap_or_else(PoisonError::into_inner).0.player1
    }
}

/// Hosting a second player.
pub struct Hosting {
    pub host: Host,
    control: Arc<HostControl>,
}

impl Hosting {
    /// The host's own controller.
    pub fn set_pad(&self, pad: GamePadState) {
        self.control.mix(|mixer| mixer.host = pad);
    }

    /// Whether the guest plays as the first player.
    pub fn player1(&self) -> bool {
        self.control.player1()
    }

    /// Hands the first player's controls to the guest, or takes them back.
    pub fn set_player1(&self, player1: bool) {
        self.control.mix(|mixer| mixer.player1 = player1 && mixer.guest.is_some());
    }

    /// What the guest's status bar should show from now on.
    pub fn publish(&self, status: HostStatus) {
        *self.control.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }
}

struct HostLink {
    control: Arc<HostControl>,
    tv: StreamTap,
    gamepad: StreamTap,
}

impl HostEvents for HostLink {
    fn input(&self, input: Option<GuestInput>) {
        self.control.mix(|mixer| {
            mixer.guest = input;
            // The next guest has to be given these rights again.
            mixer.player1 &= input.is_some();
        });
    }

    fn streaming(&self, wanted: bool) {
        self.tv.set_enabled(wanted);
        self.gamepad.set_enabled(wanted);
    }

    fn take_frames(&self) -> Vec<Frame> {
        [(Display::Tv, &self.tv), (Display::GamePad, &self.gamepad)]
            .into_iter()
            .filter_map(|(display, tap)| {
                let frame = tap.take()?;
                Some(Frame { display, width: frame.width, height: frame.height, rgba: frame.rgba })
            })
            .collect()
    }

    fn status(&self) -> HostStatus {
        let mut status = self.control.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
        status.player1 = self.control.player1();
        status
    }

    fn return_player1(&self) {
        self.control.mix(|mixer| mixer.player1 = false);
    }
}

/// Starts letting a second player in. `tv` and `gamepad` are where its pictures come from.
///
/// # Errors
/// Fails if the port is taken or the address is not this machine's.
pub fn host(
    config: HostConfig,
    commands: Sender<ConsoleCommand>,
    tv: StreamTap,
    gamepad: StreamTap,
    pad: GamePadState,
) -> std::io::Result<Hosting> {
    let mixer = PadMixer { host: pad, ..PadMixer::default() };
    let control = Arc::new(HostControl {
        commands,
        mixer: Mutex::new((mixer, pad, None)),
        status: Mutex::default(),
    });
    let link = HostLink { control: Arc::clone(&control), tv, gamepad };
    Ok(Hosting { host: Host::start(config, link)?, control })
}

/// The newest picture of each screen, by [`Display`].
type Pictures = Arc<Mutex<[Option<Frame>; 2]>>;

/// Being a second player.
pub struct Guest {
    pub client: Client,
    pictures: Pictures,
    status: Arc<Mutex<Option<HostStatus>>>,
    textures: [Option<egui::TextureHandle>; 2],
    input: GuestInput,
}

impl Guest {
    /// The newest pictures from the host, TV first, always as large as on the console: a
    /// touch on the GamePad's picture means the same whatever size it came in.
    pub fn views(&mut self, ctx: &egui::Context) -> [Option<ScreenView>; 2] {
        let frames =
            std::mem::take(&mut *self.pictures.lock().unwrap_or_else(PoisonError::into_inner));
        for (slot, frame) in self.textures.iter_mut().zip(frames) {
            let Some(frame) = frame else { continue };
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            let options = egui::TextureOptions::LINEAR;
            match slot {
                Some(texture) => texture.set(image, options),
                None => *slot = Some(ctx.load_texture("netplay", image, options)),
            }
        }
        [Screen::Tv, Screen::Drc].map(|screen| {
            let display = match screen {
                Screen::Tv => Display::Tv,
                Screen::Drc => Display::GamePad,
            };
            let (width, height) = screen.default_size();
            #[allow(clippy::cast_precision_loss)]
            let size = egui::vec2(width as f32, height as f32);
            self.textures[display as usize]
                .as_ref()
                .map(|texture| ScreenView { texture: texture.id(), size })
        })
    }

    /// What the host last said about itself.
    pub fn status(&self) -> Option<HostStatus> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// This player's controller.
    pub fn set_pad(&mut self, pad: GamePadState) {
        self.input.pad = pad;
        self.client.set_input(self.input);
    }

    /// Whether the buttons go to the GamePad instead of the Pro Controller.
    pub const fn to_gamepad(&self) -> bool {
        self.input.to_gamepad
    }

    pub fn set_to_gamepad(&mut self, to_gamepad: bool) {
        self.input.to_gamepad = to_gamepad;
        self.client.set_input(self.input);
    }

    /// Gives the first player's rights back to the host.
    pub fn return_player1(&self) {
        self.client.return_player1();
    }
}

struct GuestLink {
    pictures: Pictures,
    status: Arc<Mutex<Option<HostStatus>>>,
    audio: AudioOutput,
    repaint: egui::Context,
}

impl ClientEvents for GuestLink {
    fn frame(&mut self, frame: Frame) {
        let slot = frame.display as usize;
        self.pictures.lock().unwrap_or_else(PoisonError::into_inner)[slot] = Some(frame);
        self.repaint.request_repaint();
    }

    fn audio(&mut self, samples: &[i16]) {
        self.audio.push(samples);
    }

    fn status(&mut self, status: HostStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
        self.repaint.request_repaint();
    }

    fn changed(&mut self) {
        self.repaint.request_repaint();
    }
}

/// Starts joining the host at `address`.
///
/// # Errors
/// Fails if the link's thread cannot start.
pub fn join(
    address: &str,
    password: &str,
    volume: Volume,
    repaint: egui::Context,
) -> std::io::Result<Guest> {
    let pictures = Pictures::default();
    let status = Arc::default();
    let client = Client::start(address, password, {
        let (pictures, status) = (Arc::clone(&pictures), Arc::clone(&status));
        move || GuestLink {
            pictures,
            status,
            // The output upsamples to what the device runs at.
            audio: AudioOutput::open(protocol::AUDIO_RATE, volume),
            repaint,
        }
    })?;
    Ok(Guest { client, pictures, status, textures: [None, None], input: GuestInput::default() })
}

#[cfg(test)]
mod tests {
    use wiiu_emu::VpadButtons;

    use super::*;

    fn pad(held: VpadButtons, touch: Option<(u16, u16)>) -> GamePadState {
        GamePadState { held, touch, ..GamePadState::default() }
    }

    fn mixer(guest_pad: GamePadState, to_gamepad: bool, player1: bool) -> PadMixer {
        PadMixer {
            host: pad(VpadButtons::A, None),
            guest: Some(GuestInput { pad: guest_pad, to_gamepad }),
            player1,
        }
    }

    #[test]
    fn without_a_guest_the_host_has_the_gamepad_alone() {
        let mixer = PadMixer { host: pad(VpadButtons::A, None), ..PadMixer::default() };
        assert_eq!(mixer.gamepad(), pad(VpadButtons::A, None));
        assert_eq!(mixer.pro_controller(), None);
    }

    #[test]
    fn a_second_player_has_the_pro_controller_and_the_touch_screen() {
        let mixer = mixer(pad(VpadButtons::B, Some((10, 20))), false, false);
        assert_eq!(mixer.gamepad(), pad(VpadButtons::A, Some((10, 20))));
        assert_eq!(mixer.pro_controller(), Some(pad(VpadButtons::B, None)));
    }

    #[test]
    fn buttons_sent_to_the_gamepad_add_to_the_hosts() {
        let mixer = mixer(pad(VpadButtons::B, None), true, false);
        assert_eq!(mixer.gamepad(), pad(VpadButtons::A | VpadButtons::B, None));
        assert_eq!(mixer.pro_controller(), Some(GamePadState::default()));
    }

    #[test]
    fn a_guest_with_the_first_players_rights_replaces_the_host() {
        let mixer = mixer(pad(VpadButtons::B, Some((1, 2))), false, true);
        assert_eq!(mixer.gamepad(), pad(VpadButtons::B, Some((1, 2))));
        assert_eq!(mixer.pro_controller(), None);
    }
}
