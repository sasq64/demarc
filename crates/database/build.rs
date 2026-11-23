//! Bakes the Demozoo API key into the binary, from `$DEMOZOO_API_KEY` or the
//! repo root's `demozoo.key`, xored so it does not show up in `strings`.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("DEMARC_ROOT").unwrap_or_else(|_| "../..".into()));
    let file = root.join("demozoo.key");
    println!("cargo:rerun-if-env-changed=DEMOZOO_API_KEY");
    println!("cargo:rerun-if-changed={}", file.display());
    let key = std::env::var("DEMOZOO_API_KEY")
        .ok()
        .or_else(|| std::fs::read_to_string(&file).ok())
        .unwrap_or_default();
    let bytes: Vec<String> = key
        .trim()
        .bytes()
        .enumerate()
        .map(|(i, b)| (b ^ 0x5a ^ (i as u8).wrapping_mul(31)).to_string())
        .collect();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("demozoo_key.rs");
    std::fs::write(
        out,
        format!("const KEY_XOR: &[u8] = &[{}];\n", bytes.join(",")),
    )
    .unwrap();
}
