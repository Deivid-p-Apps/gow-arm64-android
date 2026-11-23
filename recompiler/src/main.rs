mod lift;
mod pe;

use anyhow::{Context, Result};
use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let exe = args.next().context("usage: gowrecomp <game.exe> [out_dir]")?;
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| "generated".to_string()));

    eprintln!("[pe] parsing {exe}");
    let bytes = std::fs::read(&exe).with_context(|| format!("read {exe}"))?;
    let pe = pe::PeImage::parse(bytes)?;

    eprintln!(
        "[pe] image_base=0x{:x} entry=0x{:x} sections={} imports={} exports={} pdata_funcs={}",
        pe.image_base, pe.entry_rva, pe.sections.len(), pe.imports.len(),
        pe.exports.len(), pe.pdata_functions.len()
    );

    // Emit the import/section manifest for the runtime.
    std::fs::create_dir_all(&out_dir)?;
    let mut manifest = std::fs::File::create(out_dir.join("manifest.json"))?;
    writeln!(manifest, "{{")?;
    writeln!(manifest, "  \"image_base\": {},", pe.image_base)?;
    writeln!(manifest, "  \"entry_rva\": {},", pe.entry_rva)?;
    writeln!(manifest, "  \"size_of_image\": {},", pe.size_of_image)?;
    writeln!(manifest, "  \"section_alignment\": {},", pe.section_alignment)?;
    writeln!(manifest, "  \"imports\": [")?;
    for (i, imp) in pe.imports.iter().enumerate() {
        let comma = if i + 1 == pe.imports.len() { "" } else { "," };
        writeln!(
            manifest,
            "    {{\"dll\":\"{}\",\"func\":\"{}\",\"iat_rva\":{}}}{comma}",
            imp.dll.replace('\\', "\\\\"),
            imp.func.replace('\\', "\\\\"),
            imp.iat_rva
        )?;
    }
    writeln!(manifest, "  ]")?;
    writeln!(manifest, "}}")?;

    eprintln!("[lift] lifting {} functions", pe.pdata_functions.len() + 1);
    let res = lift::lift(&pe, &out_dir)?;

    eprintln!(
        "[lift] done: {} functions, {} instructions emitted to {}",
        res.function_count,
        res.instruction_count,
        out_dir.join("gow_game.c").display()
    );
    if !res.unimpl.is_empty() {
        eprintln!("[lift] UNIMPLEMENTED mnemonics present ({} distinct):", res.unimpl.len());
        for m in res.unimpl.iter().take(200) {
            eprintln!("       - {m}");
        }
    }
    Ok(())
}
