use anyhow::{Result, bail};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, info, warn};

use super::utils::read_header;

use crate::{newsys::walk_dir, workfile::WorkFile};

use super::System;
use super::disc::{DiscImage, ISO_SECTOR, IsoSpec, build_iso, cue_data_tracks};

const CORE_NAME_PSX: &str = "pcsx_rearmed";
pub struct PSXSystem {}

impl PSXSystem {}

/// The name the executable is given on the disc. Both the BIOS and every core
/// fall back to this one when a disc has no `SYSTEM.CNF`, so it is also what
/// makes the image boot if the sheet is ever ignored.
const ISO_EXE_NAME: &str = "PSX.EXE";

/// Lay `exe` out as a bootable PlayStation disc image: a `SYSTEM.CNF` naming the
/// boot file and the executable itself, in a filesystem with nothing else in it.
///
/// The console's boot path is what decides the shape here. It reads the volume
/// descriptor at sector 16, follows the root directory record inside it, looks
/// for `SYSTEM.CNF`, takes the `BOOT = cdrom:\…` line from it, and loads that
/// file as a PS-X EXE — the file's first sector being the executable's own
/// 0x800-byte header, which is why the executable goes on the disc unaltered.
/// Cores that HLE the BIOS (pcsx_rearmed) walk the same structures themselves.
fn build_psx_iso(exe: &[u8]) -> Vec<u8> {
    // `BOOT` is the only line a core reads; the rest is what a real disc carries
    // and what the console's own BIOS would set up from.
    const CNF: &str = "BOOT = cdrom:\\PSX.EXE;1\r\nTCB = 4\r\nEVENT = 10\r\nSTACK = 801FFFF0\r\n";

    let files = [
        ("SYSTEM.CNF".to_string(), CNF.as_bytes().to_vec()),
        (ISO_EXE_NAME.to_string(), exe.to_vec()),
    ];
    let mut spec = IsoSpec {
        // A pressed PlayStation disc says exactly this, and some tools identify
        // one by it. Nothing refuses to boot without it, but it costs 11 bytes.
        system_id: "PLAYSTATION",
        volume_id: "DEMARC",
        files: &files,
        ..Default::default()
    };
    let image = build_iso(&spec);
    // pcsx_rearmed tells a 2048-byte-sector image from a raw 2352-byte one by
    // the file's length alone, so a length that divides evenly by both would be
    // read as the wrong kind of disc — which only happens at a multiple of 147
    // sectors. An extra empty sector steps past it.
    if (image.len() / ISO_SECTOR).is_multiple_of(147) {
        spec.pad_sectors = 1;
        return build_iso(&spec);
    }
    image
}

/// Wrap the PlayStation executable at `exe_path` in a bootable disc image, so it
/// can be handed to pcsx_rearmed, which loads discs but refuses a raw PS-X EXE.
/// Beetle takes the executable directly but needs a real BIOS, which is the
/// thing this avoids having to ask for.
///
/// The image is cached under its contents, since the file it came from is often
/// unpacked to a fresh temp directory on every launch — see [`prepare_psx_disc`],
/// which keys its cache the same way and for the same reason.
///
/// Returns `None` when `exe_path` isn't a PlayStation executable at all.
pub fn create_psx_iso(exe_path: &Path) -> Result<Option<PathBuf>> {
    use std::hash::{Hash, Hasher};

    let mut exe = fs::read(exe_path)?;
    if exe.len() <= PSX_HEADER_LEN as usize || exe[0..8] != *b"PS-X EXE" {
        return Ok(None);
    }
    // The header's text size has to describe what follows it exactly, and the
    // disc is read straight through from the executable's first sector, so a
    // size that overruns the file would pull in whatever sectors come after it.
    // Same repair the raw-executable path makes, applied to our copy — the file
    // we were handed isn't ours to write to.
    if let Some(size) = psx_text_size_fix(exe_path) {
        debug!("Recording a {size:#x} byte text section for {exe_path:?}");
        exe[PSX_TEXT_SIZE_OFFSET..PSX_TEXT_SIZE_OFFSET + 4].copy_from_slice(&size.to_le_bytes());
        exe.truncate(PSX_HEADER_LEN as usize + size as usize);
    }

    let mut key = std::collections::hash_map::DefaultHasher::new();
    exe.hash(&mut key);
    let stem = exe_path.file_stem().unwrap_or_default().to_string_lossy();
    let out_dir = dirs::cache_dir()
        .unwrap_or_default()
        .join("demarc")
        .join("psxexe");
    let out = out_dir.join(format!("{stem}-{:016x}.iso", key.finish()));
    if out.is_file() {
        debug!("Using cached disc image {out:?}");
        return Ok(Some(out));
    }

    info!("Building bootable disc image for {exe_path:?}");
    fs::create_dir_all(&out_dir)?;
    // Write beside the target and rename, so a second demarc looking at the
    // cache never finds a half-written image under a name that says it is done.
    let partial = out.with_extension("iso.part");
    fs::write(&partial, build_psx_iso(&exe))?;
    fs::rename(&partial, &out)?;
    Ok(Some(out))
}

pub fn is_psx_exe(path: &Path) -> bool {
    read_header(path, 8).is_ok_and(|h| h == b"PS-X EXE")
}

/// A pressed PlayStation disc keeps Sony's licence text in the system area, the
/// 16 sectors ahead of the filesystem. It is the one marker that needs no
/// filesystem at all — but scene images often have it stripped, since a rip of
/// just the data track tends to zero it, so its absence proves nothing.
const PSX_LICENCE: &[u8] = b"Sony Computer Entertainment";

/// Whether `path` is a PlayStation disc image — the data track of a cue/bin, an
/// `.iso`, or the same thing under any other name.
///
/// Either of two markers is enough. Sony's licence text in the system area
/// needs no filesystem, but only survives on a full raw dump. What every disc
/// the console boots has is `SYSTEM.CNF` in the root — or, when the boot file is
/// left at its default name, `PSX.EXE` — and that is also what separates a
/// PlayStation disc from a Saturn or PC Engine one arriving in the same
/// MODE2/2352 wrapper.
pub fn is_psx_disc(path: &Path) -> bool {
    let Some(mut disc) = DiscImage::open(path) else {
        return false;
    };
    if disc.system_area_has(PSX_LICENCE) {
        return true;
    }
    disc.root_names()
        .iter()
        .any(|name| name == "SYSTEM.CNF" || name == ISO_EXE_NAME)
}

/// Whether the cue sheet at `path` describes a PlayStation disc, judged by the
/// data track it names. A sheet holding nothing but audio tracks is a CD, and
/// one whose data track belongs to another console isn't ours either.
pub fn is_psx_cue(path: &Path) -> bool {
    cue_data_tracks(path).iter().any(|track| is_psx_disc(track))
}

/// The fixed size of a PSX executable's header. Everything after it is the
/// text section.
const PSX_HEADER_LEN: u64 = 0x800;

/// Offset of the text section's size (`t_size`) in that header, right after the
/// address it loads to (`t_addr`) at `0x18`.
const PSX_TEXT_SIZE_OFFSET: usize = 0x1c;

/// The PlayStation's 2MB of main RAM, which the text section has to fit into.
const PSX_RAM_SIZE: u32 = 0x20_0000;

/// The text size the PSX executable at `path` should be recording, or `None`
/// when it isn't one, or already records exactly that.
///
/// The core takes only one number: `t_size` has to equal the data that follows
/// the header, byte for byte, or it refuses to load the file at all ("Text
/// section recorded size is smaller/larger than data available in file"). Scene
/// releases keep running into the small side of that — the demo's data was
/// appended after the code without the header being updated to match — so the
/// fix is to record the size the file really has.
///
/// What the section can't do is run off the end of RAM. The copy is mirrored
/// the way the hardware maps it, so the overflow lands back at address zero, on
/// top of the kernel, and the demo dies there. A file holding more than fits
/// from its load address is cut down to what does by [`fix_psx_text_size`],
/// which is also what keeps this within the 2MB the core will accept at all.
fn psx_text_size_fix(path: &Path) -> Option<u32> {
    let header = read_header(path, PSX_TEXT_SIZE_OFFSET + 4).ok()?;
    if header.len() < PSX_TEXT_SIZE_OFFSET + 4 || header[0..8] != *b"PS-X EXE" {
        return None;
    }
    let field = |off: usize| u32::from_le_bytes(header[off..off + 4].try_into().unwrap());

    let available = fs::metadata(path)
        .ok()?
        .len()
        .saturating_sub(PSX_HEADER_LEN);
    // RAM is mirrored, so only the load address' offset within it says how much
    // room the text section has before it runs off the end.
    let room = PSX_RAM_SIZE - (field(0x18) & (PSX_RAM_SIZE - 1));
    let size = u32::try_from(available).unwrap_or(u32::MAX).min(room);

    // A file with nothing after its header is broken in a way no size helps
    // with; leave it to fail with the core's own complaint.
    (size > 0 && size != field(PSX_TEXT_SIZE_OFFSET)).then_some(size)
}

fn handle_exe(work_file: &Path) -> Result<PathBuf> {
    // Only Beetle loads a raw PS-X EXE, and only with a real BIOS. Wrap
    // the executable in a bootable disc image instead and the default
    // core takes it, HLE BIOS and all. The image lives in the cache and
    // is reused between runs, so it deliberately isn't the `temp_dir`.

    debug!("Building iso");
    let disc = create_psx_iso(work_file).unwrap_or_else(|err| {
        warn!("Could not build a disc image for {work_file:?}: {err}");
        None
    });
    if let Some(disc) = disc {
        debug!("FMT: PSX executable wrapped in disc image {disc:?}");
        return Ok(disc);
    } else {
        //work_file.meta.insert("psx_core".into(), "beetle".into());
        // A scene exe whose header undercounts its text section doesn't
        // load at all, so patch the header before the core sees it. The
        // file we were handed isn't ours to write to unless it already
        // came out of a temp directory, so anything else is copied first.
        // if psx_needs_text_fix(&work_file) {
        //     if temp_dir.is_none() {
        //         // TODO: temp file leek? Better to wire this in convert_dir()
        //         let dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
        //         let copy = dir.path().join(path.file_name().unwrap());
        //         fs::copy(&path, &copy)?;
        //         path = copy;
        //         temp_dir = Some(dir);
        //     }
        //     debug!("FMT: patching short PSX text section in {path:?}");
        //     fix_psx_text_size(&path)?;
        //}
    }
    bail!("Could not load EXE");
}
impl System for PSXSystem {
    fn core_name(&self) -> &'static str {
        CORE_NAME_PSX
    }

    fn name(&self) -> &'static str {
        "PSX"
    }

    fn is_console(&self) -> bool {
        true
    }

    fn default_meta(&self) -> HashMap<&str, &str> {
        [
            ("pcsx_rearmed_bios", "HLE"),
            ("pcsx_rearmed_region", "PAL"),
            ("beetle_psx_region", "pal"),
        ]
        .into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut cue = None;
        let mut disc = None;
        let mut exe = None;

        walk_dir(&file.path.clone(), 4, |path, ext, _header| {
            if is_psx_exe(path) {
                if exe.is_none() {
                    // A broken executable shouldn't take the whole release down
                    // with it; there may still be a disc image next to it.
                    match handle_exe(path) {
                        Ok(built) => exe = Some(built),
                        Err(err) => warn!("Could not use PSX executable {path:?}: {err}"),
                    }
                }
            } else if ext == "cue" {
                if cue.is_none() && is_psx_cue(path) {
                    cue = Some(path.to_owned());
                }
            } else if disc.is_none() && is_psx_disc(path) {
                disc = Some(path.to_owned());
            }
            Ok(())
        })?;

        // A cue describes the whole disc — the data track plus any CD audio — so
        // it wins over the track it names. The wrapped executable comes last: a
        // release shipping both has the disc image as the real thing.
        //
        // Whatever the sheet's audio tracks are encoded as is not settled here:
        // `NewSys::load_file` puts any cue a system picked through
        // [`super::disc::prepare_disc`] before the core sees it.
        let found = cue.or(disc).or(exe);

        if let Some(found) = found {
            debug!("FMT: PSX disc {found:?}");
            file.path = found;
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn testdata() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("psx")
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A data disc with nothing on it the PlayStation would boot — what another
    /// console's disc looks like from the outside, down to the MODE2/2352
    /// wrapper when `raw` is set.
    fn other_disc(dir: &Path, name: &str, raw: bool) -> PathBuf {
        let files = [("DATA.BIN".to_string(), vec![0u8; ISO_SECTOR])];
        let mut image = build_iso(&IsoSpec {
            system_id: "OTHER",
            volume_id: "OTHER",
            files: &files,
            ..Default::default()
        });

        if raw {
            // Wrap each sector the way a cue's bin does: sync pattern, address,
            // XA subheader, user data, then room for the error correction.
            let total = image.len() / ISO_SECTOR;
            let mut sectors = vec![0u8; total * 2352];
            for lba in 0..total {
                let at = lba * 2352;
                sectors[at] = 0;
                sectors[at + 1..at + 12].fill(0xff);
                sectors[at + 15] = 2;
                sectors[at + 24..at + 24 + ISO_SECTOR]
                    .copy_from_slice(&image[lba * ISO_SECTOR..][..ISO_SECTOR]);
            }
            image = sectors;
        }

        let path = dir.join(name);
        fs::write(&path, image).unwrap();
        path
    }

    /// The two markers, one each: a raw dump that still carries Sony's licence
    /// text, and a data track whose system area was stripped, leaving only the
    /// boot file in the root to go on.
    #[test]
    fn detects_psx_discs() {
        assert!(is_psx_disc(
            &testdata().join("thisispsx/thisispsx_rc1c_iso.bin")
        ));
        assert!(is_psx_disc(&testdata().join("monophobia/mono_t1.bin")));
        assert!(is_psx_cue(&testdata().join("monophobia/mono.cue")));
    }

    /// A wrapped executable has to come back out as a disc the same detection
    /// accepts, or the release would only load the once.
    #[test]
    fn wrapped_executable_is_a_psx_disc() {
        let iso = create_psx_iso(&testdata().join("paradox/pdx-051.psx"))
            .unwrap()
            .expect("paradox release is a PS-X EXE");
        assert!(is_psx_disc(&iso));
    }

    /// Everything that isn't one: another console's disc in either wrapper, a
    /// cue naming it, an audio-only cue, and a file that is no disc at all.
    #[test]
    fn rejects_other_discs() {
        let dir = temp_dir("psx_detect_test");
        let iso = other_disc(&dir, "other.iso", false);
        let bin = other_disc(&dir, "other.bin", true);
        // Both are readable data discs — rejected on what's on them, not
        // because the layout sniff gave up on them.
        assert!(DiscImage::open(&iso).is_some());
        assert!(DiscImage::open(&bin).is_some());
        assert!(!is_psx_disc(&iso));
        assert!(!is_psx_disc(&bin));

        let cue = dir.join("other.cue");
        fs::write(&cue, "FILE \"other.bin\" BINARY\n  TRACK 01 MODE2/2352\n").unwrap();
        assert!(!is_psx_cue(&cue));

        fs::write(dir.join("track.wav"), b"not really a wav").unwrap();
        let audio = dir.join("audio.cue");
        fs::write(&audio, "FILE \"track.wav\" WAVE\n  TRACK 01 AUDIO\n").unwrap();
        assert!(!is_psx_cue(&audio));

        assert!(!is_psx_disc(&testdata().join("paradox/pdx-051.JPG")));
    }
}
