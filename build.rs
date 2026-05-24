fn main() {
    cc::Build::new()
        .file("src/retro_log_shim.c")
        .compile("retro_log_shim");
    println!("cargo:rerun-if-changed=src/retro_log_shim.c");
}
