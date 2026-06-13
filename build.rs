use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

fn main() {
    cc::Build::new()
        .file("src/retro_log_shim.c")
        .compile("retro_log_shim");
    println!("cargo:rerun-if-changed=src/retro_log_shim.c");

    build_system_zip();
}

/// Marker files written into the extracted `system/` dir at runtime; they are
/// never packed into the archive itself.
const MARKER_FILES: &[&str] = &[".v4", ".checksum"];

/// Pack the loose `system/` directory into `system.zip` (embedded into the
/// binary via `include_bytes!`), then emit a SHA-256 of the resulting archive
/// as the `SYSTEM_ZIP_CHECKSUM` env var. The runtime writes that checksum next
/// to the extracted files and re-extracts whenever it no longer matches, which
/// replaces the old hand-maintained `.v4` marker.
fn build_system_zip() {
    // Watching the directory makes cargo re-run this script (re-zipping and
    // re-hashing) whenever anything under `system/` changes.
    println!("cargo:rerun-if-changed=system");

    let zip_path = Path::new("system.zip");
    let file = File::create(zip_path).expect("Failed to create system.zip");
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    // Collect entries first and sort them so the archive layout is stable
    // across builds.
    let mut entries = Vec::new();
    collect_entries(Path::new("system"), &mut entries);
    entries.sort();

    for path in &entries {
        // Paths inside the archive keep their `system/` prefix; `system_dir()`
        // in src/retro.rs extracts into the cache root, so the files land in
        // `<cache>/system/...`.
        let name = path.to_string_lossy().replace('\\', "/");
        if path.is_dir() {
            writer
                .add_directory(name, options)
                .expect("Failed to add directory to system.zip");
        } else {
            writer
                .start_file(name, options)
                .expect("Failed to start file in system.zip");
            let mut buf = Vec::new();
            File::open(path)
                .and_then(|mut f| f.read_to_end(&mut buf))
                .expect("Failed to read system file");
            writer
                .write_all(&buf)
                .expect("Failed to write file into system.zip");
        }
    }
    writer.finish().expect("Failed to finalize system.zip");

    let bytes = std::fs::read(zip_path).expect("Failed to read back system.zip");
    let digest = Sha256::digest(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=SYSTEM_ZIP_CHECKSUM={hex}");
}

/// Recursively gather every directory and (non-marker) file under `dir`.
fn collect_entries(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.push(path.clone());
            collect_entries(&path, out);
        } else if !MARKER_FILES.contains(&entry.file_name().to_string_lossy().as_ref()) {
            out.push(path);
        }
    }
}
