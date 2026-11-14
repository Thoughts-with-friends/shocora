//! Prints the sections, file info and imports of an RPX/RPL.
//!
//! ```text
//! cargo run -p wiiu_rpl --example rpl_info -- FILE.rpx
//! ```
use wiiu_rpl::RplFile;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or("usage: rpl_info FILE.rpx")?;
    let file = RplFile::parse(&std::fs::read(path)?)?;

    println!("entry {:#010x}  {:x?}", file.header.entry, file.file_info);
    println!(
        "{:>3} {:<28} {:>10} {:>10} {:>10} {:>10}",
        "#", "name", "type", "flags", "address", "size"
    );

    for s in &file.sections {
        println!(
            "{:>3} {:<28} {:>10x} {:>10x} {:>10x} {:>10x}",
            s.index,
            s.name,
            s.header.kind,
            s.header.flags,
            s.header.address,
            s.memory_size()
        );
    }
    println!("imports: {:?}", file.imported_libraries());

    if std::env::args_os().nth(2).is_some_and(|a| a == "--symbols") {
        for (address, name) in file.code_symbols()? {
            println!("{address:#010x} {name}");
        }
    }

    Ok(())
}
