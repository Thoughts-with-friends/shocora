//! How messages look on the wire: a tag, a length and the payload.
use super::*;

pub(crate) fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn encode_input(input: &GuestInput) -> Vec<u8> {
    let pad = &input.pad;
    let mut out = Vec::with_capacity(27);
    out.extend(pad.held.bits().to_be_bytes());
    for value in [pad.left_stick.x, pad.left_stick.y, pad.right_stick.x, pad.right_stick.y] {
        out.extend(value.to_be_bytes());
    }
    out.push(u8::from(pad.touch.is_some()));
    let (x, y) = pad.touch.unwrap_or_default();
    out.extend(x.to_be_bytes());
    out.extend(y.to_be_bytes());
    out.push(u8::from(input.to_gamepad));
    // Last, so that a host from before it reads the rest as it did.
    out.push((pad.mic.clamp(0.0, 1.0) * 255.0).round() as u8);
    out
}

pub(super) fn decode_input(payload: &[u8]) -> io::Result<GuestInput> {
    let word = |at: usize| -> io::Result<[u8; 4]> {
        payload
            .get(at..at + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| invalid("short input message"))
    };
    // What the other side sends is not trusted to be a real stick position.
    let axis = |at: usize| -> io::Result<f32> {
        let value = f32::from_be_bytes(word(at)?);
        Ok(if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 })
    };
    // A guest from before the microphone sends one byte less.
    let (touching, x0, x1, y0, y1, to_gamepad, mic) = match payload.get(20..) {
        Some(&[touching, x0, x1, y0, y1, to_gamepad]) => (touching, x0, x1, y0, y1, to_gamepad, 0),
        Some(&[touching, x0, x1, y0, y1, to_gamepad, mic]) => {
            (touching, x0, x1, y0, y1, to_gamepad, mic)
        }
        _ => return Err(invalid("short input message")),
    };
    let touch = (touching != 0).then(|| {
        (
            u16::from_be_bytes([x0, x1]).min(TOUCH_SIZE.0 - 1),
            u16::from_be_bytes([y0, y1]).min(TOUCH_SIZE.1 - 1),
        )
    });
    Ok(GuestInput {
        pad: GamePadState {
            held: VpadButtons::from_bits_truncate(u32::from_be_bytes(word(0)?)),
            left_stick: Stick { x: axis(4)?, y: axis(8)? },
            right_stick: Stick { x: axis(12)?, y: axis(16)? },
            touch,
            mic: f32::from(mic) / 255.0,
            ..GamePadState::default()
        },
        to_gamepad: to_gamepad != 0,
    })
}

/// The first `limit` bytes of `text` or fewer, without cutting a character in two.
fn clipped(text: &str, limit: usize) -> &str {
    let end = (0..=limit.min(text.len())).rev().find(|end| text.is_char_boundary(*end));
    &text[..end.unwrap_or(0)]
}

pub(super) fn encode_status(status: &HostStatus) -> Vec<u8> {
    let (state, code) = match status.state {
        HostState::Idle => (0, 0),
        HostState::Running => (1, 0),
        HostState::Paused => (2, 0),
        HostState::Exited(code) => (3, code),
        HostState::Crashed => (4, 0),
    };
    let rate = match status.rate {
        FrameRate::Title => 0,
        FrameRate::Max30 => 1,
        FrameRate::Force60 => 2,
    };
    let speed = clipped(&status.speed, u8::MAX as usize);
    let mut out = vec![state];
    out.extend(code.to_be_bytes());
    out.extend(status.fps.to_be_bytes());
    out.extend(status.instructions.to_be_bytes());
    out.extend([rate, u8::from(status.frame_generation), status.xbrz, u8::from(status.player1)]);
    #[allow(clippy::cast_possible_truncation)]
    out.push(speed.len() as u8);
    out.extend(speed.as_bytes());
    out.extend(clipped(&status.title, MAX_STATUS_TITLE).as_bytes());
    out
}

pub(super) fn decode_status(payload: &[u8]) -> io::Result<HostStatus> {
    let bad = || invalid("bad status");
    let (head, rest) = payload.split_at_checked(21).ok_or_else(bad)?;
    let &[
        state,
        c0,
        c1,
        c2,
        c3,
        f0,
        f1,
        f2,
        f3,
        i0,
        i1,
        i2,
        i3,
        i4,
        i5,
        i6,
        i7,
        rate,
        generation,
        xbrz,
        player1,
    ] = head
    else {
        return Err(bad());
    };
    let (&speed_length, rest) = rest.split_first().ok_or_else(bad)?;
    let (speed, title) = rest.split_at_checked(speed_length as usize).ok_or_else(bad)?;
    let text = |bytes: &[u8]| String::from_utf8(bytes.to_vec()).map_err(|_| bad());
    let fps = f32::from_be_bytes([f0, f1, f2, f3]);
    Ok(HostStatus {
        state: match state {
            0 => HostState::Idle,
            1 => HostState::Running,
            2 => HostState::Paused,
            3 => HostState::Exited(i32::from_be_bytes([c0, c1, c2, c3])),
            4 => HostState::Crashed,
            _ => return Err(bad()),
        },
        fps: if fps.is_finite() { fps } else { 0.0 },
        rate: match rate {
            0 => FrameRate::Title,
            1 => FrameRate::Max30,
            2 => FrameRate::Force60,
            _ => return Err(bad()),
        },
        speed: text(speed)?,
        frame_generation: generation != 0,
        xbrz,
        title: text(title)?,
        instructions: u64::from_be_bytes([i0, i1, i2, i3, i4, i5, i6, i7]),
        player1: player1 != 0,
    })
}

/// Writes one message.
///
/// # Errors
/// Fails when the connection does.
pub fn write_message(out: &mut impl Write, message: &Message) -> io::Result<()> {
    let (tag, payload) = match message {
        Message::Hello => (tag::HELLO, VERSION.to_be_bytes().to_vec()),
        Message::Ping(value) => (tag::PING, value.to_be_bytes().to_vec()),
        Message::Pong(value) => (tag::PONG, value.to_be_bytes().to_vec()),
        Message::Input(input) => (tag::INPUT, encode_input(input)),
        Message::Video(packet) => {
            let mut payload = Vec::with_capacity(VideoPacket::HEADER + packet.data.len());
            payload.extend([packet.display as u8, packet.codec as u8, u8::from(packet.keyframe)]);
            payload.extend(packet.width.to_be_bytes());
            payload.extend(packet.height.to_be_bytes());
            payload.extend(packet.taken.to_be_bytes());
            payload.extend(&packet.data);
            (tag::VIDEO, payload)
        }
        Message::Codecs(bits) => (tag::CODECS, vec![*bits]),
        Message::Keyframe(display) => (tag::KEYFRAME, vec![*display as u8]),
        Message::Audio(samples) => {
            (tag::AUDIO, samples.iter().flat_map(|sample| sample.to_be_bytes()).collect())
        }
        Message::Challenge(challenge) => (tag::CHALLENGE, challenge.to_vec()),
        Message::Answer(answer) => (tag::ANSWER, answer.to_vec()),
        Message::Welcome => (tag::WELCOME, Vec::new()),
        Message::Refused(Refusal::Network) => (tag::REFUSED, vec![0]),
        Message::Refused(Refusal::WrongPassword) => (tag::REFUSED, vec![1]),
        Message::Status(status) => (tag::STATUS, encode_status(status)),
        Message::FrameShown(display) => (tag::FRAME_SHOWN, vec![*display as u8]),
        Message::ReturnPlayer1 => (tag::RETURN_PLAYER1, Vec::new()),
    };
    let length = u32::try_from(payload.len()).map_err(|_| invalid("message too long"))?;
    let mut packet = Vec::with_capacity(5 + payload.len());
    packet.push(tag);
    packet.extend(length.to_be_bytes());
    packet.extend(payload);
    // In one piece: with `TCP_NODELAY` a header on its own would be a packet of its own.
    out.write_all(&packet)
}

fn decode_video(mut payload: Vec<u8>) -> io::Result<VideoPacket> {
    if payload.len() < VideoPacket::HEADER {
        return Err(invalid("bad picture message"));
    }
    let display = match payload[0] {
        0 => Display::Tv,
        1 => Display::GamePad,
        _ => return Err(invalid("bad picture message")),
    };
    let codec = Codec::from_u8(payload[1]).ok_or_else(|| invalid("unknown codec"))?;
    let width = u16::from_be_bytes([payload[3], payload[4]]);
    let height = u16::from_be_bytes([payload[5], payload[6]]);
    if u32::from(width) > MAX_FRAME_SIDE || u32::from(height) > MAX_FRAME_SIDE {
        return Err(invalid("picture too large"));
    }
    let mut taken = [0; 8];
    taken.copy_from_slice(&payload[7..15]);
    let keyframe = payload[2] != 0;
    let data = payload.split_off(VideoPacket::HEADER);
    Ok(VideoPacket {
        display,
        codec,
        keyframe,
        width,
        height,
        taken: u64::from_be_bytes(taken),
        data,
    })
}

/// Reads one message.
///
/// # Errors
/// Fails when the connection does or the bytes make no sense.
pub fn read_message(input: &mut impl Read) -> io::Result<Message> {
    let mut header = [0; 5];
    input.read_exact(&mut header)?;
    let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if length > MAX_PAYLOAD {
        return Err(invalid("message too long"));
    }
    let mut payload = vec![0; length];
    input.read_exact(&mut payload)?;
    let value = || -> io::Result<u64> {
        let bytes: [u8; 8] =
            payload.as_slice().try_into().map_err(|_| invalid("bad ping message"))?;
        Ok(u64::from_be_bytes(bytes))
    };
    match header[0] {
        tag::HELLO => {
            if payload != VERSION.to_be_bytes() {
                return Err(invalid("the other side runs a different shocora version"));
            }
            Ok(Message::Hello)
        }
        tag::PING => Ok(Message::Ping(value()?)),
        tag::PONG => Ok(Message::Pong(value()?)),
        tag::INPUT => decode_input(&payload).map(Message::Input),
        tag::VIDEO => decode_video(payload).map(Message::Video),
        tag::CODECS => match payload.as_slice() {
            [bits] => Ok(Message::Codecs(*bits)),
            _ => Err(invalid("bad codec list")),
        },
        tag::KEYFRAME => match payload.as_slice() {
            [0] => Ok(Message::Keyframe(Display::Tv)),
            [1] => Ok(Message::Keyframe(Display::GamePad)),
            _ => Err(invalid("bad keyframe request")),
        },
        tag::AUDIO => Ok(Message::Audio(
            payload.as_chunks::<2>().0.iter().map(|bytes| i16::from_be_bytes(*bytes)).collect(),
        )),
        tag::CHALLENGE => payload
            .as_slice()
            .try_into()
            .map(Message::Challenge)
            .map_err(|_| invalid("bad challenge")),
        tag::ANSWER => {
            payload.as_slice().try_into().map(Message::Answer).map_err(|_| invalid("bad answer"))
        }
        tag::WELCOME => Ok(Message::Welcome),
        tag::STATUS => decode_status(&payload).map(Message::Status),
        tag::FRAME_SHOWN => match payload.as_slice() {
            [0] => Ok(Message::FrameShown(Display::Tv)),
            [1] => Ok(Message::FrameShown(Display::GamePad)),
            _ => Err(invalid("bad picture acknowledgement")),
        },
        tag::RETURN_PLAYER1 => Ok(Message::ReturnPlayer1),
        tag::REFUSED => match payload.as_slice() {
            [0] => Ok(Message::Refused(Refusal::Network)),
            [1] => Ok(Message::Refused(Refusal::WrongPassword)),
            _ => Err(invalid("bad refusal")),
        },
        _ => Err(invalid("unknown message")),
    }
}
