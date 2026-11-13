//! The guest's side: keeps trying to reach the host, shows what it sends, sends back the
//! controller.
use std::{
    io,
    net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use crate::{
    DEFAULT_PORT, LinkState, PEER_TIMEOUT, PING_INTERVAL,
    protocol::{self, Frame, GuestInput, HostStatus, Message, Refusal},
};

/// What keeps a guest out.
#[derive(Debug)]
enum JoinError {
    /// The host said no.
    Refused(Refusal),
    Link(io::Error),
}

impl From<io::Error> for JoinError {
    fn from(error: io::Error) -> Self {
        Self::Link(error)
    }
}

/// How long one attempt to reach the host may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the guest waits between attempts to reach the host.
const RETRY_DELAY: Duration = Duration::from_secs(2);
/// Stopping is noticed at least this often while waiting.
const STOP_POLL: Duration = Duration::from_millis(50);

/// What the guest does with what arrives. Lives on the link's thread.
pub trait ClientEvents {
    fn frame(&mut self, frame: Frame);
    /// Interleaved stereo at [`protocol::AUDIO_RATE`].
    fn audio(&mut self, samples: &[i16]);
    /// The host's state, about once a second.
    fn status(&mut self, status: HostStatus);
    /// The link changed; a good moment to draw again.
    fn changed(&mut self);
}

#[derive(Default)]
struct Shared {
    writer: Mutex<Option<TcpStream>>,
    /// Sent again after reconnecting.
    input: Mutex<GuestInput>,
    stop: AtomicBool,
}

impl Shared {
    fn send(&self, message: &Message) -> io::Result<()> {
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        match writer.as_mut() {
            Some(stream) => protocol::write_message(stream, message),
            None => Ok(()),
        }
    }
}

/// A link to a host, kept up until dropped.
pub struct Client {
    shared: Arc<Shared>,
    state: Arc<Mutex<LinkState>>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

/// `host` or `host:port`, with the default port if none is given.
fn with_port(address: &str) -> String {
    let address = address.trim();
    if address.contains(':') { address.to_owned() } else { format!("{address}:{DEFAULT_PORT}") }
}

impl Client {
    /// Starts connecting to `address`; `password` may be empty. `events` is made on the
    /// link's thread, because audio outputs must stay on the thread that opened them.
    ///
    /// # Errors
    /// Fails if the thread cannot start.
    pub fn start<E: ClientEvents>(
        address: &str,
        password: &str,
        events: impl FnOnce() -> E + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let state = Arc::new(Mutex::new(LinkState::Connecting));
        let (address, password) = (with_port(address), password.to_owned());
        let thread = std::thread::Builder::new().name("shocora-net-client".into()).spawn({
            let (shared, state) = (Arc::clone(&shared), Arc::clone(&state));
            move || run(&address, &password, &mut events(), &shared, &state)
        })?;
        Ok(Self { shared, state, thread: Some(thread) })
    }

    pub fn state(&self) -> LinkState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Gives the first player's rights back to the host.
    pub fn return_player1(&self) {
        let _ = self.shared.send(&Message::ReturnPlayer1);
    }

    /// Sends the controller's state to the host.
    pub fn set_input(&self, input: GuestInput) {
        *self.shared.input.lock().unwrap_or_else(PoisonError::into_inner) = input;
        // A failure shows on the reading side, which then reconnects.
        let _ = self.shared.send(&Message::Input(input));
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        drop_connection(&self.shared);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn drop_connection(shared: &Shared) {
    let writer = shared.writer.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(stream) = writer {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

fn set_state(state: &Mutex<LinkState>, new: LinkState, events: &mut impl ClientEvents) {
    *state.lock().unwrap_or_else(PoisonError::into_inner) = new;
    events.changed();
}

/// Waits unless stopped; `true` if it is time to stop.
fn wait(shared: &Shared, time: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < time {
        if shared.stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(STOP_POLL);
    }
    shared.stop.load(Ordering::Relaxed)
}

/// The host's IPv4 address, the only kind hosts listen on: `localhost` may give IPv6 first.
fn resolve(address: &str) -> io::Result<SocketAddr> {
    address
        .to_socket_addrs()?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such address"))
}

/// The next message of the greeting, unless the host refuses.
fn greeting_message(stream: &mut TcpStream) -> Result<Message, JoinError> {
    match protocol::read_message(stream)? {
        Message::Refused(reason) => Err(JoinError::Refused(reason)),
        message => Ok(message),
    }
}

fn no_greeting() -> JoinError {
    JoinError::Link(io::Error::new(io::ErrorKind::InvalidData, "no greeting"))
}

fn connect(address: &str, password: &str) -> Result<TcpStream, JoinError> {
    let mut stream = TcpStream::connect_timeout(&resolve(address)?, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(PEER_TIMEOUT))?;
    stream.set_write_timeout(Some(PEER_TIMEOUT))?;
    protocol::write_message(&mut stream, &Message::Hello)?;
    if greeting_message(&mut stream)? != Message::Hello {
        return Err(no_greeting());
    }
    let Message::Challenge(challenge) = greeting_message(&mut stream)? else {
        return Err(no_greeting());
    };
    let answer = protocol::answer(&challenge, password);
    protocol::write_message(&mut stream, &Message::Answer(answer))?;
    match greeting_message(&mut stream)? {
        Message::Welcome => Ok(stream),
        _ => Err(no_greeting()),
    }
}

/// Connects, serves the connection until it ends, and tries again. Every attempt doubles as
/// the check whether the host can be reached at all.
fn run(
    address: &str,
    password: &str,
    events: &mut impl ClientEvents,
    shared: &Arc<Shared>,
    state: &Mutex<LinkState>,
) {
    while !shared.stop.load(Ordering::Relaxed) {
        let stream = match connect(address, password) {
            Ok(stream) => stream,
            Err(JoinError::Refused(reason)) => {
                log::warn!("netplay: the host refused: {reason:?}");
                set_state(state, LinkState::Refused(reason), events);
                return;
            }
            Err(JoinError::Link(error)) => {
                set_state(state, LinkState::Unreachable(error.to_string()), events);
                if wait(shared, RETRY_DELAY) {
                    return;
                }
                continue;
            }
        };
        let peer = address.to_owned();
        set_state(state, LinkState::Connected { peer: peer.clone(), round_trip: None }, events);
        if let Err(error) = serve(stream, &peer, events, shared, state) {
            if !shared.stop.load(Ordering::Relaxed) {
                log::info!("netplay: lost the host: {error}");
                set_state(state, LinkState::Unreachable(error.to_string()), events);
            }
        }
        drop_connection(shared);
        if wait(shared, RETRY_DELAY) {
            return;
        }
    }
}

fn serve(
    mut reading: TcpStream,
    peer: &str,
    events: &mut impl ClientEvents,
    shared: &Arc<Shared>,
    state: &Mutex<LinkState>,
) -> io::Result<()> {
    *shared.writer.lock().unwrap_or_else(PoisonError::into_inner) = Some(reading.try_clone()?);
    let input = *shared.input.lock().unwrap_or_else(PoisonError::into_inner);
    shared.send(&Message::Input(input))?;
    let started = Instant::now();
    let ended = Arc::new(AtomicBool::new(false));
    let pinger = std::thread::Builder::new().name("shocora-net-ping".into()).spawn({
        let (shared, ended) = (Arc::clone(shared), Arc::clone(&ended));
        move || {
            while !ended.load(Ordering::Relaxed) {
                if shared.send(&Message::Ping(protocol::nanos(started))).is_err() {
                    return;
                }
                let waited = Instant::now();
                while waited.elapsed() < PING_INTERVAL {
                    if ended.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(STOP_POLL);
                }
            }
        }
    })?;
    let result = receive(&mut reading, peer, events, shared, state, started);
    ended.store(true, Ordering::Relaxed);
    let _ = pinger.join();
    result
}

/// Handles what the host sends. The read timeout turns silence into an error.
fn receive(
    reading: &mut TcpStream,
    peer: &str,
    events: &mut impl ClientEvents,
    shared: &Shared,
    state: &Mutex<LinkState>,
    started: Instant,
) -> io::Result<()> {
    loop {
        match protocol::read_message(reading)? {
            Message::Status(status) => events.status(status),
            Message::Video(display, jpeg) => {
                match protocol::decode_frame(display, &jpeg) {
                    Ok(frame) => events.frame(frame),
                    Err(error) => log::warn!("netplay: bad picture: {error}"),
                }
                // The host sends the next picture of this screen only now: pictures never
                // pile up on the way, so what shows is close to what the host has.
                shared.send(&Message::FrameShown(display))?;
            }
            Message::Audio(samples) => events.audio(&samples),
            Message::Ping(value) => shared.send(&Message::Pong(value))?,
            Message::Pong(sent) => {
                let round_trip =
                    Duration::from_nanos(protocol::nanos(started).saturating_sub(sent));
                let connected =
                    LinkState::Connected { peer: peer.to_owned(), round_trip: Some(round_trip) };
                set_state(state, connected, events);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
