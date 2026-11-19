//! What puae needs on disk: the Kickstart ROMs, out of an installed Amiga
//! Forever, and the drive skeletons the release is booted from.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

/// Amiga Forever Essentials, the Android app holding the ROMs.
pub const FOREVER_PACKAGE: &str = "com.cloanto.amigaforever.essentials";

const ROM_DIR: &str = "assets/files/rom/";

/// Cloanto's file, the name puae looks for, and the ROM's size: the 1.x ROMs
/// are stored twice over to fill 512K.
const ROMS: [(&str, &str, usize); 4] = [
    ("amiga-os-120.rom", "kick33180.A500", 0x40000),
    ("amiga-os-130.rom", "kick34005.A500", 0x40000),
    ("amiga-os-204.rom", "kick37175.A500", 0x80000),
    ("amiga-os-310-a1200.rom", "kick40068.A1200", 0x80000),
];

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
    if ROMS.iter().all(|(_, name, _)| dest.join(name).is_file()) {
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
    for (src, name, size) in ROMS {
        let rom = decrypt(&read(src)?, &key, size)
            .with_context(|| format!("{src} is not a Cloanto ROM"))?;
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

#[cfg(test)]
#[path = "tests/amiga_tests.rs"]
mod tests;
