//! HLE export tables, call stubs and import resolution.
//!
//! Every function that the guest can reach gets an *entry index* and an
//! 8-byte stub in the HLE region: `hle_call index; blr`. Data imports get a
//! slot in the HLE data area. Stub writes are queued and flushed with
//! [`HleRegistry::commit`] because import resolution happens while the
//! loader holds guest memory.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ names the game     │   │ registry.rs              │   │ call stubs in      │
//! │ imports            │──►│ ──────────────────────── │──►│ memory             │
//! │                    │   │ table of OS functions    │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Every OS function the game can call gets a number and a tiny stub in memory. When the game
//! calls the stub, the emulator knows which Rust function to run. Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.
use std::collections::HashMap;

use wiiu_cpu::{INSTR_BLR, encode_hle_call};
use wiiu_mem_addr::HLE;
use wiiu_memory::{Memory, MemoryError};
use wiiu_rpl::{ImportKind, ImportResolver};

use crate::hle::{HleExport, HleFn};

/// First half of the HLE region holds call stubs, the second half data.
const STUB_AREA_SIZE: u32 = 0x10_0000;
const STUB_SIZE: u32 = 8;
/// Calls kept by the crash trace.
const TRACE_LENGTH: usize = 48;

/// How a data export is initialised.
#[derive(Debug, Clone, Copy)]
pub enum DataInit {
    /// A pointer to the stub of `library::name` (e.g. `MEMAllocFromDefaultHeap`).
    FunctionPointer { library: &'static str, name: &'static str },
    /// `size` zero bytes.
    Zeroed(u32),
    /// Initial big-endian words.
    Words(&'static [u32]),
}

/// A data object exported by an HLE library.
#[derive(Debug, Clone, Copy)]
pub struct HleDataExport {
    pub library: &'static str,
    pub name: &'static str,
    pub init: DataInit,
}

/// One callable entry.
#[derive(Debug, Clone)]
pub enum Entry {
    Function {
        library: &'static str,
        name: &'static str,
        function: HleFn,
    },
    /// Imported by the guest but not implemented by shocora.
    Missing {
        library: String,
        name: String,
    },
}

type Key = (String, String);

fn key(library: &str, name: &str) -> Key {
    let library = library.trim_end_matches(".rpl").to_ascii_lowercase();
    // Older titles import the same audio API under the library's first name.
    let library = if library == "snd_core" { "sndcore2".to_owned() } else { library };
    (library, name.to_owned())
}

/// The registry of every HLE function and data export.
#[derive(Debug)]
pub struct HleRegistry {
    entries: Vec<Entry>,
    functions: HashMap<Key, u32>,
    data_exports: HashMap<Key, DataInit>,
    stubs: HashMap<u32, u32>,
    data_addresses: HashMap<Key, u32>,
    libraries: Vec<String>,
    missing_called: Vec<bool>,
    /// Calls per entry, for profiling.
    call_counts: Vec<u64>,
    /// Host time per entry (only measured when profiling).
    call_nanos: Vec<u64>,
    traced: Vec<bool>,
    /// Most recent calls: (entry, caller lr, r3 in, r3 out).
    trace: std::collections::VecDeque<(u32, u32, u32, u32)>,
    pending: Vec<(u32, u32)>,
    stub_cursor: u32,
    data_cursor: u32,
}

/// What a saved state keeps of the registry: stubs and data handed out so far. The guest's
/// code holds entry indices, so the implemented functions must stay what they were.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct RegistryState {
    /// Tells builds with other HLE functions apart.
    functions_hash: u64,
    missing: Vec<(String, String)>,
    functions: Vec<(Key, u32)>,
    stubs: Vec<(u32, u32)>,
    data_addresses: Vec<(Key, u32)>,
    libraries: Vec<String>,
    pending: Vec<(u32, u32)>,
    stub_cursor: u32,
    data_cursor: u32,
}

impl Default for HleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;

mod table;
#[allow(unused_imports)]
pub(crate) use table::*;
