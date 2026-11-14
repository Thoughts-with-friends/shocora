//! Cheat codes: memory writes repeated every frame, kept per title as a text file.
//!
//! A code starts with its name in brackets. Lines starting with `*` belong to it: the
//! marker `*shocora_enabled` (or Citra's `*citra_enabled`) turns it on, any other one is a
//! note. The rest are `address value` hex lines.
//!
//! ```text
//! [Rupees 999]
//! *shocora_enabled
//! *Works with v1.5.0
//! 1050_6A1C 03E7
//! ```
//!
//! The older form is still read: a code is a run of `address value` lines closed by a blank
//! or comment-only line, a `//` comment names it, and `// disabled` turns the next one off.
//!
//! The digits of the value give the width of the write, in guest (big-endian) order:
//! up to 2 are a byte, up to 4 a halfword, up to 8 a word. Addresses are effective ones.
use std::fmt::{self, Write as _};

use wiiu_memory::Memory;

/// The marker line of an enabled code.
const ENABLED: &str = "*shocora_enabled";
/// Citra's marker, read as ours.
const CITRA_ENABLED: &str = "*citra_enabled";

/// One value to store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheatValue {
    U8(u8),
    U16(u16),
    U32(u32),
}

/// One named cheat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cheat {
    pub name: String,
    /// Free text kept with the code (one entry per line).
    pub notes: Vec<String>,
    pub enabled: bool,
    pub writes: Vec<(u32, CheatValue)>,
}

/// Why a cheat text could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheatParseError {
    /// The line is not `address value`.
    InvalidFormat(usize),
    /// The token is not hexadecimal of a fitting length.
    InvalidHex(usize, String),
}

impl fmt::Display for CheatParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat(line) => {
                write!(f, "line {line}: not \"address value\"")
            }
            Self::InvalidHex(line, token) => {
                write!(f, "line {line}: not hexadecimal: {token}")
            }
        }
    }
}

impl std::error::Error for CheatParseError {}

fn parse_value(token: &str) -> Option<CheatValue> {
    let digits = token.replace('_', "");
    match digits.len() {
        1..=2 => u8::from_str_radix(&digits, 16).ok().map(CheatValue::U8),
        3..=4 => u16::from_str_radix(&digits, 16).ok().map(CheatValue::U16),
        5..=8 => u32::from_str_radix(&digits, 16).ok().map(CheatValue::U32),
        _ => None,
    }
}

/// Reads one `address value` line (`code` holds no comment and is not empty).
fn parse_write(code: &str, number: usize) -> Result<(u32, CheatValue), CheatParseError> {
    let mut tokens = code.split_whitespace();
    let (Some(address), Some(value), None) = (tokens.next(), tokens.next(), tokens.next()) else {
        return Err(CheatParseError::InvalidFormat(number));
    };
    let parsed = u32::from_str_radix(&address.replace('_', ""), 16)
        .map_err(|_| CheatParseError::InvalidHex(number, address.to_owned()))?;
    let value =
        parse_value(value).ok_or_else(|| CheatParseError::InvalidHex(number, value.to_owned()))?;
    Ok((parsed, value))
}

/// Reads the code lines of one cheat, as typed in an editor. `//` comments and blank lines
/// are skipped; line numbers in errors are those of `text`.
///
/// # Errors
/// Fails at the first line that is not `address value`.
pub fn parse_writes(text: &str) -> Result<Vec<(u32, CheatValue)>, CheatParseError> {
    let mut writes = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let code = line.split_once("//").map_or(line, |(code, _)| code).trim();
        if !code.is_empty() {
            writes.push(parse_write(code, index + 1)?);
        }
    }
    Ok(writes)
}

/// Reads a cheat text.
///
/// # Errors
/// Fails at the first line that is neither a comment, a header, a note nor `address value`.
pub fn parse(text: &str) -> Result<Vec<Cheat>, CheatParseError> {
    let mut cheats = Vec::new();
    let mut current = Cheat { enabled: true, ..Cheat::default() };
    // Inside a `[name]` code, blank lines do not end it.
    let mut bracketed = false;

    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let trimmed = line.trim();

        if let Some(name) = trimmed.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            if bracketed || !current.writes.is_empty() {
                cheats.push(std::mem::take(&mut current));
            }
            current = Cheat { name: name.trim().to_owned(), ..Cheat::default() };
            bracketed = true;
            continue;
        }

        if bracketed && let Some(note) = trimmed.strip_prefix('*') {
            if trimmed.eq_ignore_ascii_case(ENABLED) || trimmed.eq_ignore_ascii_case(CITRA_ENABLED)
            {
                current.enabled = true;
            } else {
                current.notes.push(note.trim().to_owned());
            }
            continue;
        }

        let (code, comment) =
            trimmed.split_once("//").map_or((trimmed, None), |(c, n)| (c, Some(n)));
        let code = code.trim();

        if code.is_empty() {
            if bracketed {
                continue;
            }
            if !current.writes.is_empty() {
                let next = Cheat { enabled: true, ..Cheat::default() };
                cheats.push(std::mem::replace(&mut current, next));
            }
            if let Some(comment) = comment.map(str::trim) {
                if comment.eq_ignore_ascii_case("disabled") {
                    current.enabled = false;
                } else {
                    current.name = comment.to_owned();
                }
            }
            continue;
        }

        current.writes.push(parse_write(code, number)?);
    }

    if bracketed || !current.writes.is_empty() {
        cheats.push(current);
    }

    Ok(cheats)
}

/// Formats one write as the parser reads it back.
fn write_line(out: &mut String, (address, value): (u32, CheatValue)) {
    let _ = match value {
        CheatValue::U8(value) => writeln!(out, "{address:08X} {value:02X}"),
        CheatValue::U16(value) => writeln!(out, "{address:08X} {value:04X}"),
        CheatValue::U32(value) => writeln!(out, "{address:08X} {value:08X}"),
    };
}

/// The code lines of a cheat, for an editor.
pub fn format_writes(writes: &[(u32, CheatValue)]) -> String {
    let mut out = String::new();
    for write in writes {
        write_line(&mut out, *write);
    }
    out
}

/// Writes cheats in the bracketed form; [`parse`] reads the result back unchanged.
pub fn to_text(cheats: &[Cheat]) -> String {
    let mut out = String::new();
    for cheat in cheats {
        let name = cheat.name.replace(['\r', '\n'], " ");
        let _ = writeln!(out, "[{}]", name.trim());
        if cheat.enabled {
            out.push_str(ENABLED);
            out.push('\n');
        }
        for note in &cheat.notes {
            let _ = writeln!(out, "*{}", note.replace(['\r', '\n'], " "));
        }
        out.push_str(&format_writes(&cheat.writes));
        out.push('\n');
    }
    out
}

/// The writes of the enabled cheats, flattened once so a frame does not look at the rest.
pub fn enabled_writes(cheats: &[Cheat]) -> Vec<(u32, CheatValue)> {
    cheats
        .iter()
        .filter(|cheat| cheat.enabled)
        .flat_map(|cheat| cheat.writes.iter().copied())
        .collect()
}

/// Performs `writes` (see [`enabled_writes`]).
///
/// A value already in place is not stored again, so other cores reading it keep their cache
/// lines. A write outside mapped memory is only skipped: a code for another version of the
/// title must not stop the game.
pub fn apply(writes: &[(u32, CheatValue)], memory: &mut Memory) {
    for &(address, value) in writes {
        let written = match value {
            CheatValue::U8(value) => match memory.read_u8(address) {
                Ok(old) if old == value => Ok(()),
                _ => memory.write_u8(address, value),
            },
            CheatValue::U16(value) => match memory.read_u16(address) {
                Ok(old) if old == value => Ok(()),
                _ => memory.write_u16(address, value),
            },
            CheatValue::U32(value) => match memory.read_u32(address) {
                Ok(old) if old == value => Ok(()),
                _ => memory.write_u32(address, value),
            },
        };
        if let Err(error) = written {
            log::trace!("cheat write {address:08X}: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use wiiu_mem_addr::{MEM2, Region};

    use super::*;

    /// Intent: the older `//` form keeps working.
    /// Pass: names, widths and the `disabled` marker come out as written.
    #[test]
    fn names_widths_and_the_disabled_marker() {
        let cheats = parse(
            "// Rupees\n1000_0000 03E7\n1000_0004 12345678\n\n// disabled\n// Hearts\n10000008 50 // full\n",
        )
        .unwrap();

        assert_eq!(cheats.len(), 2);
        assert_eq!(cheats[0].name, "Rupees");
        assert!(cheats[0].enabled);
        assert_eq!(cheats[0].writes[0], (0x1000_0000, CheatValue::U16(0x3E7)));
        assert_eq!(cheats[0].writes[1], (0x1000_0004, CheatValue::U32(0x1234_5678)));
        assert_eq!(cheats[1].name, "Hearts");
        assert!(!cheats[1].enabled);
        assert_eq!(cheats[1].writes, [(0x1000_0008, CheatValue::U8(0x50))]);
    }

    /// Intent: errors point at the offending line.
    /// Pass: each bad line yields its number and token.
    #[test]
    fn bad_lines_say_where() {
        assert_eq!(parse("// x\n1000"), Err(CheatParseError::InvalidFormat(2)));
        assert_eq!(parse("1000 ZZ"), Err(CheatParseError::InvalidHex(1, "ZZ".to_owned())));
        assert_eq!(parse("1000 1 2"), Err(CheatParseError::InvalidFormat(1)));
        assert_eq!(parse_writes("\n1000 ZZ"), Err(CheatParseError::InvalidHex(2, "ZZ".to_owned())));
    }

    /// Intent: the bracketed form keeps names, notes, the enabled marker and empty codes,
    /// and writing it out reads back the same.
    /// Pass: parse → to_text → parse gives equal cheats.
    #[test]
    fn bracketed_form_round_trips() {
        let text = "[Rupees]\n*shocora_enabled\n*v1.5\n1000_0000 03E7\n\n[Empty]\n\n[Old]\n*citra_enabled\n10000008 50\n";
        let cheats = parse(text).unwrap();
        assert_eq!(cheats.len(), 3);
        assert!(cheats[0].enabled && !cheats[1].enabled && cheats[2].enabled);
        assert_eq!(cheats[0].notes, ["v1.5"]);
        assert!(cheats[1].writes.is_empty());
        assert_eq!(parse(&to_text(&cheats)).unwrap(), cheats);
    }

    /// Intent: only enabled cheats write, and a bad address does not stop the others.
    /// Pass: memory holds the enabled values and not the disabled one.
    #[test]
    fn enabled_cheats_write_and_unmapped_ones_do_not_stop_the_rest() {
        let mut memory = Memory::new();
        memory.map(Region::new("MEM2", MEM2.start, 0x1000)).unwrap();

        let start = MEM2.start;
        let text = format!(
            "{:X} 11223344\n\n// disabled\n{:X} AB\n\nFFFFFF00 1\n{:X} CCDD\n",
            start,
            start + 4,
            start + 8
        );
        apply(&enabled_writes(&parse(&text).unwrap()), &mut memory);

        assert_eq!(memory.read_u32(start).unwrap(), 0x1122_3344);
        assert_eq!(memory.read_u8(start + 4).unwrap(), 0);
        assert_eq!(memory.read_u16(start + 8).unwrap(), 0xCCDD);
    }
}
