//! Port of Cemu's `TExpressionParser`: the arithmetic in `rules.txt` and patch files.
//!
//! Infix expressions over numbers and named constants (`$width / 2`, `0x1000 + label@ha`)
//! with `+ - * / ^ %`, comparisons (`== != < <= > >=`, which give 1 or 0), parentheses,
//! optional `name(x)` functions and one `@ha`/`@hi`/`@h`/`@lo`/`@l` relocation suffix.
use std::collections::HashMap;

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExprError {
    #[error("empty expression is not allowed")]
    Empty,
    #[error("unknown constant \"{name}\" in expression: {expr}")]
    UnknownConstant { name: String, expr: String },
    #[error("parentheses mismatch in expression: {0}")]
    Parentheses(String),
    #[error("invalid operator in expression: {0}")]
    Operator(String),
    #[error("can't parse number: {0}")]
    Number(String),
    #[error("not enough parameters in expression: {0}")]
    Operands(String),
    #[error("mismatching relocation modifiers (suffix @..) in expression: {0}")]
    Reloc(String),
    #[error("unknown function \"{0}\"")]
    Function(String),
}

/// `@ha`, `@hi`/`@h`, `@lo`/`@l`: the half of a 32-bit address an instruction wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reloc {
    None,
    High,
    HighArithmetic,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    const fn precedence(self) -> u8 {
        match self {
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge => 1,
            Self::Add | Self::Sub => 2,
            Self::Mul | Self::Div | Self::Mod => 3,
            Self::Pow => 4,
        }
    }

    const fn right_associative(self) -> bool {
        matches!(self, Self::Pow)
    }

    /// Cemu evaluates in `double` (or `int` for preset conditions); `integer` truncates
    /// each result as the `int` instantiation does.
    fn apply(self, lhs: f64, rhs: f64, integer: bool) -> f64 {
        let truthy = |b: bool| if b { 1.0 } else { 0.0 };
        let value = match self {
            Self::Add => lhs + rhs,
            Self::Sub => lhs - rhs,
            Self::Mul => lhs * rhs,
            Self::Div if rhs == 0.0 => 0.0,
            Self::Div => lhs / rhs,
            Self::Pow => lhs.powf(rhs),
            Self::Mod if rhs.round() == 0.0 => 0.0,
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            Self::Mod => ((lhs.round() as i64) % (rhs.round() as i64)) as f64,
            Self::Eq if integer => truthy(lhs == rhs),
            Self::Ne if integer => truthy(lhs != rhs),
            Self::Eq => truthy((lhs - rhs).abs() <= 1e-10),
            Self::Ne => truthy((lhs - rhs).abs() > 1e-10),
            Self::Lt => truthy(lhs < rhs),
            Self::Le => truthy(lhs <= rhs),
            Self::Gt => truthy(lhs > rhs),
            Self::Ge => truthy(lhs >= rhs),
        };
        if integer { value.trunc() } else { value }
    }
}

#[derive(Debug, Clone)]
enum Token {
    Number(f64),
    Op(Op),
    Function(String),
    Open,
}

/// Function hook: `name(x)`.
pub type FunctionFn<'a> = dyn Fn(&str, f64) -> Option<f64> + 'a;
/// Fallback for names that are not constants.
pub type ConstantFn<'a> = dyn Fn(&str) -> Option<f64> + 'a;

/// Evaluates expressions against a table of constants.
#[derive(Default)]
pub struct Expr<'a> {
    constants: HashMap<String, f64>,
    constant_fallback: Option<Box<ConstantFn<'a>>>,
    function: Option<Box<FunctionFn<'a>>>,
    /// Evaluate as Cemu's `TExpressionParser<int>`: numbers and results are truncated.
    integer: bool,
}

impl<'a> Expr<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// As Cemu's `TExpressionParser<int>`, used for preset conditions.
    pub fn integer() -> Self {
        Self { integer: true, ..Self::default() }
    }

    pub fn add_constant(&mut self, name: &str, value: f64) {
        self.constants.insert(name.to_owned(), value);
    }

    /// Adds `name` unless it is already defined.
    pub fn try_add_constant(&mut self, name: &str, value: f64) {
        self.constants.entry(name.to_owned()).or_insert(value);
    }

    pub fn constant(&self, name: &str) -> Option<f64> {
        self.constants.get(name).copied()
    }

    /// Every constant, by name.
    #[inline]
    pub const fn constants(&self) -> &HashMap<String, f64> {
        &self.constants
    }

    pub fn set_constant_fallback(&mut self, f: impl Fn(&str) -> Option<f64> + 'a) {
        self.constant_fallback = Some(Box::new(f));
    }

    pub fn set_function(&mut self, f: impl Fn(&str, f64) -> Option<f64> + 'a) {
        self.function = Some(Box::new(f));
    }

    /// Whether `expr` parses, with every unknown name read as 1.
    pub fn is_valid(expr: &str) -> bool {
        let mut e = Expr::new();
        e.set_constant_fallback(|_| Some(1.0));
        e.eval(expr).is_ok()
    }

    /// Evaluates `expr`.
    ///
    /// # Errors
    /// Fails on a syntax error or an unknown name.
    #[allow(clippy::too_many_lines)]
    pub fn eval(&self, expr: &str) -> Result<f64, ExprError> {
        if expr.is_empty() {
            return Err(ExprError::Empty);
        }
        let bytes = expr.as_bytes();
        let mut output: Vec<Token> = Vec::new();
        let mut operators: Vec<Token> = Vec::new();
        let mut reloc = Reloc::None;
        let mut last_operator = true;
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b' ' || c == b'\t' {
                i += 1;
                continue;
            }
            if c.is_ascii_digit() || (last_operator && (c == b'-' || c == b'+')) {
                let (mut value, used) = parse_number(&expr[i..])?;
                if self.integer {
                    value = value.trunc();
                }
                output.push(Token::Number(value));
                i += used;
                last_operator = false;
                i += parse_reloc(&expr[i..], &mut reloc, false, expr)?;
                continue;
            }
            if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
                let mut j = i + 1;
                while j < bytes.len() {
                    let v = bytes[j];
                    if !(v.is_ascii_alphanumeric() || v == b'_' || v == b'@' || v == b'.') {
                        break;
                    }
                    j += 1;
                }
                let mut name = &expr[i..j];
                if let Some(at) = name.rfind('@') {
                    let used = parse_reloc(&name[at..], &mut reloc, true, expr)?;
                    name = &name[..name.len() - used];
                }
                if self.function.is_some() && bytes.get(j) == Some(&b'(') {
                    operators.push(Token::Function(name.to_owned()));
                    operators.push(Token::Open);
                    i = j + 1;
                    last_operator = true;
                    continue;
                }
                let value =
                    match self.constants.get(name) {
                        Some(v) => *v,
                        None => self.constant_fallback.as_ref().and_then(|f| f(name)).ok_or_else(
                            || ExprError::UnknownConstant {
                                name: name.to_owned(),
                                expr: expr.to_owned(),
                            },
                        )?,
                    };
                output.push(Token::Number(value));
                i = j;
                last_operator = false;
                continue;
            }
            if c == b'(' {
                operators.push(Token::Open);
                i += 1;
                last_operator = true;
                continue;
            }
            if c == b')' {
                let mut matched = false;
                while let Some(top) = operators.pop() {
                    if matches!(top, Token::Open) {
                        matched = true;
                        break;
                    }
                    output.push(top);
                }
                if !matched {
                    return Err(ExprError::Parentheses(expr.to_owned()));
                }
                i += 1;
                continue;
            }
            let next = bytes.get(i + 1).copied();
            let op = match (c, next) {
                (b'+', _) => Op::Add,
                (b'-', _) => Op::Sub,
                (b'*', _) => Op::Mul,
                (b'/', _) => Op::Div,
                (b'^', _) => Op::Pow,
                (b'%', _) => Op::Mod,
                (b'=', Some(b'=')) => {
                    i += 1;
                    Op::Eq
                }
                (b'!', Some(b'=')) => {
                    i += 1;
                    Op::Ne
                }
                (b'<', Some(b'=')) => {
                    i += 1;
                    Op::Le
                }
                (b'<', _) => Op::Lt,
                (b'>', Some(b'=')) => {
                    i += 1;
                    Op::Ge
                }
                (b'>', _) => Op::Gt,
                _ => return Err(ExprError::Operator(expr.to_owned())),
            };
            while let Some(Token::Op(top)) = operators.last() {
                let pops = if op.right_associative() {
                    op.precedence() < top.precedence()
                } else {
                    op.precedence() <= top.precedence()
                };
                if !pops {
                    break;
                }
                output.extend(operators.pop());
            }
            operators.push(Token::Op(op));
            i += 1;
            last_operator = true;
        }
        while let Some(top) = operators.pop() {
            if matches!(top, Token::Open) {
                return Err(ExprError::Parentheses(expr.to_owned()));
            }
            output.push(top);
        }
        let mut stack: Vec<f64> = Vec::new();
        for token in output {
            match token {
                Token::Number(n) => stack.push(n),
                Token::Function(name) => {
                    let arg = stack.pop().ok_or_else(|| ExprError::Operands(expr.to_owned()))?;
                    let f =
                        self.function.as_ref().ok_or_else(|| ExprError::Function(name.clone()))?;
                    stack.push(f(&name, arg).ok_or(ExprError::Function(name))?);
                }
                Token::Op(op) => {
                    let rhs = stack.pop().ok_or_else(|| ExprError::Operands(expr.to_owned()))?;
                    let lhs = stack.pop().ok_or_else(|| ExprError::Operands(expr.to_owned()))?;
                    stack.push(op.apply(lhs, rhs, self.integer));
                }
                Token::Open => return Err(ExprError::Parentheses(expr.to_owned())),
            }
        }
        let top = stack.last().copied().ok_or_else(|| ExprError::Operands(expr.to_owned()))?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let addr = top as i64 as u32;
        Ok(match reloc {
            Reloc::None => top,
            Reloc::High => f64::from(addr >> 16),
            Reloc::HighArithmetic => {
                f64::from(((addr >> 16) + u32::from(addr & 0x8000 != 0)) & 0xFFFF)
            }
            Reloc::Low => f64::from(addr & 0xFFFF),
        })
    }

    /// Evaluates and converts as Cemu's `Evaluate<T>` cast does (toward zero).
    ///
    /// # Errors
    /// See [`Self::eval`].
    #[allow(clippy::cast_possible_truncation)]
    pub fn eval_i64(&self, expr: &str) -> Result<i64, ExprError> {
        self.eval(expr).map(|v| v as i64)
    }
}

fn parse_reloc(s: &str, reloc: &mut Reloc, lenient: bool, expr: &str) -> Result<usize, ExprError> {
    let Some(rest) = s.strip_prefix('@') else { return Ok(0) };
    let lower = rest.to_ascii_lowercase();
    let (kind, len) = if lower.starts_with("ha") {
        (Reloc::HighArithmetic, 3)
    } else if lower.starts_with("hi") {
        (Reloc::High, 3)
    } else if lower.starts_with("lo") {
        (Reloc::Low, 3)
    } else if lower.starts_with('h') {
        (Reloc::High, 2)
    } else if lower.starts_with('l') {
        (Reloc::Low, 2)
    } else if lenient || rest.is_empty() {
        return Ok(0);
    } else {
        return Err(ExprError::Reloc(expr.to_owned()));
    };
    if *reloc != Reloc::None && *reloc != kind {
        return Err(ExprError::Reloc(expr.to_owned()));
    }
    *reloc = kind;
    Ok(len)
}

/// Cemu's `ConvertString`: a sign, then `0x` hex, a leading-zero hex (`01C`), or a
/// decimal with an optional point and exponent. Returns the value and bytes used.
pub fn parse_number(s: &str) -> Result<(f64, usize), ExprError> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Err(ExprError::Number(String::new()));
    }
    let mut pos = 0;
    let negative = bytes[0] == b'-';
    if negative || bytes[0] == b'+' {
        pos = 1;
    }
    let body = &s[pos..];
    let b = body.as_bytes();
    let hex = |start: usize| -> Result<(f64, usize), ExprError> {
        let digits = b[start..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
        if digits == 0 {
            return Err(ExprError::Number(s.to_owned()));
        }
        let text = &body[start..start + digits];
        let value = i64::from_str_radix(text, 16)
            .or_else(|_| u64::from_str_radix(text, 16).map(u64::cast_signed))
            .map_err(|_| ExprError::Number(s.to_owned()))?;
        #[allow(clippy::cast_precision_loss)]
        Ok((value as f64, pos + start + digits))
    };
    let (value, used) = if b.len() >= 3 && b[0] == b'0' && (b[1] | 0x20) == b'x' {
        hex(2)?
    } else if b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_hexdigit() && !body.contains('.') {
        hex(1)?
    } else {
        let mut point = false;
        let mut exponent = false;
        let mut end = 0;
        for (k, &c) in b.iter().enumerate() {
            let ok = c.is_ascii_digit()
                || (c == b'.' && !point && {
                    point = true;
                    true
                })
                || ((c | 0x20) == b'e' && !exponent && {
                    exponent = true;
                    true
                })
                // A sign right after the exponent.
                || ((c == b'-' || c == b'+') && k > 0 && (b[k - 1] | 0x20) == b'e');
            if !ok {
                break;
            }
            end = k + 1;
        }
        // Like `from_chars`, take the longest prefix that is a number.
        let mut take = end;
        let parsed = loop {
            if take == 0 {
                return Err(ExprError::Number(s.to_owned()));
            }
            if let Ok(v) = body[..take].parse::<f64>() {
                break v;
            }
            take -= 1;
        };
        (parsed, pos + take)
    };
    Ok((if negative { -value } else { value }, used))
}

#[cfg(test)]
mod tests {
    use super::Expr;

    fn eval(s: &str) -> f64 {
        Expr::new().eval(s).unwrap()
    }

    /// Intent: the cases of Cemu's `ExpressionParser_test` give the same results.
    /// Pass: every listed expression evaluates to Cemu's value.
    #[test]
    fn matches_cemus_self_test() {
        assert_eq!(eval("0x100005E0"), f64::from(0x1000_05E0_u32));
        assert_eq!(eval("0x10+0x20+0x40"), 112.0);
        assert_eq!(eval("0+-3"), -3.0);
        assert_eq!(eval("0x0-3"), -3.0);
        assert_eq!(eval("01C+0x10"), 44.0);
        assert_eq!(eval("+0x10"), 16.0);
        assert_eq!(eval("01C++0x10"), 44.0);
        assert_eq!(eval("01C+(+0x10)"), 44.0);
        assert_eq!(eval("(62156250 / 1000) * 30").trunc(), 1_864_687.0);
        assert_eq!(Expr::integer().eval("(62156250 / 1000) * 30").unwrap(), 1_864_680.0);
        assert_eq!(eval("5 > 3"), 1.0);
        assert_eq!(eval("5 < -10"), 0.0);
        assert_eq!(eval("5 <= 4.999"), 0.0);
        assert_eq!(eval("5 > 4 > 3 > 2"), 0.0);
        assert_eq!(eval("5 > 4 > 3 > -2"), 1.0);
        assert_eq!(eval("(5 == 5) > (5 == 6)"), 1.0);
        let mut e = Expr::new();
        e.add_constant("test", 5.0);
        assert_eq!(e.eval("test+15@lo").unwrap(), 20.0);
        assert_eq!(e.eval("test+15@hi").unwrap(), 0.0);
        assert!(Expr::is_valid("test@ha + test@ha"));
        assert!(!Expr::is_valid("test@ha + test@lo"));
    }

    /// Intent: preset variables in rules.txt evaluate as Cemu does.
    /// Pass: the XCX resolution formula gives 1920 at 1.5x, and @ha rounds up.
    #[test]
    fn rules_style_expressions() {
        let mut e = Expr::new();
        e.add_constant("$width", 1920.0);
        e.add_constant("$gameWidth", 1280.0);
        assert_eq!(e.eval("($width/$gameWidth)*1280").unwrap(), 1920.0);
        assert_eq!(e.eval("0x12348000@ha").unwrap(), f64::from(0x1235_u32));
        assert!(e.eval("$missing").is_err());
        assert_eq!(eval("2^3^2"), 512.0);
        assert_eq!(eval("1.5e2"), 150.0);
    }
}
