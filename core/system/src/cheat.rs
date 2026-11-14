//! Cheat codes: memory writes repeated every frame, kept per title as a text file.
//!
//! The text format follows lunaris's cheat editor: a code is a run of `address value`
//! hex lines; a blank line or a comment-only line closes it. A comment names the code,
//! and a comment that is exactly `disabled` turns the following code off.
//!
//! ```text
//! // Rupees 999
//! 1050_6A1C 03E7
//!
//! // disabled
//! // Full hearts
//! 1050_6A20 0050
//! ```
//!
//! The digits of the value give the width of the write, in guest (big-endian) order:
//! up to 2 are a byte, up to 4 a halfword, up to 8 a word. Addresses are effective ones.
use std::fmt;

use wiiu_memory::Memory;

/// One value to store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheatValue {
    U8(u8),
    U16(u16),
    U32(u32),
}

/// One named cheat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cheat {
    pub name: String,
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

/// Reads a cheat text.
///
/// # Errors
/// Fails at the first line that is neither a comment nor `address value`.
pub fn parse(text: &str) -> Result<Vec<Cheat>, CheatParseError> {
    let mut cheats = Vec::new();
    let (mut name, mut enabled, mut writes) = (String::new(), true, Vec::new());

    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let (code, comment) = line.split_once("//").map_or((line, None), |(c, n)| (c, Some(n)));
        let code = code.trim();

        if code.is_empty() {
            if !writes.is_empty() {
                let finished = std::mem::take(&mut writes);
                cheats.push(Cheat { name: std::mem::take(&mut name), enabled, writes: finished });
                enabled = true;
            }

            if let Some(comment) = comment.map(str::trim) {
                if comment.eq_ignore_ascii_case("disabled") {
                    enabled = false;
                } else {
                    name = comment.to_owned();
                }
            }
            continue;
        }

        let mut tokens = code.split_whitespace();
        let (Some(address), Some(value), None) = (tokens.next(), tokens.next(), tokens.next())
        else {
            return Err(CheatParseError::InvalidFormat(number));
        };

        let parsed = u32::from_str_radix(&address.replace('_', ""), 16)
            .map_err(|_| CheatParseError::InvalidHex(number, address.to_owned()))?;
        let value = parse_value(value)
            .ok_or_else(|| CheatParseError::InvalidHex(number, value.to_owned()))?;
        writes.push((parsed, value));
    }

    if !writes.is_empty() {
        cheats.push(Cheat { name, enabled, writes });
    }

    Ok(cheats)
}

/// Performs the writes of the enabled cheats. A write outside mapped memory only skips that
/// write: a code for another version of the title must not stop the game.
pub fn apply(cheats: &[Cheat], memory: &mut Memory) {
    for cheat in cheats.iter().filter(|cheat| cheat.enabled) {
        for (address, value) in &cheat.writes {
            let written = match *value {
                CheatValue::U8(value) => memory.write_u8(*address, value),
                CheatValue::U16(value) => memory.write_u16(*address, value),
                CheatValue::U32(value) => memory.write_u32(*address, value),
            };

            if let Err(error) = written {
                log::debug!("cheat {:?}: {error}", cheat.name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use wiiu_mem_addr::{MEM2, Region};

    use super::*;

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

    #[test]
    fn bad_lines_say_where() {
        assert_eq!(parse("// x\n1000"), Err(CheatParseError::InvalidFormat(2)));
        assert_eq!(parse("1000 ZZ"), Err(CheatParseError::InvalidHex(1, "ZZ".to_owned())));
        assert_eq!(parse("1000 1 2"), Err(CheatParseError::InvalidFormat(1)));
    }

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
        apply(&parse(&text).unwrap(), &mut memory);

        assert_eq!(memory.read_u32(start).unwrap(), 0x1122_3344);
        assert_eq!(memory.read_u8(start + 4).unwrap(), 0);
        assert_eq!(memory.read_u16(start + 8).unwrap(), 0xCCDD);
    }
}
