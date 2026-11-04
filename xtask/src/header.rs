//! `gen-header`: regenerates `crates/bindings/ffi/include/coblet.h` with cbindgen.

use std::path::Path;

/// Crate directory of the C ABI, relative to the workspace root.
const FFI_CRATE: &str = "crates/bindings/ffi";
/// Header path relative to the crate directory.
const HEADER: &str = "include/coblet.h";

fn generate(crate_dir: &Path) -> Result<Vec<u8>, String> {
    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml"))
        .map_err(|error| format!("cannot read cbindgen.toml: {error}"))?;
    let bindings = cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(config)
        .generate()
        .map_err(|error| format!("cbindgen failed: {error}"))?;
    let mut out = Vec::new();
    bindings.write(&mut out);
    Ok(out)
}

/// Writes the header, or with `check` reports whether the committed one is current.
pub(crate) fn run(root: &Path, check: bool) -> Result<(), String> {
    let crate_dir = root.join(FFI_CRATE);
    let header = crate_dir.join(HEADER);
    let generated = generate(&crate_dir)?;
    if check {
        let committed = std::fs::read(&header)
            .map_err(|error| format!("cannot read {}: {error}", header.display()))?;
        if committed != generated {
            return Err(format!(
                "{} is out of date; run `cargo xtask gen-header`",
                header.display()
            ));
        }
        println!("gen-header: {HEADER} is current");
        return Ok(());
    }
    std::fs::write(&header, generated)
        .map_err(|error| format!("cannot write {}: {error}", header.display()))?;
    println!("gen-header: wrote {}", header.display());
    Ok(())
}
