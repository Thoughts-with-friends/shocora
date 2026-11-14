//! Lists the system functions a title imports that shocora does not implement.
//!
//! ```text
//! cargo run -p shocora_tests --example import_audit -- FILE.rpx [...]
//! ```
//!
//! Without arguments every title in `shocora.toml` is audited. Output never names
//! a title, so it can be pasted into issues.
use std::{collections::BTreeMap, path::PathBuf};

use wiiu_cafe_os::CafeOs;
use wiiu_rpl::RplFile;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if files.is_empty() {
        files = shocora_tests::local_games().into_iter().map(|game| game.rpx_path).collect();
    }

    let registry = CafeOs::new().registry;

    for (index, path) in files.iter().enumerate() {
        let file = RplFile::parse(&std::fs::read(path)?)?;
        let mut libraries: BTreeMap<String, (u32, Vec<String>)> = BTreeMap::new();

        for (library, symbol, is_function) in file.imported_symbols()? {
            let entry = libraries.entry(library.clone()).or_default();

            if !is_function {
                continue;
            }
            entry.0 += 1;

            if registry.function_index(&library, &symbol).is_none() {
                entry.1.push(symbol);
            }
        }
        println!("# title {index}");

        for (library, (total, missing)) in &libraries {
            println!("{library}: {}/{total} implemented", total - missing.len() as u32);
            for symbol in missing {
                println!("  {symbol}");
            }
        }
    }
    Ok(())
}
