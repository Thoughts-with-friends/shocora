//! `printf`-style formatting for `OSReport`, `__os_snprintf`, ...
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ format string +    │   │ coreinit/printf.rs       │   │ text               │
//! │ values             │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ printf formatting        │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Turns a C-style format string and values into text, for the game's debug messages. Terms
//! such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, clippy::cast_sign_loss)]

use crate::hle::HleError;

/// Source of variadic arguments.
pub trait VarArgs {
    /// # Errors
    /// Fails if the argument cannot be read.
    fn next_int(&mut self) -> Result<u32, HleError>;
    /// # Errors
    /// Fails if the argument cannot be read.
    fn next_long(&mut self) -> Result<u64, HleError>;
    /// # Errors
    /// Fails if the argument cannot be read.
    fn next_double(&mut self) -> Result<f64, HleError>;
    /// # Errors
    /// Fails if the string cannot be read.
    fn string_at(&mut self, address: u32) -> Result<String, HleError>;
}

#[derive(Debug, Default, Clone, Copy)]
struct Spec {
    left: bool,
    zero: bool,
    plus: bool,
    space: bool,
    alt: bool,
    width: usize,
    precision: Option<usize>,
    long_long: bool,
    short: u8,
}

impl Spec {
    /// Pads `body` (with an optional sign/prefix kept in front of zero padding).
    fn pad(&self, prefix: &str, body: &str, numeric: bool) -> String {
        let len = prefix.len() + body.len();
        if len >= self.width {
            return format!("{prefix}{body}");
        }
        let fill = self.width - len;
        if self.left {
            format!("{prefix}{body}{}", " ".repeat(fill))
        } else if self.zero && numeric && self.precision.is_none() {
            format!("{prefix}{}{body}", "0".repeat(fill))
        } else {
            format!("{}{prefix}{body}", " ".repeat(fill))
        }
    }

    fn sign(&self, negative: bool) -> &'static str {
        if negative {
            "-"
        } else if self.plus {
            "+"
        } else if self.space {
            " "
        } else {
            ""
        }
    }

    fn integer_digits(&self, digits: String) -> String {
        match self.precision {
            Some(0) if digits == "0" => String::new(),
            Some(p) if digits.len() < p => format!("{}{digits}", "0".repeat(p - digits.len())),
            _ => digits,
        }
    }
}

/// `%e` formatting with C's exponent style (`1.500000e+00`).
fn format_exp(value: f64, precision: usize, upper: bool) -> String {
    let text = format!("{value:.precision$e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if exponent < 0 { '-' } else { '+' };
    let e = if upper { 'E' } else { 'e' };
    format!("{mantissa}{e}{sign}{:02}", exponent.abs())
}

/// `%g`: shortest of `%e`/`%f` with trailing zeros removed.
fn format_general(value: f64, precision: usize, upper: bool, alt: bool) -> String {
    let precision = precision.max(1);
    if value == 0.0 {
        return "0".to_owned();
    }
    let exponent = value.abs().log10().floor() as i32;
    let trim = |s: String| {
        if alt || !s.contains('.') {
            s
        } else {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
    };
    if exponent < -4 || exponent >= precision as i32 {
        let text = format_exp(value, precision - 1, upper);
        let (mantissa, rest) = text.split_at(text.find(['e', 'E']).unwrap_or(text.len()));
        format!("{}{rest}", trim(mantissa.to_owned()))
    } else {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        trim(format!("{value:.decimals$}"))
    }
}

/// Formats `format` with arguments from `args`.
///
/// # Errors
/// Fails if an argument cannot be read.
pub fn format(format: &str, args: &mut dyn VarArgs) -> Result<String, HleError> {
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = Spec::default();
        while let Some(&flag) = chars.peek() {
            match flag {
                '-' => spec.left = true,
                '0' => spec.zero = true,
                '+' => spec.plus = true,
                ' ' => spec.space = true,
                '#' => spec.alt = true,
                _ => break,
            }
            chars.next();
        }
        if chars.peek() == Some(&'*') {
            chars.next();
            let width = args.next_int()? as i32;
            spec.left |= width < 0;
            spec.width = width.unsigned_abs() as usize;
        }
        while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
            spec.width = spec.width * 10 + d as usize;
            chars.next();
        }
        if chars.peek() == Some(&'.') {
            chars.next();
            let mut precision = if chars.peek() == Some(&'*') {
                chars.next();
                (args.next_int()? as i32).max(0) as usize
            } else {
                0
            };
            while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                precision = precision * 10 + d as usize;
                chars.next();
            }
            spec.precision = Some(precision);
        }
        while let Some(&length) = chars.peek() {
            match length {
                'l' | 'j' | 'q' => {
                    // `l` is 32-bit on Espresso; `ll`, `j` and `q` are 64-bit.
                    if length != 'l' || spec.short == b'l' {
                        spec.long_long = true;
                    }
                    spec.short = b'l';
                }
                'h' => spec.short = if spec.short == b'h' { b'b' } else { b'h' },
                'z' | 't' | 'L' => {}
                _ => break,
            }
            chars.next();
        }
        let Some(conversion) = chars.next() else { break };
        let piece = match conversion {
            'd' | 'i' => {
                let value: i64 = if spec.long_long {
                    args.next_long()? as i64
                } else {
                    let raw = args.next_int()?;
                    match spec.short {
                        b'h' => i64::from(raw as i16),
                        b'b' => i64::from(raw as i8),
                        _ => i64::from(raw as i32),
                    }
                };
                let digits = spec.integer_digits(value.unsigned_abs().to_string());
                spec.pad(spec.sign(value < 0), &digits, true)
            }
            'u' | 'x' | 'X' | 'o' => {
                let value: u64 = if spec.long_long {
                    args.next_long()?
                } else {
                    let raw = args.next_int()?;
                    match spec.short {
                        b'h' => u64::from(raw as u16),
                        b'b' => u64::from(raw as u8),
                        _ => u64::from(raw),
                    }
                };
                let digits = spec.integer_digits(match conversion {
                    'x' => format!("{value:x}"),
                    'X' => format!("{value:X}"),
                    'o' => format!("{value:o}"),
                    _ => value.to_string(),
                });
                let prefix = match conversion {
                    'x' if spec.alt && value != 0 => "0x",
                    'X' if spec.alt && value != 0 => "0X",
                    'o' if spec.alt => "0",
                    _ => "",
                };
                spec.pad(prefix, &digits, true)
            }
            'p' => spec.pad("0x", &format!("{:08x}", args.next_int()?), true),
            'c' => spec.pad("", &char::from(args.next_int()? as u8).to_string(), false),
            's' => {
                let address = args.next_int()?;
                let mut text =
                    if address == 0 { "(null)".to_owned() } else { args.string_at(address)? };
                if let Some(p) = spec.precision {
                    text = text.chars().take(p).collect();
                }
                spec.pad("", &text, false)
            }
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => {
                let value = args.next_double()?;
                let precision = spec.precision.unwrap_or(6);
                let body = if value.is_nan() {
                    "nan".to_owned()
                } else if value.is_infinite() {
                    "inf".to_owned()
                } else {
                    match conversion {
                        'e' | 'E' => format_exp(value.abs(), precision, conversion == 'E'),
                        'g' | 'G' => {
                            format_general(value.abs(), precision, conversion == 'G', spec.alt)
                        }
                        _ => format!("{:.precision$}", value.abs()),
                    }
                };
                let body = if conversion.is_ascii_uppercase() { body.to_uppercase() } else { body };
                spec.pad(
                    spec.sign(value.is_sign_negative() && !value.is_nan()),
                    &body,
                    value.is_finite(),
                )
            }
            'n' => {
                args.next_int()?;
                String::new()
            }
            '%' => "%".to_owned(),
            other => format!("%{other}"),
        };
        out.push_str(&piece);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Arg {
        I(u32),
        L(u64),
        D(f64),
        S(&'static str),
    }

    struct Args(std::vec::IntoIter<Arg>, Vec<&'static str>);

    impl VarArgs for Args {
        fn next_int(&mut self) -> Result<u32, HleError> {
            Ok(match self.0.next() {
                Some(Arg::I(v)) => v,
                Some(Arg::S(s)) => {
                    self.1.push(s);
                    self.1.len() as u32
                }
                _ => 0,
            })
        }
        fn next_long(&mut self) -> Result<u64, HleError> {
            Ok(match self.0.next() {
                Some(Arg::L(v)) => v,
                _ => 0,
            })
        }
        fn next_double(&mut self) -> Result<f64, HleError> {
            Ok(match self.0.next() {
                Some(Arg::D(v)) => v,
                _ => 0.0,
            })
        }
        fn string_at(&mut self, address: u32) -> Result<String, HleError> {
            Ok(self.1[address as usize - 1].to_owned())
        }
    }

    fn f(fmt: &str, args: Vec<Arg>) -> String {
        format(fmt, &mut Args(args.into_iter(), Vec::new())).unwrap()
    }

    #[test]
    fn integers() {
        assert_eq!(
            f(
                "%d|%5d|%-5d|%05d",
                vec![Arg::I((-42_i32) as u32), Arg::I(7), Arg::I(7), Arg::I((-7_i32) as u32)]
            ),
            "-42|    7|7    |-0007"
        );
        assert_eq!(
            f("%x %#X %08x %u", vec![Arg::I(255), Arg::I(255), Arg::I(0xBEEF), Arg::I(u32::MAX)]),
            "ff 0XFF 0000beef 4294967295"
        );
        assert_eq!(
            f("%lld %llx %hd", vec![Arg::L(u64::MAX), Arg::L(0x1_0000_0000), Arg::I(0xFFFF)]),
            "-1 100000000 -1"
        );
        assert_eq!(f("%.3d|%+d|% d", vec![Arg::I(5), Arg::I(5), Arg::I(5)]), "005|+5| 5");
    }

    #[test]
    fn strings_and_chars() {
        assert_eq!(
            f(
                "[%s] [%-6s] [%.2s] [%c] 100%%",
                vec![Arg::S("hi"), Arg::S("ab"), Arg::S("xyz"), Arg::I(u32::from(b'Q'))]
            ),
            "[hi] [ab    ] [xy] [Q] 100%"
        );
        assert_eq!(f("%s", vec![Arg::I(0)]), "(null)");
        assert_eq!(f("%*d", vec![Arg::I(4), Arg::I(1)]), "   1");
    }

    #[test]
    fn floats() {
        assert_eq!(
            f("%f %.2f %8.3f", vec![Arg::D(1.5), Arg::D(-2.345), Arg::D(1.23456)]),
            "1.500000 -2.35    1.235"
        );
        assert_eq!(f("%e %.1E", vec![Arg::D(1234.5), Arg::D(0.00012)]), "1.234500e+03 1.2E-04");
        assert_eq!(
            f("%g %g %g %g", vec![Arg::D(100_000.0), Arg::D(1e7), Arg::D(0.0001), Arg::D(0.5)]),
            "100000 1e+07 0.0001 0.5"
        );
    }
}
