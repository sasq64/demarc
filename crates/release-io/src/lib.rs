//! Everything between a release's URL and its files on disk: fetching into the
//! cache, unpacking, and finding what to boot. Nothing here runs a release, so
//! it builds wherever the libretro layer does (see `docs/ANDROID.md`).

pub mod c64;
pub mod cache;
pub mod cbmconvert;
pub mod fetch;
pub mod m3u;
pub mod release;
pub mod release_override;
pub mod song_name;
pub mod utils;
pub mod workfile;

pub use cache::set_cache_dir;
pub use release::{
    ARCHIVE_NAME, SFX, collect_disk_images, unpack_release, walk_dir, walk_dir_find,
};
pub use release_override::{Override, Patch, apply_override, apply_patches, leak};
pub use song_name::SongName;
