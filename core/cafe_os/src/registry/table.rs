//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ names the game     │   │ registry/table.rs        │   │ call stubs in      │
//! │ imports            │──►│ ──────────────────────── │──►│ memory             │
//! │                    │   │ table of OS functions:   │   │                    │
//! │                    │   │ table                    │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "table" share of registry.rs (table of OS functions), moved here so that no file is
//! longer than 500 lines. It works exactly as if it were still written in that file. Every OS
//! function the game can call gets a number and a tiny stub in memory. When the game calls the
//! stub, the emulator knows which Rust function to run. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.

use super::*;

impl HleRegistry {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            functions: HashMap::new(),
            data_exports: HashMap::new(),
            stubs: HashMap::new(),
            data_addresses: HashMap::new(),
            libraries: Vec::new(),
            missing_called: Vec::new(),
            call_counts: Vec::new(),
            call_nanos: Vec::new(),
            traced: Vec::new(),
            trace: std::collections::VecDeque::new(),
            pending: Vec::new(),
            stub_cursor: HLE.start,
            data_cursor: HLE.start + STUB_AREA_SIZE,
        }
    }

    /// A hash of the names of the implemented functions, in their order.
    pub(super) fn functions_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for entry in &self.entries {
            if let Entry::Function { library, name, .. } = entry {
                (library, name).hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    pub fn save_state(&self) -> RegistryState {
        RegistryState {
            functions_hash: self.functions_hash(),
            missing: self
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Missing { library, name } => Some((library.clone(), name.clone())),
                    Entry::Function { .. } => None,
                })
                .collect(),
            functions: self.functions.iter().map(|(key, index)| (key.clone(), *index)).collect(),
            stubs: self.stubs.iter().map(|(entry, stub)| (*entry, *stub)).collect(),
            data_addresses: self
                .data_addresses
                .iter()
                .map(|(key, address)| (key.clone(), *address))
                .collect(),
            libraries: self.libraries.clone(),
            pending: self.pending.clone(),
            stub_cursor: self.stub_cursor,
            data_cursor: self.data_cursor,
        }
    }

    /// Goes on from `state`.
    ///
    /// # Errors
    /// Fails if `state` comes from a build with other HLE functions.
    pub fn restore_state(&mut self, state: RegistryState) -> Result<(), String> {
        if state.functions_hash != self.functions_hash() {
            return Err("saved by a different build of shocora".to_owned());
        }
        self.entries.retain(|entry| matches!(entry, Entry::Function { .. }));
        let implemented = self.entries.len();
        self.missing_called.truncate(implemented);
        self.call_counts.truncate(implemented);
        self.call_nanos.truncate(implemented);
        self.traced.truncate(implemented);
        for (library, name) in state.missing {
            self.push_entry(Entry::Missing { library, name });
        }
        self.functions = state.functions.into_iter().collect();
        self.stubs = state.stubs.into_iter().collect();
        self.data_addresses = state.data_addresses.into_iter().collect();
        self.libraries = state.libraries;
        self.pending = state.pending;
        self.stub_cursor = state.stub_cursor;
        self.data_cursor = state.data_cursor;
        self.trace.clear();
        Ok(())
    }

    #[allow(clippy::cast_possible_truncation)]
    pub(super) fn push_entry(&mut self, entry: Entry) -> u32 {
        let name = match &entry {
            Entry::Function { name, .. } => *name,
            Entry::Missing { name, .. } => name.as_str(),
        };
        self.traced.push(std::env::var("SHOCORA_TRACE_HLE").is_ok_and(|list| {
            list.split(',')
                .any(|n| n.strip_suffix('*').map_or(n == name, |prefix| name.starts_with(prefix)))
        }));
        self.entries.push(entry);
        self.missing_called.push(false);
        self.call_counts.push(0);
        self.call_nanos.push(0);
        self.entries.len() as u32 - 1
    }

    pub fn register_functions(&mut self, exports: &[HleExport]) {
        for export in exports {
            let index = self.push_entry(Entry::Function {
                library: export.library,
                name: export.name,
                function: export.function,
            });
            self.functions.insert(key(export.library, export.name), index);
        }
    }

    pub fn register_data(&mut self, exports: &[HleDataExport]) {
        for export in exports {
            self.data_exports.insert(key(export.library, export.name), export.init);
        }
    }

    /// The function behind entry `index`, if implemented (no allocation).
    pub fn function(&self, index: u32) -> Option<HleFn> {
        match self.entries.get(index as usize)? {
            Entry::Function { function, .. } => Some(*function),
            Entry::Missing { .. } => None,
        }
    }

    /// Whether `index` names an entry, implemented or not.
    #[inline]
    pub fn contains(&self, index: u32) -> bool {
        (index as usize) < self.entries.len()
    }

    #[inline]
    pub fn entry(&self, index: u32) -> Option<Entry> {
        self.entries.get(index as usize).cloned()
    }

    /// `SHOCORA_TRACE_HLE` lists comma-separated export names to log every call of.
    #[inline]
    pub fn is_traced(&self, index: u32) -> bool {
        self.traced.get(index as usize).copied().unwrap_or(false)
    }

    /// Printable name of entry `index`.
    pub fn name_of(&self, index: u32) -> String {
        match self.entries.get(index as usize) {
            Some(Entry::Function { library, name, .. }) => format!("{library}::{name}"),
            Some(Entry::Missing { library, name }) => format!("{library}::{name} (missing)"),
            None => format!("#{index}"),
        }
    }

    /// Counts a call of entry `index`.
    pub fn note_call(&mut self, index: u32) {
        if let Some(count) = self.call_counts.get_mut(index as usize) {
            *count += 1;
            // SHOCORA_TRACE_FIRST logs the first call of every export.
            if *count == 1 && std::env::var_os("SHOCORA_TRACE_FIRST").is_some() {
                log::info!("first call: {}", self.name_of(index));
            }
        }
    }

    /// Remembers a call for crash reports (bounded).
    pub fn record_trace(&mut self, index: u32, lr: u32, arg: u32, result: u32) {
        if self.trace.len() == TRACE_LENGTH {
            self.trace.pop_front();
        }
        self.trace.push_back((index, lr, arg, result));
    }

    /// Recent calls, oldest first, formatted for logs.
    pub fn recent_calls(&self) -> Vec<String> {
        self.trace
            .iter()
            .map(|&(index, lr, arg, result)| {
                let name = match self.entries.get(index as usize) {
                    Some(Entry::Function { library, name, .. }) => format!("{library}::{name}"),
                    Some(Entry::Missing { library, name }) => {
                        format!("{library}::{name} (missing)")
                    }
                    None => format!("#{index}"),
                };
                format!("lr {lr:#010x}  r3 {arg:#010x} -> {result:#010x}  {name}")
            })
            .collect()
    }

    /// The `limit` most-called entries as `(library::name, calls)`.
    pub fn hottest(&self, limit: usize) -> Vec<(String, u64)> {
        let mut calls: Vec<(String, u64)> = self
            .entries
            .iter()
            .zip(&self.call_counts)
            .filter(|(_, count)| **count > 0)
            .map(|(entry, count)| {
                let name = match entry {
                    Entry::Function { library, name, .. } => format!("{library}::{name}"),
                    Entry::Missing { library, name } => format!("{library}::{name} (missing)"),
                };
                (name, *count)
            })
            .collect();
        calls.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        calls.truncate(limit);
        calls
    }

    /// Adds host time spent in entry `index`.
    #[inline]
    pub fn note_time(&mut self, index: u32, nanos: u64) {
        if let Some(total) = self.call_nanos.get_mut(index as usize) {
            *total += nanos;
        }
    }

    /// The `limit` entries that took longest as `(library::name, calls, nanos)`.
    pub fn slowest(&self, limit: usize) -> Vec<(String, u64, u64)> {
        let mut list: Vec<_> = (0..self.entries.len())
            .filter(|i| self.call_nanos[*i] > 0)
            .map(|i| (self.name_of(i as u32), self.call_counts[i], self.call_nanos[i]))
            .collect();
        list.sort_by_key(|(_, _, nanos)| std::cmp::Reverse(*nanos));
        list.truncate(limit);
        list
    }

    /// Returns `true` the first time a missing entry is called.
    #[inline]
    pub fn note_missing_call(&mut self, index: u32) -> bool {
        self.missing_called
            .get_mut(index as usize)
            .is_some_and(|seen| !std::mem::replace(seen, true))
    }

    /// `(library, name)` of every implemented function export.
    pub fn function_names(&self) -> impl Iterator<Item = (&str, &str)> {
        self.functions.keys().map(|(library, name)| (library.as_str(), name.as_str()))
    }

    /// Entry index of an implemented export.
    #[inline]
    pub fn function_index(&self, library: &str, name: &str) -> Option<u32> {
        self.functions.get(&key(library, name)).copied()
    }

    /// Bump-allocates `size` bytes in the HLE data area (zeroed by the region mapping).
    #[inline]
    pub fn allocate_data(&mut self, size: u32, align: u32) -> u32 {
        let address = self.data_cursor.next_multiple_of(align.max(4));
        self.data_cursor = address + size;
        address
    }

    /// Address of the call stub for `library::name`, creating it on first use.
    pub fn function_stub(&mut self, library: &str, name: &str) -> u32 {
        let index = match self.function_index(library, name) {
            Some(index) => index,
            None => {
                let index = self.push_entry(Entry::Missing {
                    library: library.to_owned(),
                    name: name.to_owned(),
                });
                self.functions.insert(key(library, name), index);
                index
            }
        };
        if let Some(&stub) = self.stubs.get(&index) {
            return stub;
        }
        let stub = self.stub_cursor;
        self.stub_cursor += STUB_SIZE;
        self.pending.push((stub, encode_hle_call(index)));
        self.pending.push((stub + 4, INSTR_BLR));
        self.stubs.insert(index, stub);
        stub
    }

    /// Address of data export `library::name`, creating it on first use.
    ///
    /// Unknown data imports get a zeroed 4 KiB block, which keeps most
    /// globals harmless.
    pub fn data_address(&mut self, library: &str, name: &str) -> u32 {
        let k = key(library, name);
        if let Some(&address) = self.data_addresses.get(&k) {
            return address;
        }
        let init = self.data_exports.get(&k).copied();
        let address = match init {
            Some(DataInit::FunctionPointer { library, name }) => {
                let stub = self.function_stub(library, name);
                let slot = self.allocate_data(4, 4);
                self.pending.push((slot, stub));
                slot
            }
            Some(DataInit::Zeroed(size)) => self.allocate_data(size, 16),
            Some(DataInit::Words(words)) => {
                #[allow(clippy::cast_possible_truncation)]
                let slot = self.allocate_data(4 * words.len() as u32, 16);
                for (i, word) in (0_u32..).zip(words) {
                    self.pending.push((slot + 4 * i, *word));
                }
                slot
            }
            None => {
                log::warn!("unknown data import {library}::{name}");
                self.allocate_data(0x1000, 16)
            }
        };
        self.data_addresses.insert(k, address);
        address
    }

    /// Opaque handle for `OSDynLoad_Acquire`.
    #[allow(clippy::cast_possible_truncation)]
    pub fn library_handle(&mut self, library: &str) -> u32 {
        let name = key(library, "").0;
        let index = self.libraries.iter().position(|l| *l == name).unwrap_or_else(|| {
            self.libraries.push(name);
            self.libraries.len() - 1
        });
        0x4C49_0000 | index as u32
    }

    /// Library name for a handle from [`Self::library_handle`].
    pub fn library_name(&self, handle: u32) -> Option<&str> {
        self.libraries.get((handle & 0xFFFF) as usize).map(String::as_str)
    }

    /// Writes queued stubs and data initialisers into guest memory.
    ///
    /// # Errors
    /// Fails if the HLE region is not mapped.
    pub fn commit(&mut self, memory: &mut Memory) -> Result<(), MemoryError> {
        for (address, word) in self.pending.drain(..) {
            memory.write_u32(address, word)?;
        }
        Ok(())
    }
}

impl ImportResolver for HleRegistry {
    fn resolve(&mut self, library: &str, symbol: &str, kind: ImportKind) -> u32 {
        match kind {
            ImportKind::Function => self.function_stub(library, symbol),
            ImportKind::Data => self.data_address(library, symbol),
        }
    }
}
