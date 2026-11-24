//! What amiberry needs on disk: the Kickstart ROMs, out of an installed Amiga
//! Forever, and the drive skeletons the release is booted from.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use tracing::info;

/// Amiga Forever Essentials, the Android app holding the ROMs.
pub const FOREVER_PACKAGE: &str = "com.cloanto.amigaforever.essentials";

const ROM_DIR: &str = "assets/files/rom/";

pub struct Rom {
    /// Cloanto's file.
    cloanto: &'static str,
    /// The name amiberry looks for.
    name: &'static str,
    /// The 1.x ROMs are stored twice over to fill 512K.
    size: usize,
    pub version: &'static str,
    /// As in amiberry's `src/rommgr.cpp`.
    crcs: &'static [u32],
}

const ROMS: [Rom; 4] = [
    Rom {
        cloanto: "amiga-os-120.rom",
        name: "kick33180.A500",
        size: 0x40000,
        version: "1.2",
        crcs: &[0xa6ce1636],
    },
    Rom {
        cloanto: "amiga-os-130.rom",
        name: "kick34005.A500",
        size: 0x40000,
        version: "1.3",
        // Amiga Forever's is the A3000 one.
        crcs: &[0xc4f0f55f, 0xe0f37258],
    },
    Rom {
        cloanto: "amiga-os-204.rom",
        name: "kick37175.A500",
        size: 0x80000,
        version: "2.04",
        crcs: &[0xc3bdb240],
    },
    Rom {
        cloanto: "amiga-os-310-a1200.rom",
        name: "kick40068.A1200",
        size: 0x80000,
        version: "3.1",
        crcs: &[0x1483a091],
    },
];

static USER_ROM_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Where the user can copy Kickstarts of their own.
pub fn set_user_rom_dir(dir: PathBuf) {
    let _ = USER_ROM_DIR.set(dir);
}

pub fn user_rom_dir() -> PathBuf {
    USER_ROM_DIR
        .get()
        .cloned()
        .unwrap_or_else(release_io::amiga::amiga_system_dir)
}

/// The CRC32 of a ROM, of one half when it is a 1.x ROM stored twice over.
fn rom_crc(data: &[u8]) -> u32 {
    let (a, b) = data.split_at(data.len() / 2);
    crc32fast::hash(if data.len() == 0x80000 && a == b {
        a
    } else {
        data
    })
}

fn file_crc(path: &Path) -> Option<u32> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 0x80000 {
        return None;
    }
    std::fs::read(path).ok().map(|data| rom_crc(&data))
}

fn missing_in<'a>(dir: &Path, roms: &'a [Rom]) -> Vec<&'a Rom> {
    roms.iter()
        .filter(|rom| !file_crc(&dir.join(rom.name)).is_some_and(|crc| rom.crcs.contains(&crc)))
        .collect()
}

/// The Kickstarts in `dir` that are absent or not the right ROM.
pub fn missing_kickstarts(dir: &Path) -> Vec<&'static Rom> {
    missing_in(dir, &ROMS)
}

fn import_in(from: &Path, dest: &Path, roms: &[Rom]) -> Result<usize> {
    let mut missing = missing_in(dest, roms);
    if missing.is_empty() {
        return Ok(0);
    }
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(0);
    };
    let mut written = 0;
    let mut install = |data: &[u8], source: &Path| -> Result<()> {
        let crc = rom_crc(data);
        if let Some(i) = missing.iter().position(|rom| rom.crcs.contains(&crc)) {
            let rom = missing.remove(i);
            std::fs::create_dir_all(dest)?;
            std::fs::write(dest.join(rom.name), data)?;
            info!("Installed {} from {source:?}", rom.name);
            written += 1;
        }
        Ok(())
    };
    for path in entries.flatten().map(|e| e.path()) {
        if let Ok(file) = std::fs::File::open(&path)
            && let Ok(mut zip) = zip::ZipArchive::new(file)
        {
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i)?;
                if entry.is_file() && entry.size() <= 0x80000 {
                    let mut data = vec![];
                    entry.read_to_end(&mut data)?;
                    install(&data, &path.join(entry.name()))?;
                }
            }
            continue;
        }
        if std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= 0x80000) {
            install(&std::fs::read(&path)?, &path)?;
        }
    }
    Ok(written)
}

/// Copy the Kickstarts `dest` lacks out of `from`, whatever they are called
/// there. Returns how many were copied.
pub fn import_kickstarts(from: &Path, dest: &Path) -> Result<usize> {
    import_in(from, dest, &ROMS)
}

const CLOANTO_HEADER: &[u8] = b"AMIROMTYPE1";

/// A Cloanto ROM is its header and then the ROM xored with `rom.key`.
fn decrypt(data: &[u8], key: &[u8], size: usize) -> Option<Vec<u8>> {
    let data = data.strip_prefix(CLOANTO_HEADER)?;
    if key.is_empty() || data.len() < size {
        return None;
    }
    Some(
        data[..size]
            .iter()
            .zip(key.iter().cycle())
            .map(|(b, k)| b ^ k)
            .collect(),
    )
}

/// Write the ROMs out of Amiga Forever's APK at `apk` into `dest`, those
/// that are not there already. Returns how many were written.
pub fn install_kickstarts(apk: &Path, dest: &Path) -> Result<usize> {
    if ROMS.iter().all(|rom| dest.join(rom.name).is_file()) {
        return Ok(0);
    }
    let file = std::fs::File::open(apk).with_context(|| format!("Can't open {apk:?}"))?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut read = |name: &str| -> Result<Vec<u8>> {
        let mut data = vec![];
        zip.by_name(&format!("{ROM_DIR}{name}"))?
            .read_to_end(&mut data)?;
        Ok(data)
    };
    let key = read("rom.key")?;
    std::fs::create_dir_all(dest)?;
    let mut written = 0;
    for Rom {
        cloanto,
        name,
        size,
        ..
    } in ROMS
    {
        let rom = decrypt(&read(cloanto)?, &key, size)
            .with_context(|| format!("{cloanto} is not a Cloanto ROM"))?;
        std::fs::write(dest.join(name), rom)?;
        written += 1;
    }
    info!("Installed {written} Kickstarts from {apk:?}");
    Ok(written)
}

/// Unpack the zip at `zip` into `dest`, over what is there.
pub fn unpack_zip(zip: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(zip).with_context(|| format!("Can't open {zip:?}"))?;
    zip::ZipArchive::new(file)?.extract(dest)?;
    Ok(())
}

/// Where Android installed `package`, if it is.
#[cfg(target_os = "android")]
pub fn package_path(
    app: &winit::platform::android::activity::AndroidApp,
    package: &str,
) -> Option<std::path::PathBuf> {
    use jni::JavaVM;
    use jni::objects::{JObject, JString};

    let result = (|| -> jni::errors::Result<Option<String>> {
        // SAFETY: both pointers stay valid for as long as the activity.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast())? };
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let mut env = vm.attach_current_thread()?;
        let package = env.new_string(package)?;
        let call = env.call_method(
            &activity,
            "packagePath",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[(&package).into()],
        );
        if call.is_err() {
            let _ = env.exception_clear();
        }
        let path = JString::from(call?.l()?);
        if path.is_null() {
            return Ok(None);
        }
        Ok(Some(env.get_string(&path)?.into()))
    })();
    result.ok().flatten().map(Into::into)
}

static PICKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the user has picked files since the last call.
pub fn take_picked() -> bool {
    PICKED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// `MiniActivity.nativeKickstartsPicked()`, once the picked files are copied.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_demarc_c64_MiniActivity_nativeKickstartsPicked(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) {
    PICKED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Let the user pick Kickstart files or zips to copy into [`user_rom_dir`].
#[cfg(target_os = "android")]
pub fn pick_kickstarts() {
    use jni::JavaVM;
    use jni::objects::JObject;

    let Some(app) = crate::web::app() else {
        return;
    };
    let result = (|| -> jni::errors::Result<()> {
        // SAFETY: both pointers stay valid for as long as the activity.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast())? };
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let mut env = vm.attach_current_thread()?;
        let dest = env.new_string(user_rom_dir().to_string_lossy())?;
        let call = env.call_method(
            &activity,
            "pickKickstarts",
            "(Ljava/lang/String;)V",
            &[(&dest).into()],
        );
        if call.is_err() {
            let _ = env.exception_clear();
        }
        call.map(|_| ())
    })();
    if let Err(e) = result {
        tracing::warn!("Can't pick Kickstarts: {e}");
    }
}

#[cfg(test)]
#[path = "tests/amiga_tests.rs"]
mod tests;
