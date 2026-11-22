//! Code patches of graphic packs, ported from Cemu's `GraphicPack2Patches*.cpp`.
//!
//! A pack's `patch_*.asm` files (or a Cemuhook `patches.txt` without them) hold groups of
//! PowerPC code. A group names the modules it fits by checksum (`moduleMatches`) and
//! writes instructions either over the module's code (`0x02123456 = nop`) or into a code
//! cave of its own (`.origin = codecave`), with labels and variables between them.
//! [`parse_cemu`] and [`parse_cemuhook`] read the files; [`apply`] resolves the symbols
//! against a loaded module and gives the bytes to write.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ patch_*.asm files  │   │ patch/mod.rs             │   │ changed game code  │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ code patches             │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Packs can patch the game's own machine code (for example FPS++). This reads those patch
//! files. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
mod apply;
pub mod asm;

use std::path::Path;

pub use apply::{AppliedGroups, ModuleInfo, PatchHost, apply};

/// Addresses below this are offsets into the group's code cave.
pub const CODECAVE_MAX_OFFSET: u32 = 8 * 1024 * 1024;

/// One line of a group, in file order.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// Assembled bytes at `address` (module or code cave address) with their relocations.
    Instruction { line: u32, address: u32, data: Vec<u8>, relocs: Vec<asm::Reloc> },
    /// `name:`, at `address`.
    Label { line: u32, name: String, address: u32 },
    /// `name = expression`.
    Variable { line: u32, name: String, expr: String },
    /// Cemuhook `name = address`: an address in the module.
    CemuhookSymbol { line: u32, name: String, expr: String },
}

impl Entry {
    const fn line(&self) -> u32 {
        match self {
            Self::Instruction { line, .. }
            | Self::Label { line, .. }
            | Self::Variable { line, .. }
            | Self::CemuhookSymbol { line, .. } => *line,
        }
    }
}

/// A `[Group]` of a patch file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PatchGroup {
    pub name: String,
    /// Checksums of the modules the group fits.
    pub module_matches: Vec<u32>,
    /// `moduleMatches = rpx`: the main executable, whatever its checksum.
    pub rpx_only: bool,
    pub entries: Vec<Entry>,
    /// `.callback entry <symbol>`: functions to call before the title's entry point.
    pub entry_callbacks: Vec<String>,
    /// Bytes of code cave the group needs.
    pub code_cave_size: u32,
}

impl PatchGroup {
    fn new(name: &str) -> Self {
        Self { name: name.to_owned(), ..Self::default() }
    }

    /// Whether the group applies to a module with checksum `crc`.
    pub fn matches(&self, crc: u32, is_rpx: bool) -> bool {
        self.module_matches.contains(&crc) || (self.rpx_only && is_rpx)
    }
}

/// A syntax error, with its line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct PatchError {
    pub line: u32,
    pub message: String,
}

fn err<T>(line: u32, message: impl Into<String>) -> Result<T, PatchError> {
    Err(PatchError { line, message: message.into() })
}

/// The length of `line` without its `#` / `;` comment (outside strings).
fn without_comment(line: &str) -> &str {
    let mut in_string = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_string = !in_string,
            '#' | ';' if !in_string => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Cemu's `StringTokenParser` over one line.
#[derive(Clone, Copy)]
struct Tokens<'a> {
    s: &'a str,
}

impl<'a> Tokens<'a> {
    fn skip_ws(&mut self) {
        self.s = self.s.trim_start_matches([' ', '\t']);
    }

    fn trim_ws(&mut self) {
        self.s = self.s.trim_end_matches([' ', '\t']);
    }

    const fn is_end(&self) -> bool {
        self.s.is_empty()
    }

    fn at(&self, index: usize) -> Option<u8> {
        self.s.as_bytes().get(index).copied()
    }

    /// Skips whitespace, then the case-insensitive prefix `word`.
    fn match_word(&mut self, word: &str) -> bool {
        let rest = self.s.trim_start_matches([' ', '\t']);
        match rest.get(..word.len()) {
            Some(head) if head.eq_ignore_ascii_case(word) => {
                self.s = &rest[word.len()..];
                true
            }
            _ => false,
        }
    }

    /// A hexadecimal number (`0x` or a leading zero) or a decimal one.
    fn parse_u32(&mut self) -> Option<u32> {
        let rest = self.s.trim_start_matches([' ', '\t']);
        let bytes = rest.as_bytes();
        let (hex, start) = match bytes {
            [b'0', b'x' | b'X', ..] => (true, 2),
            [b'0', ..] => (true, 1),
            [_, ..] => (false, 0),
            [] => return None,
        };
        if bytes.len() <= start {
            return None;
        }
        let radix = if hex { 16 } else { 10 };
        let mut value: u32 = 0;
        let mut end = start;
        while let Some(d) = bytes.get(end).and_then(|&b| char::from(b).to_digit(radix)) {
            value = value.wrapping_mul(radix).wrapping_add(d);
            end += 1;
        }
        if end == start {
            return None;
        }
        self.s = &rest[end..];
        Some(value)
    }

    /// A letter or `_`, then letters, digits, `_` and `.`.
    fn parse_symbol(&mut self) -> Option<&'a str> {
        let rest = self.s.trim_start_matches([' ', '\t']);
        let first = rest.bytes().next()?;
        if !(first.is_ascii_alphabetic() || first == b'_') {
            return None;
        }
        let len = rest
            .bytes()
            .position(|c| !(c.is_ascii_alphanumeric() || c == b'_' || c == b'.'))
            .unwrap_or(rest.len());
        self.s = &rest[len..];
        Some(&rest[..len])
    }
}

/// `[name]` alone on the line.
fn group_header(line_no: u32, tokens: &Tokens<'_>) -> Result<Option<String>, PatchError> {
    if tokens.at(0) != Some(b'[') {
        return Ok(None);
    }
    let body = &tokens.s[1..];
    let Some(end) = body.find(']') else { return err(line_no, "Expected ']'") };
    if !body[end + 1..].trim_matches([' ', '\t']).is_empty() {
        return err(line_no, "Unexpected characters after ']'");
    }
    Ok(Some(body[..end].to_owned()))
}

/// `moduleMatches = crc, crc, ...` (and `rpx` when `allow_rpx`).
fn module_matches(
    line_no: u32,
    tokens: &mut Tokens<'_>,
    group: Option<&mut PatchGroup>,
    allow_rpx: bool,
) -> Result<(), PatchError> {
    let Some(group) = group else {
        return err(line_no, "Specified 'ModuleMatches' outside of a group");
    };
    if !tokens.match_word("=") {
        return err(line_no, "Expected '=' after ModuleMatches");
    }
    loop {
        if allow_rpx && tokens.match_word("rpx") {
            group.rpx_only = true;
            break;
        }
        let Some(crc) = tokens.parse_u32() else {
            return err(line_no, "Invalid value for ModuleMatches");
        };
        group.module_matches.push(crc);
        if !tokens.match_word(",") {
            break;
        }
    }
    tokens.skip_ws();
    if !tokens.is_end() {
        return err(line_no, "Unexpected character");
    }
    Ok(())
}

/// Cemu's `AddPatchGroup`: the code cave covers every instruction placed in it.
fn finish_group(mut group: PatchGroup, groups: &mut Vec<PatchGroup>) -> Result<(), PatchError> {
    if group.module_matches.is_empty() && !group.rpx_only {
        return err(0, format!("Group \"{}\" has no moduleMatches definition", group.name));
    }
    group.code_cave_size = group
        .entries
        .iter()
        .filter_map(|e| match e {
            #[allow(clippy::cast_possible_truncation)]
            Entry::Instruction { address, data, .. } if *address < CODECAVE_MAX_OFFSET => {
                Some(address + data.len() as u32)
            }
            _ => None,
        })
        .max()
        .unwrap_or(0);
    groups.push(group);
    Ok(())
}

fn lines(text: &str) -> impl Iterator<Item = (u32, &str)> {
    (1_u32..).zip(text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)))
}

/// Reads a Cemu `patch_*.asm` file.
///
/// # Errors
/// The first syntax or assembler error; Cemu then applies none of the pack's patches.
#[allow(clippy::too_many_lines)]
pub fn parse_cemu(text: &str) -> Result<Vec<PatchGroup>, PatchError> {
    const INVALID: u32 = u32::MAX;
    let mut groups = Vec::new();
    let mut current: Option<PatchGroup> = None;
    let mut origin = INVALID;
    let mut cave_origin = 0_u32;
    // Labels in the code flow wait for the next data, so no padding falls between them.
    let mut scheduled: Vec<(u32, String)> = Vec::new();

    let flush = |current: &mut Option<PatchGroup>, scheduled: &mut Vec<(u32, String)>, at: u32| {
        if let Some(group) = current.as_mut() {
            for (line, name) in scheduled.drain(..) {
                group.entries.push(Entry::Label { line, name, address: at });
            }
        }
        scheduled.clear();
    };

    for (line_no, raw) in lines(text) {
        let mut tokens = Tokens { s: without_comment(raw) };
        tokens.skip_ws();
        if tokens.is_end() {
            continue;
        }
        if let Some(name) = group_header(line_no, &tokens)? {
            flush(&mut current, &mut scheduled, origin);
            if let Some(group) = current.take() {
                finish_group(group, &mut groups)?;
            }
            current = Some(PatchGroup::new(&name));
            origin = INVALID;
            cave_origin = 0;
            continue;
        }
        if tokens.match_word("moduleMatches") {
            module_matches(line_no, &mut tokens, current.as_mut(), true)?;
            continue;
        }
        let Some(group) = current.as_mut() else {
            return err(line_no, "Patch line outside of a group");
        };

        // `<address> = ...` places this one line there.
        let fixed = if tokens.at(0) == Some(b'0') && matches!(tokens.at(1), Some(b'x' | b'X')) {
            let Some(address) = tokens.parse_u32() else {
                return err(line_no, "Malformed address");
            };
            if !tokens.match_word("=") {
                return err(line_no, "Expected '=' after address");
            }
            tokens.skip_ws();
            tokens.trim_ws();
            address
        } else {
            INVALID
        };

        if tokens.match_word(".origin") {
            if fixed != INVALID {
                return err(
                    line_no,
                    ".origin directive must appear alone without <address> = prefix.",
                );
            }
            if !tokens.match_word("=") {
                return err(line_no, "Missing '=' after .origin");
            }
            if tokens.match_word("codecave") {
                origin = cave_origin;
            } else if let Some(address) = tokens.parse_u32() {
                origin = address;
            } else {
                return err(
                    line_no,
                    "'.origin =' must be followed by the keyword codecave or a valid address",
                );
            }
            continue;
        }
        if tokens.match_word(".callback") {
            if !tokens.match_word("entry") {
                return err(line_no, "Unrecognized type for '.callback'");
            }
            let Some(symbol) = tokens.parse_symbol() else {
                return err(line_no, "'.callback' must reference a symbol after the type");
            };
            group.entry_callbacks.push(symbol.to_owned());
            continue;
        }

        let saved = tokens;
        // `name:` is a label.
        if let Some(name) = tokens.parse_symbol()
            && tokens.match_word(":")
        {
            tokens.skip_ws();
            if !tokens.is_end() {
                return err(line_no, "Unexpected characters after label");
            }
            if fixed != INVALID {
                group.entries.push(Entry::Label {
                    line: line_no,
                    name: name.to_owned(),
                    address: fixed,
                });
            } else if origin == INVALID {
                return err(
                    line_no,
                    "Defined label has no address assigned or there is no active .origin",
                );
            } else {
                scheduled.push((line_no, name.to_owned()));
            }
            continue;
        }
        tokens = saved;
        // `name = expression` is a variable.
        if let Some(name) = tokens.parse_symbol()
            && tokens.match_word("=")
        {
            tokens.skip_ws();
            tokens.trim_ws();
            group.entries.push(Entry::Variable {
                line: line_no,
                name: name.to_owned(),
                expr: tokens.s.to_owned(),
            });
            continue;
        }
        tokens = saved;
        // Anything else is assembly.
        let address = if fixed != INVALID {
            fixed
        } else if origin != INVALID {
            origin
        } else {
            return err(
                line_no,
                "Trying to assemble line but no address specified. (Declare .origin or prefix \
                 line with <address> = )",
            );
        };
        let out = asm::assemble(tokens.s, address, fixed != INVALID).map_err(|e| PatchError {
            line: line_no,
            message: format!("Error in assembler: {e}"),
        })?;
        if fixed == INVALID {
            #[allow(clippy::cast_possible_truncation)]
            let size = out.data.len() as u32;
            for step in [out.padding, size] {
                origin = origin.wrapping_add(step);
                if origin <= 32 * 1024 * 1024 {
                    cave_origin = cave_origin.max(origin);
                }
            }
        }
        for (line, name) in std::mem::take(&mut scheduled) {
            group.entries.push(Entry::Label { line, name, address: out.address });
        }
        group.entries.push(Entry::Instruction {
            line: line_no,
            address: out.address,
            data: out.data,
            relocs: out.relocs,
        });
    }
    if current.is_some() {
        flush(&mut current, &mut scheduled, origin);
    }
    if let Some(group) = current {
        finish_group(group, &mut groups)?;
    }
    Ok(groups)
}

#[cfg(test)]
mod tests;

mod cemuhook;
#[allow(unused_imports)]
pub use cemuhook::*;
