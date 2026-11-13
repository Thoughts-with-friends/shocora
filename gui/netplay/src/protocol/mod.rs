//! What travels over the connection: `tag: u8, length: u32, payload`, big-endian.
use std::{
    io::{self, Cursor, Read, Write},
    time::Instant,
};

use image::{ExtendedColorType, ImageFormat, ImageReader, Limits, codecs::jpeg::JpegEncoder};
use sha2::{Digest, Sha256};
use wiiu_peripherals::{
    GamePadState,
    controller::{Stick, VpadButtons},
};

mod media;
mod wire;
pub use media::*;
pub use wire::*;
/// Port the host listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 47_800;
/// Rate of the sound on the wire: interleaved stereo.
pub const AUDIO_RATE: u32 = 16_000;
/// Rate of the sound the emulator produces.
const INPUT_AUDIO_RATE: u32 = 48_000;
/// Bumped when the messages change: both sides must speak the same version.
const VERSION: u16 = 4;
/// Anything longer did not come from a peer speaking this protocol.
const MAX_PAYLOAD: usize = 4 << 20;
/// Largest picture a guest decodes.
const MAX_FRAME_SIDE: u32 = 4096;
const JPEG_QUALITY: u8 = 70;
/// Size of the GamePad's screen: a touch is never outside of it.
const TOUCH_SIZE: (u16, u16) = (854, 480);
/// Longest title a status carries, in bytes.
const MAX_STATUS_TITLE: usize = 256;

mod tag {
    pub const HELLO: u8 = 1;
    pub const PING: u8 = 2;
    pub const PONG: u8 = 3;
    pub const INPUT: u8 = 4;
    pub const VIDEO: u8 = 5;
    pub const AUDIO: u8 = 6;
    pub const CHALLENGE: u8 = 7;
    pub const ANSWER: u8 = 8;
    pub const WELCOME: u8 = 9;
    pub const REFUSED: u8 = 10;
    pub const STATUS: u8 = 11;
    pub const FRAME_SHOWN: u8 = 12;
    pub const RETURN_PLAYER1: u8 = 13;
}

/// Bytes of the random value a host greets a guest with.
const CHALLENGE_SIZE: usize = 16;
/// Bytes of a guest's answer: a SHA-256 digest.
const ANSWER_SIZE: usize = 32;

/// Why a host does not let a guest in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The guest is not on the host's network.
    Network,
    WrongPassword,
}

/// A fresh value for a guest to prove it knows the password with.
///
/// # Errors
/// Fails if the system has no randomness to give.
pub fn new_challenge() -> io::Result<[u8; CHALLENGE_SIZE]> {
    let mut challenge = [0; CHALLENGE_SIZE];
    getrandom::fill(&mut challenge).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(challenge)
}

/// What proves the password without sending it: a recorded answer is no use for the next
/// greeting, whose challenge differs.
pub fn answer(challenge: &[u8; CHALLENGE_SIZE], password: &str) -> [u8; ANSWER_SIZE] {
    let mut hash = Sha256::new();
    hash.update(b"shocora netplay");
    hash.update(challenge);
    hash.update(password.as_bytes());
    hash.finalize().into()
}

/// Whether two answers are equal, taking the same time wherever they differ.
#[inline]
pub fn answers_match(a: &[u8; ANSWER_SIZE], b: &[u8; ANSWER_SIZE]) -> bool {
    a.iter().zip(b).fold(0, |difference, (x, y)| difference | (x ^ y)) == 0
}

/// Which of the console's screens a picture shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Tv = 0,
    GamePad = 1,
}

/// A picture as RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub display: Display,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What a guest does with its controller.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GuestInput {
    pub pad: GamePadState,
    /// The buttons and sticks go to the GamePad instead of the Pro Controller.
    pub to_gamepad: bool,
}

/// What the emulator on the host is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostState {
    #[default]
    Idle,
    Running,
    Paused,
    Exited(i32),
    Crashed,
}

/// The frame rate setting of the host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FrameRate {
    #[default]
    Title,
    Max30,
    Force60,
}

/// What a guest's status bar shows about the host.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostStatus {
    pub state: HostState,
    /// Frames the title flips to the screen per second.
    pub fps: f32,
    pub rate: FrameRate,
    /// How fast the title runs, as a label.
    pub speed: String,
    pub frame_generation: bool,
    /// Factor of the xBRZ upscaling; zero when off.
    pub xbrz: u8,
    /// The running title's name; empty when there is none.
    pub title: String,
    pub instructions: u64,
    /// The guest plays as the first player.
    pub player1: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Hello,
    Ping(u64),
    Pong(u64),
    Input(GuestInput),
    /// A JPEG picture of a screen.
    Video(Display, Vec<u8>),
    /// Interleaved stereo at [`AUDIO_RATE`].
    Audio(Vec<i16>),
    Challenge([u8; CHALLENGE_SIZE]),
    Answer([u8; ANSWER_SIZE]),
    Welcome,
    Refused(Refusal),
    Status(HostStatus),
    /// The guest has taken in a picture of this screen: the host may send the next one.
    FrameShown(Display),
    /// The guest gives the first player's rights back to the host.
    ReturnPlayer1,
}

/// What a ping carries: its time of sending on the clock of the side that sends it.
pub(crate) fn nanos(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: &Message) -> Message {
        let mut bytes = Vec::new();
        write_message(&mut bytes, message).unwrap();
        read_message(&mut bytes.as_slice()).unwrap()
    }

    #[test]
    fn messages_survive_the_wire() {
        let pad = GamePadState {
            held: VpadButtons::A | VpadButtons::DOWN,
            left_stick: Stick { x: -0.5, y: 1.0 },
            right_stick: Stick { x: 0.25, y: 0.0 },
            touch: Some((12, 34)),
        };
        let status = HostStatus {
            state: HostState::Exited(-3),
            fps: 29.5,
            rate: FrameRate::Max30,
            speed: "2x".to_owned(),
            frame_generation: true,
            xbrz: 4,
            title: "ゲーム".to_owned(),
            instructions: u64::MAX - 1,
            player1: true,
        };
        for message in [
            Message::Hello,
            Message::Ping(7),
            Message::Pong(u64::MAX),
            Message::Input(GuestInput { pad, to_gamepad: true }),
            Message::Input(GuestInput::default()),
            Message::Video(Display::Tv, vec![1, 2, 3]),
            Message::Video(Display::GamePad, vec![4]),
            Message::Status(status),
            Message::Status(HostStatus::default()),
            Message::Audio(vec![-1, 2, i16::MIN, i16::MAX]),
            Message::Challenge([7; CHALLENGE_SIZE]),
            Message::Answer([9; ANSWER_SIZE]),
            Message::Welcome,
            Message::Refused(Refusal::Network),
            Message::Refused(Refusal::WrongPassword),
            Message::FrameShown(Display::Tv),
            Message::FrameShown(Display::GamePad),
            Message::ReturnPlayer1,
        ] {
            assert_eq!(round_trip(&message), message);
        }
    }

    #[test]
    fn answers_depend_on_both_challenge_and_password() {
        let (one, two) = ([1; CHALLENGE_SIZE], [2; CHALLENGE_SIZE]);
        assert!(answers_match(&answer(&one, "secret"), &answer(&one, "secret")));
        assert!(!answers_match(&answer(&one, "secret"), &answer(&one, "Secret")));
        assert!(!answers_match(&answer(&one, "secret"), &answer(&two, "secret")));
        assert_ne!(new_challenge().unwrap(), new_challenge().unwrap());
    }

    #[test]
    fn nonsense_is_refused() {
        // Longer than any real message: must fail before allocating that much.
        let huge = [tag::VIDEO, 0xFF, 0xFF, 0xFF, 0xFF];
        assert!(read_message(&mut huge.as_slice()).is_err());
        let unknown = [200, 0, 0, 0, 0];
        assert!(read_message(&mut unknown.as_slice()).is_err());
        let other_version = [tag::HELLO, 0, 0, 0, 2, 0xFF, 0xFF];
        assert!(read_message(&mut other_version.as_slice()).is_err());
        let short_input = [tag::INPUT, 0, 0, 0, 1, 0];
        assert!(read_message(&mut short_input.as_slice()).is_err());
        let no_screen = [tag::VIDEO, 0, 0, 0, 0];
        assert!(read_message(&mut no_screen.as_slice()).is_err());
        let short_status = [tag::STATUS, 0, 0, 0, 1, 0];
        assert!(read_message(&mut short_status.as_slice()).is_err());
    }

    #[test]
    fn stick_values_are_sane_whatever_arrives() {
        let mut payload = vec![0; 26];
        payload[4..8].copy_from_slice(&f32::NAN.to_be_bytes());
        payload[8..12].copy_from_slice(&5.0_f32.to_be_bytes());
        let pad = decode_input(&payload).unwrap().pad;
        assert_eq!((pad.left_stick.x, pad.left_stick.y), (0.0, 1.0));
        assert_eq!(pad.touch, None);
    }

    #[test]
    fn touches_stay_on_the_screen_whatever_arrives() {
        let mut payload = vec![0; 26];
        payload[20] = 1;
        payload[21..25].fill(0xFF);
        assert_eq!(decode_input(&payload).unwrap().pad.touch, Some((853, 479)));
    }

    #[test]
    fn long_titles_are_cut_between_characters() {
        let status = HostStatus { title: "あ".repeat(200), ..HostStatus::default() };
        let decoded = decode_status(&encode_status(&status)).unwrap();
        assert!(decoded.title.len() <= MAX_STATUS_TITLE);
        assert!(status.title.starts_with(&decoded.title));
    }

    #[test]
    fn pictures_keep_their_size_and_rough_color() {
        let frame = Frame {
            display: Display::GamePad,
            width: 32,
            height: 16,
            rgba: [200, 40, 10, 255].repeat(32 * 16),
        };
        let decoded = decode_frame(Display::GamePad, &encode_frame(&frame).unwrap()).unwrap();
        assert_eq!((decoded.display, decoded.width, decoded.height), (Display::GamePad, 32, 16));
        assert!(decoded.rgba[0].abs_diff(200) < 8 && decoded.rgba[1].abs_diff(40) < 8);
        assert!(decode_frame(Display::Tv, b"not a picture").is_err());
    }

    #[test]
    fn three_frames_of_sound_become_their_average() {
        let mut down = Downsampler::default();
        let mut out = Vec::new();
        down.push(&[3, 30, 6, 60], &mut out);
        assert!(out.is_empty(), "two frames are not enough yet");
        down.push(&[9, 90, 100, 100], &mut out);
        assert_eq!(out, [6, 60]);
    }
}
