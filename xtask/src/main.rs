//! `cargo xtask <build|run|test> [--release] [--toolchain gnu|msvc|<name>]
//!     [--graphics wgpu|vulkan|opengl] [-- ARGS...]`
//! `cargo xtask icon`: makes the PNG and ICO icons from `gui/egui/assets/shocora.svg`.
//!
//! Thin dispatcher so the common invocations are one short command.
//!
//! `--toolchain` picks the Rust toolchain through `rustup run`: `gnu` and `msvc` stand for
//! `stable-x86_64-pc-windows-<abi>`. Without it, the toolchain that runs xtask builds.
//! GNU builds go to `target/gnu` so that the two do not rebuild over each other, and GNU
//! release builds carry no debug info (it would be embedded in the exe, not in a .pdb).
//! xtask itself is linked by the toolchain that starts it, so with no MSVC linker installed:
//! `cargo +stable-x86_64-pc-windows-gnu xtask build`.
mod icon;

use std::process::{Command, ExitCode};

const USAGE: &str = "usage: cargo xtask <build|run|test> [--release] [--toolchain gnu|msvc|<name>]
           [--graphics wgpu|vulkan|opengl] [-- ARGS...]
       cargo xtask icon";

/// Rustup toolchain for a `--toolchain` value.
fn toolchain_name(value: &str) -> String {
    match value {
        "gnu" | "msvc" => format!("stable-x86_64-pc-windows-{value}"),
        other => other.to_owned(),
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(task) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    if task == "icon" {
        return match icon::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }

    let mut release = false;
    let mut toolchain = None;
    let mut passthrough = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--release" => release = true,
            "--toolchain" | "-t" => {
                let Some(value) = args.next() else {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                };
                toolchain = Some(toolchain_name(&value));
            }
            "--graphics" | "-g" => {
                passthrough.push("--graphics".to_owned());
                passthrough.extend(args.next());
            }
            "--" => passthrough.extend(args.by_ref()),
            other => passthrough.push(other.to_owned()),
        }
    }

    let mut command = if let Some(toolchain) = &toolchain {
        let mut command = Command::new("rustup");
        command.args(["run", toolchain, "cargo"]);
        command
    } else {
        Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
    };
    // The toolchain asked for, else the one rustup started xtask with.
    let effective = toolchain.or_else(|| std::env::var("RUSTUP_TOOLCHAIN").ok());
    match task.as_str() {
        "build" => command.args(["build", "-p", "shocora"]),
        "run" => command.args(["run", "-p", "shocora"]),
        "test" => command.args(["test", "--workspace"]),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if release {
        command.arg("--release");
    }
    let gnu = effective.as_deref().is_some_and(|name| name.contains("-gnu"));
    if gnu && std::env::var_os("CARGO_TARGET_DIR").is_none() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        command.arg("--target-dir").arg(workspace.join("target/gnu"));
    }
    // MSVC puts debug info in a separate .pdb; GNU embeds it as DWARF, which takes a
    // release exe from about 36 MB past 400 MB. Symbols stay for backtraces.
    if gnu && release {
        command.args(["--config", "profile.release.debug=false"]);
        command.args(["--config", "profile.release.strip=\"debuginfo\""]);
    }
    if task == "run" && !passthrough.is_empty() {
        command.arg("--").args(&passthrough);
    }

    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("failed to run cargo: {error}");
            ExitCode::FAILURE
        }
    }
}
