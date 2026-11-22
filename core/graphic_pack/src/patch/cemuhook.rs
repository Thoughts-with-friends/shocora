//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!               ▲▲▲▲▲▲    ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ patch_*.asm files  │   │ patch/cemuhook.rs        │   │ changed game code  │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ code patches: cemuhook   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "cemuhook" share of mod.rs (code patches), moved here so that no file is longer than 500
//! lines. It works exactly as if it were still written in that file. Packs can patch the game's
//! own machine code (for example FPS++). This reads those patch files. Terms such as HLE, JIT
//! or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

/// Reads a Cemuhook `patches.txt`.
///
/// # Errors
/// The first syntax or assembler error.
pub fn parse_cemuhook(text: &str) -> Result<Vec<PatchGroup>, PatchError> {
    let mut groups = Vec::new();
    let mut current: Option<PatchGroup> = None;
    for (line_no, raw) in lines(text) {
        let mut tokens = Tokens { s: without_comment(raw) };
        tokens.skip_ws();
        if tokens.is_end() {
            continue;
        }
        if let Some(name) = group_header(line_no, &tokens)? {
            if let Some(group) = current.take() {
                finish_group(group, &mut groups)?;
            }
            current = Some(PatchGroup::new(&name));
            continue;
        }
        if tokens.at(0) == Some(b'0') && matches!(tokens.at(1), Some(b'x' | b'X')) {
            let Some(address) = tokens.parse_u32() else {
                return err(line_no, "Malformed address");
            };
            if !tokens.match_word("=") {
                return err(line_no, "Expected '=' after address");
            }
            tokens.skip_ws();
            tokens.trim_ws();
            let out = asm::assemble(tokens.s, address, false).map_err(|e| PatchError {
                line: line_no,
                message: format!("Error in assembler: {e}"),
            })?;
            let Some(group) = current.as_mut() else {
                return err(line_no, "Patch line outside of a group");
            };
            group.entries.push(Entry::Instruction {
                line: line_no,
                address,
                data: out.data,
                relocs: out.relocs,
            });
            continue;
        }
        if tokens.match_word("moduleMatches") {
            module_matches(line_no, &mut tokens, current.as_mut(), false)?;
            continue;
        }
        let Some(group) = current.as_mut() else {
            return err(line_no, "Invalid syntax");
        };
        if let Some(name) = tokens.parse_symbol()
            && tokens.match_word("=")
        {
            tokens.skip_ws();
            tokens.trim_ws();
            group.entries.push(Entry::CemuhookSymbol {
                line: line_no,
                name: name.to_owned(),
                expr: tokens.s.to_owned(),
            });
            continue;
        }
        return err(line_no, "Invalid syntax");
    }
    if let Some(group) = current {
        finish_group(group, &mut groups)?;
    }
    Ok(groups)
}

/// A pack's patch files read (Cemu's `LoadPatchFiles`): its `patch_*.asm` files, or
/// `patches.txt` when it has none. `Ok(empty)` without either.
///
/// # Errors
/// The file and line of the first error; none of the pack's patches apply then.
pub fn load(dir: &Path) -> Result<Vec<PatchGroup>, String> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
        .unwrap_or_default();
    files.sort();
    let mut groups = Vec::new();
    let mut found = false;
    for path in &files {
        let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase());
        let Some(name) = name else { continue };
        if !(name.starts_with("patch_") && name.ends_with(".asm")) {
            continue;
        }
        found = true;
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let parsed = parse_cemu(&String::from_utf8_lossy(&text))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        groups.extend(parsed);
    }
    if found {
        return Ok(groups);
    }
    let path = dir.join("patches.txt");
    let Ok(text) = std::fs::read(&path) else { return Ok(Vec::new()) };
    parse_cemuhook(&String::from_utf8_lossy(&text)).map_err(|e| format!("{}: {e}", path.display()))
}
