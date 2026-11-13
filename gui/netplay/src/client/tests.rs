use std::sync::mpsc;

use wiiu_peripherals::{GamePadState, controller::VpadButtons};

use super::*;
use crate::{
    host::{Host, HostConfig, HostEvents},
    protocol::{Display, HostState},
};

const LOOPBACK: &str = "127.0.0.1";

/// A host's side of the greeting, for tests playing the host themselves.
fn greet(stream: &mut std::net::TcpStream) {
    protocol::write_message(stream, &Message::Hello).unwrap();
    assert_eq!(protocol::read_message(stream).unwrap(), Message::Hello);
    protocol::write_message(stream, &Message::Challenge([3; 16])).unwrap();
    assert!(matches!(protocol::read_message(stream).unwrap(), Message::Answer(_)));
    protocol::write_message(stream, &Message::Welcome).unwrap();
}

fn guest_events() -> impl FnOnce() -> RecordingGuest + Send + 'static {
    let (frames, _f) = mpsc::channel();
    let (audio, _a) = mpsc::channel();
    let (statuses, _s) = mpsc::channel();
    move || RecordingGuest { frames, audio, statuses }
}
const PATIENCE: Duration = Duration::from_secs(10);

/// Reports everything the host's side sees.
struct RecordingHost {
    inputs: Mutex<mpsc::Sender<Option<GuestInput>>>,
    wanted: Mutex<mpsc::Sender<bool>>,
    streaming: AtomicBool,
}

impl HostEvents for RecordingHost {
    fn input(&self, input: Option<GuestInput>) {
        let _ = self.inputs.lock().unwrap().send(input);
    }

    fn streaming(&self, wanted: bool) {
        self.streaming.store(wanted, Ordering::Relaxed);
        let _ = self.wanted.lock().unwrap().send(wanted);
    }

    fn take_frames(&self) -> Vec<Frame> {
        if !self.streaming.swap(false, Ordering::Relaxed) {
            return Vec::new();
        }
        [(Display::Tv, 16), (Display::GamePad, 8)]
            .map(|(display, width)| Frame {
                display,
                width,
                height: 8,
                rgba: [10, 200, 30, 255].repeat(width as usize * 8),
            })
            .to_vec()
    }

    fn status(&self) -> HostStatus {
        HostStatus { state: HostState::Running, fps: 30.0, ..HostStatus::default() }
    }
}

struct RecordingGuest {
    frames: mpsc::Sender<(Display, u32, u32)>,
    audio: mpsc::Sender<Vec<i16>>,
    statuses: mpsc::Sender<HostStatus>,
}

impl ClientEvents for RecordingGuest {
    fn frame(&mut self, frame: Frame) {
        let _ = self.frames.send((frame.display, frame.width, frame.height));
    }

    fn audio(&mut self, samples: &[i16]) {
        let _ = self.audio.send(samples.to_vec());
    }

    fn status(&mut self, status: HostStatus) {
        let _ = self.statuses.send(status);
    }

    fn changed(&mut self) {}
}

struct Rig {
    host: Host,
    inputs: mpsc::Receiver<Option<GuestInput>>,
    wanted: mpsc::Receiver<bool>,
}

fn start_host() -> Rig {
    start_host_with("")
}

fn start_host_with(password: &str) -> Rig {
    let (inputs_tx, inputs) = mpsc::channel();
    let (wanted_tx, wanted) = mpsc::channel();
    let events = RecordingHost {
        inputs: Mutex::new(inputs_tx),
        wanted: Mutex::new(wanted_tx),
        streaming: AtomicBool::new(false),
    };
    let config = HostConfig::new("127.0.0.1/8", 0, password).unwrap();
    Rig { host: Host::start(config, events).unwrap(), inputs, wanted }
}

fn eventually(what: &str, condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < PATIENCE, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn guest_plays_along_and_leaves() {
    let rig = start_host();
    let (frames_tx, frames) = mpsc::channel();
    let (audio_tx, audio) = mpsc::channel();
    let (statuses_tx, statuses) = mpsc::channel();
    let address = format!("{LOOPBACK}:{}", rig.host.port());
    let client = Client::start(&address, "", move || RecordingGuest {
        frames: frames_tx,
        audio: audio_tx,
        statuses: statuses_tx,
    })
    .unwrap();

    eventually("the link", || matches!(client.state(), LinkState::Connected { .. }));
    assert_eq!(rig.wanted.recv_timeout(PATIENCE), Ok(true));
    assert_eq!(frames.recv_timeout(PATIENCE), Ok((Display::Tv, 16, 8)));
    assert_eq!(frames.recv_timeout(PATIENCE), Ok((Display::GamePad, 8, 8)));
    let status = statuses.recv_timeout(PATIENCE).unwrap();
    assert_eq!((status.state, status.fps), (HostState::Running, 30.0));

    // A held button and a touch reach the host (the first input is the idle one sent on
    // joining).
    let pad = GamePadState {
        touch: Some((100, 200)),
        ..GamePadState::from_button_bits(VpadButtons::A.bits())
    };
    client.set_input(GuestInput { pad, to_gamepad: true });
    loop {
        let input = rig.inputs.recv_timeout(PATIENCE).unwrap().unwrap();
        if input.pad.held == VpadButtons::A {
            assert_eq!(input.pad.touch, Some((100, 200)));
            assert!(input.to_gamepad);
            break;
        }
    }

    // 48 kHz in, 16 kHz out, in 20 ms pieces.
    rig.host.audio_sink().send(vec![1000; 2 * 960]).unwrap();
    let samples = audio.recv_timeout(PATIENCE).unwrap();
    assert_eq!(samples.len(), 640);
    assert!(samples.iter().all(|sample| *sample == 1000));

    // The pings measure the round trip on both sides.
    let measured =
        |state: LinkState| matches!(state, LinkState::Connected { round_trip: Some(_), .. });
    eventually("the guest's round trip", || measured(client.state()));
    eventually("the host's round trip", || measured(rig.host.state()));

    drop(client);
    eventually("the host noticing", || rig.host.state() == LinkState::Waiting);
    // The controller gets unplugged.
    loop {
        if rig.inputs.recv_timeout(PATIENCE).unwrap().is_none() {
            break;
        }
    }
}

#[test]
fn unreachable_hosts_are_reported_and_tried_again() {
    // Nobody listens on a port that was just free.
    let free = std::net::TcpListener::bind((LOOPBACK, 0)).unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let (frames, _f) = mpsc::channel();
    let (audio, _a) = mpsc::channel();
    let (statuses, _s) = mpsc::channel();
    let client = Client::start(&format!("{LOOPBACK}:{port}"), "", move || RecordingGuest {
        frames,
        audio,
        statuses,
    })
    .unwrap();
    eventually("the report", || matches!(client.state(), LinkState::Unreachable(_)));

    // The host appears: the guest gets in by itself.
    let listener = std::net::TcpListener::bind((LOOPBACK, port)).unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    greet(&mut stream);
    eventually("the link", || matches!(client.state(), LinkState::Connected { .. }));
}

#[test]
fn a_silent_host_counts_as_gone() {
    let listener = std::net::TcpListener::bind((LOOPBACK, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (frames, _f) = mpsc::channel();
    let (audio, _a) = mpsc::channel();
    let (statuses, _s) = mpsc::channel();
    let client = Client::start(&format!("{LOOPBACK}:{port}"), "", move || RecordingGuest {
        frames,
        audio,
        statuses,
    })
    .unwrap();
    // Answers the greeting, then never says anything again but keeps the socket open.
    let (mut stream, _) = listener.accept().unwrap();
    greet(&mut stream);
    eventually("the link", || matches!(client.state(), LinkState::Connected { .. }));
    eventually("the timeout", || matches!(client.state(), LinkState::Unreachable(_)));
}

#[test]
fn the_seat_is_taken_while_a_guest_plays() {
    let rig = start_host();
    let address = format!("{LOOPBACK}:{}", rig.host.port());
    let guest = || {
        let (frames, _f) = mpsc::channel();
        let (audio, _a) = mpsc::channel();
        let (statuses, _s) = mpsc::channel();
        let address = address.clone();
        Client::start(&address, "", move || RecordingGuest { frames, audio, statuses }).unwrap()
    };
    let first = guest();
    eventually("the first guest", || matches!(first.state(), LinkState::Connected { .. }));
    let second = guest();
    eventually("the refusal", || matches!(second.state(), LinkState::Unreachable(_)));
    assert!(matches!(first.state(), LinkState::Connected { .. }));
}

#[test]
fn the_right_password_opens_the_door() {
    let rig = start_host_with("secret");
    let address = format!("{LOOPBACK}:{}", rig.host.port());
    let guest = Client::start(&address, "secret", guest_events()).unwrap();
    eventually("the link", || matches!(guest.state(), LinkState::Connected { .. }));
    assert!(matches!(rig.host.state(), LinkState::Connected { .. }));
}

#[test]
fn a_wrong_password_is_refused_and_not_tried_again() {
    let rig = start_host_with("secret");
    let address = format!("{LOOPBACK}:{}", rig.host.port());
    for password in ["wrong", ""] {
        let guest = Client::start(&address, password, guest_events()).unwrap();
        let refused = LinkState::Refused(Refusal::WrongPassword);
        eventually("the refusal", || guest.state() == refused);
        assert_eq!(rig.host.state(), LinkState::Waiting);
    }
}

#[test]
fn a_guest_from_another_network_is_refused() {
    // Everything on loopback is this machine, which is always let in: refuses by hand.
    let listener = std::net::TcpListener::bind((LOOPBACK, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let guest = Client::start(&format!("{LOOPBACK}:{port}"), "", guest_events()).unwrap();
    let (stream, _) = listener.accept().unwrap();
    crate::host::refuse(stream, Refusal::Network).unwrap();
    let refused = LinkState::Refused(Refusal::Network);
    eventually("the refusal", || guest.state() == refused);
}

fn silent_host_events() -> RecordingHost {
    let (inputs, _) = mpsc::channel();
    let (wanted, _) = mpsc::channel();
    RecordingHost {
        inputs: Mutex::new(inputs),
        wanted: Mutex::new(wanted),
        streaming: AtomicBool::new(false),
    }
}

/// A host on a network address lets a window on this machine in at any of its addresses.
#[test]
fn this_machine_joins_at_any_of_its_addresses() {
    // A link-local address of an adapter that is down cannot be reached by anyone.
    let reachable: Vec<String> = crate::local_addresses()
        .iter()
        .filter(|local| !local.address.is_loopback() && !local.address.is_link_local())
        .map(|local| local.address.to_string())
        .collect();
    // Machines without a network have nothing to try this on.
    let Some(first) = reachable.first() else { return };
    let config = HostConfig::new(&format!("{first}/32"), 0, "").unwrap();
    let host = Host::start(config, silent_host_events()).unwrap();
    let port = host.port();
    let mut addresses = vec![LOOPBACK.to_owned(), "localhost".to_owned()];
    addresses.extend(reachable.iter().cloned());
    for address in addresses {
        let guest = Client::start(&format!("{address}:{port}"), "", guest_events()).unwrap();
        let start = Instant::now();
        while !matches!(guest.state(), LinkState::Connected { .. }) {
            assert!(start.elapsed() < PATIENCE, "{address}: {:?}", guest.state());
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(guest);
        eventually("the seat to free up", || host.state() == LinkState::Waiting);
    }
}

/// The default: a host on a loopback address only listens there.
#[test]
fn a_loopback_host_listens_on_loopback_only() {
    let config = HostConfig::new(LOOPBACK, 0, "").unwrap();
    let host = Host::start(config, silent_host_events()).unwrap();
    let port = host.port();
    let guest = Client::start(&format!("localhost:{port}"), "", guest_events()).unwrap();
    eventually("the link", || matches!(guest.state(), LinkState::Connected { .. }));
    drop(guest);

    let others = crate::local_addresses()
        .into_iter()
        .filter(|local| !local.address.is_loopback() && !local.address.is_link_local());
    for other in others {
        let at = SocketAddr::from((other.address, port));
        assert!(TcpStream::connect_timeout(&at, CONNECT_TIMEOUT).is_err(), "{}", other.interface);
    }
}

#[test]
fn hosts_refuse_addresses_of_other_machines() {
    // From the block reserved for documentation: never a real adapter's.
    let config = HostConfig::new("192.0.2.1", 0, "").unwrap();
    assert!(Host::start(config, silent_host_events()).is_err());
}

#[test]
fn a_network_is_as_large_as_the_adapter_says() {
    for local in crate::local_addresses() {
        let config = HostConfig::new(&local.address.to_string(), 0, "").unwrap();
        assert_eq!(config.prefix, local.prefix, "{}", local.interface);
    }
}

#[test]
fn hosts_admit_their_own_network_only() {
    let ip = |text: &str| text.parse::<std::net::IpAddr>().unwrap();
    let home = HostConfig::new(" 192.168.1.10/24 ", 0, "").unwrap();
    let at = ip("192.168.1.10");
    assert_eq!(home.network(), "192.168.1.0/24");
    assert!(home.admits(ip("192.168.1.200"), at));
    assert!(home.admits(ip("::ffff:192.168.1.200"), at));
    assert!(!home.admits(ip("192.168.2.10"), at));
    assert!(!home.admits(ip("8.8.8.8"), at));
    assert!(!home.admits(ip("fe80::1"), at));

    let wide = HostConfig::new("10.1.2.3/16", 0, "").unwrap();
    let at = ip("10.1.2.3");
    assert_eq!(wide.network(), "10.1.0.0/16");
    assert!(wide.admits(ip("10.1.200.1"), at));
    assert!(!wide.admits(ip("10.2.0.1"), at));

    // Two machines on a VPN, whose network spans a whole /8.
    let vpn = HostConfig::new("26.1.2.3/8", 0, "").unwrap();
    let at = ip("26.1.2.3");
    assert_eq!(vpn.network(), "26.0.0.0/8");
    assert!(vpn.admits(ip("26.200.3.4"), at));
    assert!(!vpn.admits(ip("27.62.81.253"), at));

    // This machine, at whichever address, even one off the host's network.
    assert!(home.admits(ip("127.0.0.1"), ip("127.0.0.1")));
    assert!(home.admits(ip("::1"), ip("::1")));
    assert!(home.admits(ip("26.1.2.3"), ip("26.1.2.3")));
    // Another machine that merely reached this one at that address.
    assert!(!home.admits(ip("26.200.3.4"), ip("26.1.2.3")));

    assert!(HostConfig::new("not an address", 0, "").is_err());
    assert!(HostConfig::new("10.0.0.1/33", 0, "").is_err());
}

#[test]
fn addresses_get_the_default_port() {
    assert_eq!(with_port(" 192.168.0.2 "), format!("192.168.0.2:{DEFAULT_PORT}"));
    assert_eq!(with_port("192.168.0.2:1234"), "192.168.0.2:1234");
}

/// A host whose title draws a new picture of each screen all the time.
struct BusyHost {
    returned: Mutex<mpsc::Sender<()>>,
}

impl HostEvents for BusyHost {
    fn input(&self, _input: Option<GuestInput>) {}

    fn streaming(&self, _wanted: bool) {}

    fn take_frames(&self) -> Vec<Frame> {
        [Display::Tv, Display::GamePad]
            .map(|display| Frame { display, width: 8, height: 8, rgba: vec![0; 8 * 8 * 4] })
            .to_vec()
    }

    fn status(&self) -> HostStatus {
        HostStatus::default()
    }

    fn return_player1(&self) {
        let _ = self.returned.lock().unwrap().send(());
    }
}

/// Plays the guest by hand: greets the host and returns the connection.
fn join_by_hand(port: u16) -> std::net::TcpStream {
    let mut stream = std::net::TcpStream::connect((LOOPBACK, port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
    protocol::write_message(&mut stream, &Message::Hello).unwrap();
    assert_eq!(protocol::read_message(&mut stream).unwrap(), Message::Hello);
    let Message::Challenge(challenge) = protocol::read_message(&mut stream).unwrap() else {
        panic!("no challenge");
    };
    let answer = protocol::answer(&challenge, "");
    protocol::write_message(&mut stream, &Message::Answer(answer)).unwrap();
    assert_eq!(protocol::read_message(&mut stream).unwrap(), Message::Welcome);
    stream
}

/// The pictures of each screen that arrive before the host falls silent.
fn pictures_until_quiet(stream: &mut std::net::TcpStream) -> [usize; 2] {
    let mut counts = [0; 2];
    while let Ok(message) = protocol::read_message(stream) {
        if let Message::Video(display, _) = message {
            counts[display as usize] += 1;
        }
    }
    counts
}

#[test]
fn a_new_picture_waits_until_the_guest_has_the_last_one() {
    let (returned_tx, returned) = mpsc::channel();
    let config = HostConfig::new("127.0.0.1/8", 0, "").unwrap();
    let host = Host::start(config, BusyHost { returned: Mutex::new(returned_tx) }).unwrap();
    let mut stream = join_by_hand(host.port());

    // However fast the title draws, one picture of each screen is on its way at a time.
    assert_eq!(pictures_until_quiet(&mut stream), [1, 1]);
    // Taking in the TV's picture lets exactly the newest one of it come.
    protocol::write_message(&mut stream, &Message::FrameShown(Display::Tv)).unwrap();
    assert_eq!(pictures_until_quiet(&mut stream), [1, 0]);

    // The guest can give the first player's rights back.
    protocol::write_message(&mut stream, &Message::ReturnPlayer1).unwrap();
    assert_eq!(returned.recv_timeout(PATIENCE), Ok(()));
}
