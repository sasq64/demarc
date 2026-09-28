fn main() {
    cc::Build::new()
        .file("src/c_shims/retro_log_shim.c")
        .compile("retro_log_shim");
    println!("cargo:rerun-if-changed=src/c_shims/retro_log_shim.c");
}
