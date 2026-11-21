use std::collections::HashMap;

use super::*;

struct Host {
    next_cave: u32,
}

impl PatchHost for Host {
    fn alloc_code_cave(&mut self, size: u32) -> Option<u32> {
        let at = self.next_cave;
        self.next_cave += size.next_multiple_of(256);
        Some(at)
    }

    fn import(&mut self, module: &str, function: &str) -> Option<u32> {
        (module == "coreinit" && function == "OSReport").then_some(0x0181_0000)
    }
}

const PATCH: &str = r"
[Speed]
moduleMatches = 0x12345678, 0xABCDEF01

.origin = codecave
_speed:
.float $speed ; preset variable
newCode:
lis r3, _speed@ha
lfs f1, _speed@l(r3)
bl import.coreinit.OSReport
b _back

0x02000100 = b newCode
_back = 0x02000104
";

fn module() -> ModuleInfo {
    // Text linked at 0x02000000, placed at 0x02100000.
    ModuleInfo {
        crc: 0xABCD_EF01,
        is_rpx: true,
        sections: vec![(0x0200_0000, 0x1000, 0x0210_0000)],
    }
}

/// Intent: a Cemu patch with a code cave, a preset variable, an import, a label used
/// before and after its definition and a variable resolves as Cemu resolves it.
/// Pass: the cave holds the float and code with `@ha`/`@l` of its address; the hook at
/// 0x02000100 (moved with its section) branches into the cave; the cave branches back.
#[test]
fn resolves_a_code_cave_patch() {
    let groups = parse_cemu(PATCH).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].code_cave_size, 20);
    let constants = HashMap::from([("$speed".to_owned(), 1.5)]);
    let mut host = Host { next_cave: 0x0180_0000 };
    let applied = apply(&groups, &module(), &constants, &mut host).unwrap();
    let writes: HashMap<u32, Vec<u8>> = applied.writes.into_iter().collect();
    assert_eq!(writes[&0x0180_0000], 1.5_f32.to_be_bytes());
    // lis r3, 0x0180
    assert_eq!(writes[&0x0180_0004], 0x3C60_0180_u32.to_be_bytes());
    // lfs f1, 0(r3)
    assert_eq!(writes[&0x0180_0008], 0xC023_0000_u32.to_be_bytes());
    // bl 0x01810000 from 0x0180000C
    let bl = u32::from_be_bytes(writes[&0x0180_000C].clone().try_into().unwrap());
    assert_eq!(bl, 0x4800_0001 | ((0x0181_0000 - 0x0180_000C) & 0x03FF_FFFC));
    // b back to 0x02000104 from 0x01800010: a variable is an absolute address, as in Cemu
    let back = u32::from_be_bytes(writes[&0x0180_0010].clone().try_into().unwrap());
    assert_eq!(back, 0x4800_0000 | ((0x0200_0104 - 0x0180_0010) & 0x03FF_FFFC));
    // The hook, relocated into the placed section.
    let hook = u32::from_be_bytes(writes[&0x0210_0100].clone().try_into().unwrap());
    assert_eq!(hook, 0x4800_0000 | (0x0180_0004_u32.wrapping_sub(0x0210_0100) & 0x03FF_FFFC));
}

/// Intent: groups for other modules are left alone and missing symbols are reported.
/// Pass: a checksum mismatch applies nothing; an unknown name fails with its name.
#[test]
fn skips_other_modules_and_reports_unknown_symbols() {
    let groups = parse_cemu("[A]\nmoduleMatches = 0x1\n0x02000000 = nop\n").unwrap();
    let mut host = Host { next_cave: 0x0180_0000 };
    let applied = apply(&groups, &module(), &HashMap::new(), &mut host).unwrap();
    assert!(applied.writes.is_empty());

    let groups = parse_cemu("[B]\nmoduleMatches = rpx\n0x02000000 = b nowhere\n").unwrap();
    let errors = apply(&groups, &module(), &HashMap::new(), &mut host).unwrap_err();
    assert!(errors.iter().any(|e| e.contains("Unresolved symbol: nowhere")), "{errors:?}");
}

/// Intent: Cemuhook `patches.txt` symbols are module addresses.
/// Pass: the symbol relocates with the section and the patched word uses it.
#[test]
fn reads_cemuhook_patches() {
    let text = "[Hook]\nmoduleMatches = 0xABCDEF01\n_fn = 0x02000200\n0x02000010 = bl _fn\n";
    let groups = parse_cemuhook(text).unwrap();
    let mut host = Host { next_cave: 0x0180_0000 };
    let applied = apply(&groups, &module(), &HashMap::new(), &mut host).unwrap();
    assert_eq!(applied.writes, [(0x0210_0010, 0x4800_01F1_u32.to_be_bytes().to_vec())]);
}

/// Intent: errors name their line, as Cemu's log does.
/// Pass: an unknown instruction fails on line 3.
#[test]
fn syntax_errors_name_the_line() {
    let error = parse_cemu("[A]\nmoduleMatches = rpx\n0x02000000 = frobnicate r3\n").unwrap_err();
    assert_eq!(error.line, 3);
    assert!(error.message.contains("FROBNICATE"), "{error}");
}

/// Intent: every patch of a real graphic pack download parses (run with
/// `SHOCORA_GRAPHIC_PACK_DIR` set to a `graphicPacks` folder).
/// Pass: no pack fails; the failures are listed with file and line.
#[test]
#[ignore = "needs a graphic pack download"]
fn parses_downloaded_packs() {
    let Some(root) = std::env::var_os("SHOCORA_GRAPHIC_PACK_DIR") else { return };
    let packs = crate::load_all(std::path::Path::new(&root));
    let mut failures = Vec::new();
    let mut groups = 0;
    for pack in &packs {
        match load(pack.dir()) {
            Ok(parsed) => groups += parsed.len(),
            Err(error) => failures.push(error),
        }
    }
    eprintln!("{} packs, {groups} patch groups", packs.len());
    assert!(failures.is_empty(), "{} failed:\n{}", failures.len(), failures.join("\n"));
}
