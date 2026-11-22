//! Resolving and placing patch groups (Cemu's `GraphicPack2PatchesApply.cpp`).
//!
//! Every entry of the groups that fit a module is resolved in passes: variables and
//! labels register their values, instructions get their final address and fill in their
//! relocations. Entries that refer to something not yet known wait for the next pass, so
//! references may point forward, backward and across groups. When nothing more resolves,
//! what is left is reported and nothing is applied.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ patch groups       │   │ patch/apply.rs           │   │ code written to    │
//! │                    │──►│ ──────────────────────── │──►│ memory             │
//! │                    │   │ placing patches          │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Works out where each patch goes and writes it into the game's code. Terms such as HLE, JIT
//! or shader are explained in `docs/guide/wiiu_term.md`.
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap},
};

use super::{
    CODECAVE_MAX_OFFSET, Entry, PatchGroup,
    asm::{RelocKind, to_u32},
};
use crate::expr::Expr;

/// The module patches are applied to.
#[derive(Debug, Clone, Default)]
pub struct ModuleInfo {
    /// Cemu's `patchCRC` of the module file.
    pub crc: u32,
    pub is_rpx: bool,
    /// Placed sections: link-time address, size, address in memory.
    pub sections: Vec<(u32, u32, u32)>,
}

/// What the emulator provides while applying.
pub trait PatchHost {
    /// Memory for a group's code cave, 256-byte aligned; `None` when it ran out.
    fn alloc_code_cave(&mut self, size: u32) -> Option<u32>;
    /// Address of `function` exported by `module` (`import.module.function`).
    fn import(&mut self, module: &str, function: &str) -> Option<u32>;
}

/// The result of applying a pack's groups to a module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppliedGroups {
    /// Bytes to write, in order.
    pub writes: Vec<(u32, Vec<u8>)>,
    /// Functions to call before the title's entry point.
    pub entry_callbacks: Vec<u32>,
    /// Names of the applied groups with their code caves.
    pub groups: Vec<(String, Option<(u32, u32)>)>,
}

fn high_adjusted(value: u32) -> u32 {
    ((value >> 16) + u32::from(value & 0x8000 != 0)) & 0xFFFF
}

struct Resolver<'g, 'h> {
    module: &'g ModuleInfo,
    constants: &'g HashMap<String, f64>,
    host: RefCell<&'h mut dyn PatchHost>,
    values: HashMap<String, u32>,
    /// Code cave of each group, by group index.
    caves: Vec<u32>,
    /// While evaluating: names not known, for the report.
    unknown: RefCell<Vec<String>>,
    errors: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Resolved,
    /// Refers to something not known yet: try again next pass.
    Unknown,
    /// Reported in `errors`.
    Failed,
}

impl Resolver<'_, '_> {
    /// Cemu's `_relocateAddress`: a code cave offset or a link-time module address.
    fn relocate(&self, group: usize, address: u32) -> Option<u32> {
        if address <= CODECAVE_MAX_OFFSET {
            return Some(self.caves[group].wrapping_add(address));
        }
        self.module
            .sections
            .iter()
            .find(|&&(link, size, _)| address >= link && address - link < size)
            .map(|&(link, _, loaded)| address - link + loaded)
    }

    /// Evaluates `text`; `None` when it names something unknown or does not parse.
    fn eval(&self, group: usize, text: &str) -> Option<f64> {
        let mut expr = Expr::new();
        for (name, value) in self.constants {
            expr.add_constant(name, *value);
        }
        expr.set_constant_fallback(|name| {
            let value = if name.starts_with('$') {
                None
            } else if name.len() >= 7 && name[..7].eq_ignore_ascii_case("import.") {
                let rest = &name[7..];
                match rest.split_once('.') {
                    Some((module, function)) => {
                        self.host.borrow_mut().import(module, function).map(f64::from)
                    }
                    None => None,
                }
            } else {
                self.values.get(name).map(|&v| f64::from(v))
            };
            if value.is_none() {
                self.unknown.borrow_mut().push(name.to_owned());
            }
            value
        });
        expr.set_function(|name, input| {
            let value = to_u32(input);
            Some(f64::from(match name.to_ascii_lowercase().as_str() {
                "ha" | "ha16" => high_adjusted(value),
                "hi" | "hi16" => (value >> 16) & 0xFFFF,
                "lo" | "lo16" => value & 0xFFFF,
                "reloc" => match self.relocate(group, value) {
                    Some(address) => address,
                    None => {
                        self.unknown.borrow_mut().push(format!(
                            "reloc({value:#010x}): Address does not point to a known memory region"
                        ));
                        return None;
                    }
                },
                _ => {
                    self.unknown.borrow_mut().push(format!("{name}() (unknown function)"));
                    return None;
                }
            }))
        });
        expr.eval(text).ok()
    }

    fn register(&mut self, group: &str, line: u32, name: &str, value: u32) -> Outcome {
        if self.values.contains_key(name) {
            self.errors.push(format!("[{group}, Line {line}] Symbol {name} is already defined"));
            return Outcome::Failed;
        }
        log::debug!("graphic pack patch: {name} = {value:#010x}");
        self.values.insert(name.to_owned(), value);
        Outcome::Resolved
    }
}

/// An entry while it is being resolved.
struct Pending<'g> {
    /// Position among all entries: the order the bytes are written in.
    order: usize,
    group: usize,
    entry: &'g Entry,
    /// Instructions: final address and bytes with the relocations done so far.
    address: Option<u32>,
    data: Vec<u8>,
    done: Vec<bool>,
}

impl Pending<'_> {
    #[allow(clippy::too_many_lines)]
    fn resolve(&mut self, r: &mut Resolver<'_, '_>, name: &str) -> Outcome {
        match self.entry {
            Entry::Variable { line, name: symbol, expr } => match r.eval(self.group, expr) {
                Some(value) => r.register(name, *line, symbol, to_u32(value)),
                None => Outcome::Unknown,
            },
            Entry::CemuhookSymbol { line, name: symbol, expr } => {
                let Some(value) = r.eval(self.group, expr) else { return Outcome::Unknown };
                match r.relocate(self.group, to_u32(value)) {
                    Some(address) => r.register(name, *line, symbol, address),
                    None => {
                        r.errors.push(format!(
                            "[{name}, Line {line}] Address of {symbol} is not within any module \
                             section or code cave"
                        ));
                        Outcome::Failed
                    }
                }
            }
            Entry::Label { line, name: symbol, address } => {
                match r.relocate(self.group, *address) {
                    Some(relocated) => r.register(name, *line, symbol, relocated),
                    None => {
                        r.errors.push(format!(
                            "[{name}, Line {line}] Address {address:#08x} of label {symbol} does \
                             not point to any module section or code cave"
                        ));
                        Outcome::Failed
                    }
                }
            }
            Entry::Instruction { line, address, relocs, .. } => {
                let base = match self.address {
                    Some(base) => base,
                    None => match r.relocate(self.group, *address) {
                        Some(base) => {
                            self.address = Some(base);
                            base
                        }
                        None => {
                            r.errors.push(format!(
                                "[{name}, Line {line}] Address {address:#010x} is not within code \
                                 cave or any module section"
                            ));
                            return Outcome::Failed;
                        }
                    },
                };
                for (index, reloc) in relocs.iter().enumerate() {
                    if self.done[index] {
                        continue;
                    }
                    let Some(value) = r.eval(self.group, &reloc.expr) else {
                        return Outcome::Unknown;
                    };
                    let at = reloc.byte_offset as usize;
                    let word = |data: &[u8]| {
                        u32::from_be_bytes(data[at..at + 4].try_into().unwrap_or_default())
                    };
                    let target = to_u32(value);
                    let place = base.wrapping_add(reloc.byte_offset);
                    let out_of_range = |r: &mut Resolver<'_, '_>| {
                        r.errors.push(format!(
                            "[{name}, Line {line}] Target '{}' for branch out of range (use \
                             MTCTR + BCTR or similar for long distance branches)",
                            reloc.expr
                        ));
                        Outcome::Failed
                    };
                    #[allow(clippy::cast_possible_truncation)]
                    match reloc.kind {
                        RelocKind::Float => {
                            self.data[at..at + 4].copy_from_slice(&(value as f32).to_be_bytes());
                        }
                        RelocKind::Double => {
                            self.data[at..at + 8].copy_from_slice(&value.to_be_bytes());
                        }
                        RelocKind::U32 => {
                            self.data[at..at + 4].copy_from_slice(&target.to_be_bytes());
                        }
                        RelocKind::U16 => {
                            self.data[at..at + 2].copy_from_slice(&(target as u16).to_be_bytes());
                        }
                        RelocKind::U8 => self.data[at] = target as u8,
                        RelocKind::MaskedImm => {
                            let mask =
                                (u32::MAX >> (32 - u32::from(reloc.bit_count))) << reloc.bit_offset;
                            let opcode =
                                (word(&self.data) & !mask) | ((target << reloc.bit_offset) & mask);
                            self.data[at..at + 4].copy_from_slice(&opcode.to_be_bytes());
                        }
                        RelocKind::BranchS26 => {
                            let mut opcode = word(&self.data);
                            let field = if opcode & 2 != 0 {
                                if target >= 0x03FF_FFFC {
                                    return out_of_range(r);
                                }
                                target
                            } else if target < place {
                                let back = place - target;
                                if back > 0x01FF_FFFF {
                                    return out_of_range(r);
                                }
                                back.wrapping_neg()
                            } else {
                                let forward = target - place;
                                if forward >= 0x01FF_FFFF {
                                    return out_of_range(r);
                                }
                                forward
                            };
                            opcode = (opcode & !0x03FF_FFFC) | (field & 0x03FF_FFFC);
                            self.data[at..at + 4].copy_from_slice(&opcode.to_be_bytes());
                        }
                        RelocKind::BranchS16 => {
                            let mut opcode = word(&self.data);
                            let field = if target < place {
                                let back = place - target;
                                if back > 0x8000 {
                                    return out_of_range(r);
                                }
                                back.wrapping_neg()
                            } else {
                                let forward = target - place;
                                if forward >= 0x8000 {
                                    return out_of_range(r);
                                }
                                forward
                            };
                            opcode = (opcode & !0xFFFC) | (field & 0xFFFC);
                            self.data[at..at + 4].copy_from_slice(&opcode.to_be_bytes());
                        }
                    }
                    self.done[index] = true;
                }
                Outcome::Resolved
            }
        }
    }
}

/// Applies the groups of one pack that fit `module` (Cemu's `ApplyPatchGroups`).
/// `constants` are the pack's preset variables (`$name`).
///
/// # Errors
/// What could not be resolved or placed; nothing of the pack applies then.
pub fn apply(
    groups: &[PatchGroup],
    module: &ModuleInfo,
    constants: &HashMap<String, f64>,
    host: &mut dyn PatchHost,
) -> Result<AppliedGroups, Vec<String>> {
    let groups: Vec<&PatchGroup> =
        groups.iter().filter(|g| g.matches(module.crc, module.is_rpx)).collect();
    let mut applied = AppliedGroups::default();
    if groups.is_empty() {
        return Ok(applied);
    }
    let mut caves = Vec::with_capacity(groups.len());
    for group in &groups {
        if group.code_cave_size == 0 {
            caves.push(0);
            applied.groups.push((group.name.clone(), None));
            continue;
        }
        let Some(cave) = host.alloc_code_cave(group.code_cave_size) else {
            return Err(vec![format!(
                "[{}] Not enough code cave memory for {} bytes",
                group.name, group.code_cave_size
            )]);
        };
        caves.push(cave);
        applied.groups.push((group.name.clone(), Some((cave, group.code_cave_size))));
    }

    let mut resolver = Resolver {
        module,
        constants,
        host: RefCell::new(host),
        values: HashMap::new(),
        caves,
        unknown: RefCell::new(Vec::new()),
        errors: Vec::new(),
    };
    let mut pending: Vec<Pending<'_>> = groups
        .iter()
        .enumerate()
        .flat_map(|(group, g)| g.entries.iter().map(move |entry| (group, entry)))
        .enumerate()
        .map(|(order, (group, entry))| {
            let (data, count) = match entry {
                Entry::Instruction { data, relocs, .. } => (data.clone(), relocs.len()),
                _ => (Vec::new(), 0),
            };
            Pending { order, group, entry, address: None, data, done: vec![false; count] }
        })
        .collect();
    let mut done: Vec<Pending<'_>> = Vec::with_capacity(pending.len());

    const PASSES: usize = 30;
    for pass in 0..PASSES {
        let mut resolved = 0;
        let mut waiting = Vec::new();
        for mut entry in pending {
            let name = groups[entry.group].name.as_str();
            match entry.resolve(&mut resolver, name) {
                Outcome::Resolved => {
                    resolved += 1;
                    done.push(entry);
                }
                Outcome::Unknown | Outcome::Failed => waiting.push(entry),
            }
        }
        pending = waiting;
        if !resolver.errors.is_empty() {
            return Err(resolver.errors);
        }
        if pending.is_empty() {
            break;
        }
        if resolved == 0 || pass == PASSES - 1 {
            // Stuck: report every name that is still missing.
            let mut report = Vec::new();
            if pass == PASSES - 1 {
                report.push(
                    "Some symbols could not be resolved because the dependency chain is too deep"
                        .to_owned(),
                );
            }
            let mut missing = BTreeSet::new();
            for mut entry in pending {
                resolver.unknown.borrow_mut().clear();
                let name = groups[entry.group].name.clone();
                let _ = entry.resolve(&mut resolver, &name);
                for symbol in resolver.unknown.borrow().iter() {
                    missing.insert((entry.entry.line(), name.clone(), symbol.clone()));
                }
            }
            report.extend(missing.into_iter().map(|(line, group, symbol)| {
                format!("[{group}, Line {line}] Unresolved symbol: {symbol}")
            }));
            report.extend(resolver.errors);
            return Err(report);
        }
    }

    // In file order, as Cemu writes them.
    done.sort_by_key(|p| p.order);
    for entry in done {
        if let (Entry::Instruction { .. }, Some(address)) = (entry.entry, entry.address) {
            applied.writes.push((address, entry.data));
        }
    }
    for group in &groups {
        for name in &group.entry_callbacks {
            match resolver.values.get(name) {
                Some(&address) => applied.entry_callbacks.push(address),
                None => {
                    return Err(vec![format!(
                        "[{}] Failed to resolve .callback symbol: {name}",
                        group.name
                    )]);
                }
            }
        }
    }
    Ok(applied)
}
