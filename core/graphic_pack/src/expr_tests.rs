//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ cargo test         │   │ expr_tests.rs            │   │ pass / fail        │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ tests of expr.rs         │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Unit tests for expr.rs (expression parser). They run with `cargo test` only and are not part
//! of the emulator you play with. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

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
