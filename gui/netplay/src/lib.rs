//! A second player on the local network or a VPN.
//!
//! ```text
//!   host: runs the title                     guest: only a screen and a controller
//!   ───────────────────                      ─────────────────────────────────────
//!   TV, thinned to 20 fps ── JPEG ─────────► shown
//!   GamePad, thinned to 10 fps ── JPEG ────► shown next to it
//!   status, once a second ────────────────► status bar
//!   sound, 48 kHz → 16 kHz ── PCM ─────────► played, upsampled by the audio output
//!   Pro Controller 1 or GamePad ◄── buttons, sticks, touch  keyboard, controllers, mouse
//!   ping ◄──────────────────────────────────► ping: 3 s of silence ends the link
//! ```
//!
//! One TCP connection carries everything. A host given a loopback address can only be reached
//! from this machine. Any other address makes it listen on all of this machine's addresses, but
//! it only lets in this machine and guests from that address's network. A password is optional.
//! The password itself never travels, but nothing is encrypted: anyone on the network can see
//! the pictures, and could try to guess the password from a recorded greeting.
pub mod client;
pub mod host;
pub mod protocol;
pub mod video;

use std::{net::Ipv4Addr, time::Duration};

pub use client::{Client, ClientEvents};
pub use host::{Host, HostConfig, HostEvents};
use if_addrs::IfAddr;
pub use protocol::{
    Codec, DEFAULT_PORT, Display, Frame, FrameRate, GuestInput, HostState, HostStatus, Picture,
    Pixels, Refusal,
};

/// How often each side asks whether the other is still there.
pub const PING_INTERVAL: Duration = Duration::from_secs(1);
/// Silence after which the other side counts as gone.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(3);

/// What a link is doing, for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    /// The host waits for a guest.
    Waiting,
    /// The guest tries to reach the host.
    Connecting,
    /// The host did not answer; the guest keeps trying.
    Unreachable(String),
    /// The host said no; trying again would not help.
    Refused(Refusal),
    Connected {
        peer: String,
        /// Round trip of the last ping, once one came back.
        round_trip: Option<Duration>,
    },
}

/// An IPv4 address of this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalAddress {
    /// The adapter's name, like `Radmin VPN`.
    pub(crate) interface: String,
    pub(crate) address: Ipv4Addr,
    /// Bits of the address that make up the adapter's network.
    pub(crate) prefix: u8,
}

/// This machine's IPv4 addresses; none if the system does not tell.
pub(crate) fn local_addresses() -> Vec<LocalAddress> {
    let interfaces = if_addrs::get_if_addrs().unwrap_or_else(|error| {
        log::warn!("netplay: cannot list this machine's addresses: {error}");
        Vec::new()
    });
    interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr {
            IfAddr::V4(v4) => Some(LocalAddress {
                interface: interface.name,
                address: v4.ip,
                prefix: v4.prefixlen,
            }),
            IfAddr::V6(_) => None,
        })
        .collect()
}
