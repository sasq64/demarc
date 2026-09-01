use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

const SYSTEM_ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/system.zip"));

/// SHA-256 of `system.zip`, computed by `build.rs` when the archive is packed.
const SYSTEM_CHECKSUM: &str = env!("SYSTEM_ZIP_CHECKSUM");

pub fn system_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let system = resolve_system_dir();
        let system = system
            .canonicalize()
            .unwrap_or_else(|e| panic!("Failed to canonicalize system dir {system:?}: {e}"));
        // Cores get this path (and everything derived from it) as their libretro
        // system/save directory, so it has to be one a C library can open — not
        // the `\\?\` form canonicalize() hands back on Windows.
        crate::utils::strip_verbatim_prefix(&system)
    })
    .as_path()
}

/// Locate the `system` directory, preferring a local one in debug builds and
/// otherwise extracting the embedded `system.zip` into the user cache.
fn resolve_system_dir() -> PathBuf {
    if cfg!(debug_assertions) {
        let local = PathBuf::from("system");
        if local.is_dir() {
            debug!("Using local system dir");
            return local;
        }
        warn!("Could not find local system dir");
    }
    let cache = dirs::cache_dir().unwrap_or_default().join("demarc");
    info!("CACHE {cache:?}");
    let system = cache.join("system");
    let checksum_file = system.join(".checksum");
    let up_to_date = std::fs::read_to_string(&checksum_file)
        .map(|c| c.trim() == SYSTEM_CHECKSUM)
        .unwrap_or(false);
    if !up_to_date {
        std::fs::create_dir_all(&cache).expect("Failed to create demarc cache directory");
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(SYSTEM_ZIP))
            .expect("Failed to read embedded system.zip");
        archive
            .extract(&cache)
            .expect("Failed to extract system.zip");
        std::fs::write(&checksum_file, SYSTEM_CHECKSUM).expect("Failed to write system checksum");
    }
    system
}
