//! Code patches of graphic packs in the running title (Cemu's `ApplyPatchesForModule` and
//! `UndoPatchGroups`).
//!
//! The patches are applied to the main executable right after it is loaded, before its
//! first instruction runs. Changing the enabled packs while it runs undoes every patch,
//! restoring the original bytes, and applies those of the packs now enabled: mods take
//! effect at once, as in Cemu.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲     ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ enabled packs      │   │ emulator/patches.rs      │   │ changed code       │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ pack patches at run time │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Applies (and removes) graphic pack code patches in the running game. Terms such as HLE, JIT
//! or shader are explained in `docs/guide/wiiu_term.md`.
use std::collections::HashMap;

use wiiu_cafe_os::{CafeOs, hle::callback::GuestCall};
use wiiu_graphic_pack::{
    AppliedPack,
    patch::{ModuleInfo, PatchHost, apply},
};
use wiiu_mem_addr::CODE_CAVE;
use wiiu_rpl::LoadedModule;

use super::Emulator;

/// Patches in place: the module they went into and the bytes they replaced.
#[derive(Debug, Default)]
pub(super) struct PatchState {
    pub module: Option<ModuleInfo>,
    /// Address and original bytes of every write, in the order written.
    backups: Vec<(u32, Vec<u8>)>,
}

impl PatchState {
    /// Nothing patched yet in `module`, just loaded.
    pub fn for_module(module: &LoadedModule) -> Self {
        Self {
            module: Some(ModuleInfo {
                crc: module.patch_crc,
                is_rpx: module.is_rpx,
                sections: module.sections.clone(),
            }),
            backups: Vec::new(),
        }
    }
}

/// Code caves from Cemu's area, and imports as HLE call stubs.
///
/// The stubs lie out of reach of a relative branch from the code, so each import is a
/// trampoline at the top of the code cave area that jumps to its stub through `ctr`.
struct Host<'a> {
    os: &'a mut CafeOs,
    next_cave: u32,
    /// Start of the trampolines, which grow down from the end of the area.
    trampolines_start: u32,
    trampolines: HashMap<u32, u32>,
    /// Trampolines to write.
    writes: Vec<(u32, Vec<u8>)>,
}

/// `lis r12, stub@h; ori r12, r12, stub@l; mtctr r12; bctr`: `r12` is free at a call.
fn trampoline(stub: u32) -> Vec<u8> {
    [0x3D80_0000 | (stub >> 16), 0x618C_0000 | (stub & 0xFFFF), 0x7D89_03A6, 0x4E80_0420]
        .iter()
        .flat_map(|word| word.to_be_bytes())
        .collect()
}

impl PatchHost for Host<'_> {
    fn alloc_code_cave(&mut self, size: u32) -> Option<u32> {
        let start = self.next_cave.next_multiple_of(256);
        let end = start.checked_add(size)?;
        if end > self.trampolines_start {
            return None;
        }
        self.next_cave = end;
        Some(start)
    }

    fn import(&mut self, module: &str, function: &str) -> Option<u32> {
        let stub = self.os.registry.function_stub(module, function);
        if let Some(&at) = self.trampolines.get(&stub) {
            return Some(at);
        }
        let at = self.trampolines_start.checked_sub(16).filter(|&at| at >= self.next_cave)?;
        self.trampolines_start = at;
        self.trampolines.insert(stub, at);
        self.writes.push((at, trampoline(stub)));
        Some(at)
    }
}

impl Emulator {
    /// Puts back the bytes the patches replaced.
    pub(super) fn undo_patches(&mut self) {
        for (address, original) in std::mem::take(&mut self.patches.backups).into_iter().rev() {
            if let Err(error) = self.memory.write_bytes(address, &original) {
                log::warn!("graphic pack patch at {address:#010x} not undone: {error}");
            }
            #[allow(clippy::cast_possible_truncation)]
            self.jit.invalidate(address, original.len() as u32);
        }
    }

    /// Applies the patches of `packs` to the main executable, after undoing those in
    /// place. At boot, `.callback entry` functions are queued to run before the title's
    /// entry point. Each pack is named in `notices` with the outcome.
    pub(super) fn apply_patches(
        &mut self,
        packs: &[AppliedPack],
        notices: &mut Vec<String>,
        at_boot: bool,
    ) {
        self.undo_patches();
        let Some(module) = self.patches.module.clone() else { return };
        #[allow(clippy::cast_possible_truncation)]
        let mut host = Host {
            os: &mut self.os,
            next_cave: CODE_CAVE.start,
            trampolines_start: CODE_CAVE.end() as u32,
            trampolines: HashMap::new(),
            writes: Vec::new(),
        };
        let mut callbacks = Vec::new();
        let mut writes = Vec::new();
        for pack in packs {
            if let Some(error) = &pack.patch_error {
                notices.push(format!("graphic pack {}: patches not applied: {error}", pack.name));
                continue;
            }
            if pack.patches.is_empty() {
                continue;
            }
            match apply(&pack.patches, &module, &pack.constants, &mut host) {
                Ok(applied) if applied.groups.is_empty() => notices.push(format!(
                    "graphic pack {}: no patch group fits this version ({:#010X})",
                    pack.name, module.crc
                )),
                Ok(applied) => {
                    let names: Vec<&str> = applied.groups.iter().map(|(n, _)| n.as_str()).collect();
                    notices.push(format!(
                        "graphic pack {}: patches applied ({})",
                        pack.name,
                        names.join(", ")
                    ));
                    writes.extend(applied.writes);
                    callbacks.extend(applied.entry_callbacks);
                }
                Err(errors) => notices.push(format!(
                    "graphic pack {}: patches not applied: {}",
                    pack.name,
                    errors.join("; ")
                )),
            }
        }
        writes.append(&mut host.writes);
        for (address, bytes) in writes {
            let mut original = vec![0; bytes.len()];
            let written = self
                .memory
                .read_bytes(address, &mut original)
                .and_then(|()| self.memory.write_bytes(address, &bytes));
            match written {
                Ok(()) => self.patches.backups.push((address, original)),
                Err(error) => log::warn!("graphic pack patch at {address:#010x}: {error}"),
            }
            #[allow(clippy::cast_possible_truncation)]
            self.jit.invalidate(address, bytes.len() as u32);
        }
        // Imports may have created call stubs.
        if let Err(error) = self.os.registry.commit(&mut self.memory) {
            log::warn!("graphic pack imports: {error}");
        }
        if at_boot {
            for function in callbacks {
                self.os.callbacks.push(GuestCall { function, args: Vec::new() });
            }
        }
    }
}
