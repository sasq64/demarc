//! CD image handling shared by the disc-based systems: reading an ISO9660
//! filesystem out of a dump in whatever sector layout it arrived in, writing a
//! minimal one from a set of files, and picking apart a cue sheet.
//!
//! Both users want the same three things and for the same reason — a release
//! turns up either as a disc image to identify or as the loose files that were
//! meant to be burned onto one — so the format lives here and the console
//! specifics ([`super::playstation`], [`super::neo_geo`]) stay with the system.

use std::{fs, path::Path, path::PathBuf};

/// One sector of an ISO9660 image: the user-data half of a CD-ROM data sector,
/// and the unit every address in the format counts in.
pub const ISO_SECTOR: usize = 2048;

/// The primary volume descriptor's fixed address. The 16 sectors ahead of it
/// are the system area, which the filesystem itself never uses.
pub const LBA_PVD: u32 = 16;

const LBA_TERMINATOR: u32 = 17;
const LBA_PATH_TABLE_L: u32 = 18;
const LBA_PATH_TABLE_M: u32 = 19;
const LBA_ROOT_DIR: u32 = 20;

/// An ISO9660 32-bit field: the value little endian, then big endian again.
/// Sizes and addresses are all stored twice so a reader of either byte order can
/// take the half it likes.
fn both_endian32(value: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0..4].copy_from_slice(&value.to_le_bytes());
    out[4..8].copy_from_slice(&value.to_be_bytes());
    out
}

/// [`both_endian32`] for the format's 16-bit fields.
fn both_endian16(value: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    out[0..2].copy_from_slice(&value.to_le_bytes());
    out[2..4].copy_from_slice(&value.to_be_bytes());
    out
}

/// Write `text` into `buf` at `at` as a fixed-width, space-padded field, which
/// is how ISO9660 stores every string.
fn put_padded(buf: &mut [u8], at: usize, len: usize, text: &str) {
    buf[at..at + len].fill(b' ');
    let n = text.len().min(len);
    buf[at..at + n].copy_from_slice(&text.as_bytes()[..n]);
}

/// An ISO9660 directory record naming `len` bytes at `lba`. `name` is the
/// identifier exactly as it goes on the disc, so files keep the `;1` version
/// suffix and the `.`/`..` entries are the single bytes 0 and 1.
///
/// Records are padded to an even length, which is what makes the one-byte-named
/// entries — and so the copy of the root's record inside the volume descriptor —
/// 34 bytes.
pub fn dir_record(name: &[u8], lba: u32, len: u32, is_dir: bool) -> Vec<u8> {
    let mut rec = vec![0u8; 33 + name.len()];
    rec[2..10].copy_from_slice(&both_endian32(lba));
    rec[10..18].copy_from_slice(&both_endian32(len));
    // Recording time, as years-since-1900 down to seconds plus a timezone in
    // quarter hours. Nothing on the console side reads it; a fixed 1980-01-01
    // GMT keeps the image byte-identical between builds, which the
    // content-keyed caches the callers use rely on.
    rec[18..25].copy_from_slice(&[80, 1, 1, 0, 0, 0, 0]);
    rec[25] = if is_dir { 0x02 } else { 0x00 };
    rec[28..32].copy_from_slice(&both_endian16(1)); // volume sequence number
    rec[32] = name.len() as u8;
    rec[33..].copy_from_slice(name);
    if !rec.len().is_multiple_of(2) {
        rec.push(0);
    }
    rec[0] = rec.len() as u8;
    rec
}

/// What [`build_iso`] should put on the disc and what to call it.
#[derive(Default)]
pub struct IsoSpec<'a> {
    /// Volume descriptor system identifier, e.g. `PLAYSTATION`. Some tools
    /// recognise a disc by it; no console here refuses to boot without it.
    pub system_id: &'a str,
    /// Volume descriptor volume identifier — the disc's name.
    pub volume_id: &'a str,
    /// The files, in root directory order, as the ISO9660 identifier *without*
    /// the `;1` version suffix (which [`build_iso`] appends) and the contents.
    pub files: &'a [(String, Vec<u8>)],
    /// Names for the volume descriptor's copyright, abstract and bibliographic
    /// file identifier fields, each with the `;1` suffix if it wants one.
    pub copyright_file: Option<&'a str>,
    pub abstract_file: Option<&'a str>,
    pub bibliographic_file: Option<&'a str>,
    /// Empty sectors appended past the last file, counted in the recorded
    /// volume size. Only there so a caller can steer the image away from a
    /// length that some reader would mistake for a different sector layout.
    pub pad_sectors: u32,
}

/// The primary volume descriptor: the sector at a fixed 16 that every reader
/// starts from, and which points at the root directory's own record.
fn primary_volume_descriptor(
    spec: &IsoSpec,
    total_sectors: u32,
    path_table_size: u32,
    root: &[u8],
) -> Vec<u8> {
    let mut pvd = vec![0u8; ISO_SECTOR];
    pvd[0] = 1; // primary volume descriptor
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1; // descriptor version
    put_padded(&mut pvd, 8, 32, spec.system_id);
    put_padded(&mut pvd, 40, 32, spec.volume_id);
    pvd[80..88].copy_from_slice(&both_endian32(total_sectors));
    pvd[120..124].copy_from_slice(&both_endian16(1)); // volume set size
    pvd[124..128].copy_from_slice(&both_endian16(1)); // volume sequence number
    pvd[128..132].copy_from_slice(&both_endian16(ISO_SECTOR as u16));
    pvd[132..140].copy_from_slice(&both_endian32(path_table_size));
    // The two path tables are the one pair of fields stored as a plain value in
    // each byte order rather than both-endian.
    pvd[140..144].copy_from_slice(&LBA_PATH_TABLE_L.to_le_bytes());
    pvd[148..152].copy_from_slice(&LBA_PATH_TABLE_M.to_be_bytes());
    pvd[156..156 + root.len()].copy_from_slice(root);
    // Volume set, publisher, data preparer, application.
    for (at, len) in [(190, 128), (318, 128), (446, 128), (574, 128)] {
        put_padded(&mut pvd, at, len, "");
    }
    for (at, name) in [
        (702, spec.copyright_file),
        (739, spec.abstract_file),
        (776, spec.bibliographic_file),
    ] {
        put_padded(&mut pvd, at, 37, name.unwrap_or(""));
    }
    // Creation and modification, then expiration and effective, as
    // YYYYMMDDHHMMSSCC plus a timezone byte. All zeros means "unspecified",
    // which is what a disc that never expires records.
    for at in [813, 830] {
        pvd[at..at + 17].copy_from_slice(b"1980010100000000\0");
    }
    for at in [847, 864] {
        pvd[at..at + 17].copy_from_slice(b"0000000000000000\0");
    }
    pvd[881] = 1; // file structure version
    pvd
}

/// Lay directory records out the way the format requires: a record may not
/// straddle a sector boundary, and the tail of each sector is zeroed, which is
/// also what tells a reader there are no more records in it. The result is a
/// whole number of sectors.
fn pack_records(records: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    for rec in records {
        if at + rec.len() > ISO_SECTOR {
            out.resize(out.len() + (ISO_SECTOR - at), 0);
            at = 0;
        }
        out.extend_from_slice(rec);
        at += rec.len();
    }
    if at > 0 {
        out.resize(out.len() + (ISO_SECTOR - at), 0);
    }
    out
}

/// Write the smallest ISO9660 image that holds `spec.files` in its root
/// directory: no subdirectories, no Joliet or Rock Ridge extensions, one
/// path table entry. That is deliberately the level-1 filesystem a console's
/// own boot ROM was written against, so names have to already be 8.3 and upper
/// case — see [`iso_name`], which the callers use to check.
pub fn build_iso(spec: &IsoSpec) -> Vec<u8> {
    // Version suffix and all: the identifier as it goes in the record.
    let names: Vec<Vec<u8>> = spec
        .files
        .iter()
        .map(|(name, _)| format!("{name};1").into_bytes())
        .collect();

    // Where the files start depends on how many sectors the root directory
    // takes, and that depends only on the name lengths — so size the records
    // once with placeholder addresses, then build them again for real.
    let sizing: Vec<Vec<u8>> = std::iter::once(dir_record(&[0], 0, 0, true))
        .chain(std::iter::once(dir_record(&[1], 0, 0, true)))
        .chain(names.iter().map(|name| dir_record(name, 0, 0, false)))
        .collect();
    let root_sectors = (pack_records(&sizing).len() / ISO_SECTOR) as u32;
    let root_len = root_sectors * ISO_SECTOR as u32;

    let mut lba = LBA_ROOT_DIR + root_sectors;
    let mut records = vec![
        dir_record(&[0], LBA_ROOT_DIR, root_len, true),
        dir_record(&[1], LBA_ROOT_DIR, root_len, true),
    ];
    let mut placed = Vec::with_capacity(spec.files.len());
    for (name, (_, data)) in names.iter().zip(spec.files) {
        records.push(dir_record(name, lba, data.len() as u32, false));
        placed.push((lba, data));
        // An empty file still gets a sector, so its address is one no other
        // file's extent covers.
        lba += (data.len().div_ceil(ISO_SECTOR) as u32).max(1);
    }
    let root_dir = pack_records(&records);
    let total_sectors = lba + spec.pad_sectors;

    // The path table, listing the one directory there is: a one-byte name (the
    // root's, which is the single zero byte), parented to itself.
    let mut path_l = vec![1u8, 0];
    path_l.extend_from_slice(&LBA_ROOT_DIR.to_le_bytes());
    path_l.extend_from_slice(&1u16.to_le_bytes());
    path_l.extend_from_slice(&[0, 0]); // name, then padding to an even length
    let mut path_m = vec![1u8, 0];
    path_m.extend_from_slice(&LBA_ROOT_DIR.to_be_bytes());
    path_m.extend_from_slice(&1u16.to_be_bytes());
    path_m.extend_from_slice(&[0, 0]);

    let root_record = dir_record(&[0], LBA_ROOT_DIR, root_len, true);
    let pvd = primary_volume_descriptor(spec, total_sectors, path_l.len() as u32, &root_record);

    let mut terminator = vec![0u8; ISO_SECTOR];
    terminator[0] = 0xff; // volume descriptor set terminator
    terminator[1..6].copy_from_slice(b"CD001");
    terminator[6] = 1;

    let mut image = vec![0u8; total_sectors as usize * ISO_SECTOR];
    let mut put = |lba: u32, bytes: &[u8]| {
        let at = lba as usize * ISO_SECTOR;
        image[at..at + bytes.len()].copy_from_slice(bytes);
    };
    put(LBA_PVD, &pvd);
    put(LBA_TERMINATOR, &terminator);
    put(LBA_PATH_TABLE_L, &path_l);
    put(LBA_PATH_TABLE_M, &path_m);
    put(LBA_ROOT_DIR, &root_dir);
    for (lba, data) in placed {
        put(lba, data);
    }
    image
}

/// `name` as an ISO9660 level 1 identifier — upper case, at most 8 characters
/// and a 3 character extension — or `None` if it doesn't fit, since a console
/// boot ROM reading a level 1 filesystem has no way to see a name that doesn't.
///
/// The length limit is the part that matters and the part that is enforced;
/// `-` is not strictly a d-character but every burner passes it through, and a
/// name here has to keep matching whatever already refers to it (a Neo Geo CD's
/// `IPL.TXT`, say), so mangling it would be worse than allowing it.
///
/// The version suffix isn't part of this; [`build_iso`] appends it.
pub fn iso_name(name: &str) -> Option<String> {
    let ok = |s: &str, max: usize| {
        !s.is_empty()
            && s.len() <= max
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    let upper = name.to_ascii_uppercase();
    match upper.rsplit_once('.') {
        Some((stem, ext)) if ok(stem, 8) && ok(ext, 3) => Some(upper),
        None if ok(&upper, 8) => Some(upper),
        _ => None,
    }
}

/// The sector layouts a disc image turns up in, as the bytes one sector takes
/// in the file and where its 2048 bytes of user data start inside that.
///
/// A `.iso` holds the user data and nothing else. The `.bin` a cue names holds
/// whole CD sectors: a 12-byte sync pattern and a 4-byte address in front, then
/// — for Mode 2, which is what PlayStation discs are pressed as — an 8-byte XA
/// subheader, with error correction after the data. A CloneCD `.img` adds 96
/// bytes of subchannel per sector on top of that.
///
/// Order matters: the first layout whose sector 16 looks like a volume
/// descriptor is the one taken, and Mode 2 is the common case here.
const SECTOR_LAYOUTS: &[(u64, usize)] = &[
    (2352, 24), // Mode 2 Form 1
    (2352, 16), // Mode 1
    (2048, 0),  // user data only
    (2336, 8),  // Mode 2 with the sync and address stripped
    (2448, 24), // Mode 2 Form 1 plus subchannel
    (2448, 16), // Mode 1 plus subchannel
];

/// Sectors of the root directory [`DiscImage::root_names`] will read before
/// giving up. Only enough to see the boot files matters, and a corrupt length
/// field shouldn't turn a sniff into a long read.
const MAX_ROOT_SECTORS: usize = 16;

/// A disc image opened in whatever sector layout it turned out to be stored in.
/// One only exists for a file that really holds an ISO9660 filesystem, since
/// finding the volume descriptor is what identifies the layout in the first
/// place.
pub struct DiscImage {
    file: fs::File,
    sector_size: u64,
    data_offset: usize,
}

impl DiscImage {
    /// Open `path` as a disc image, or `None` if no [`SECTOR_LAYOUTS`] entry
    /// puts a primary volume descriptor at sector 16 — which is to say, if it
    /// isn't a data disc at all.
    pub fn open(path: &Path) -> Option<Self> {
        let mut disc = DiscImage {
            file: fs::File::open(path).ok()?,
            sector_size: 0,
            data_offset: 0,
        };
        for &(sector_size, data_offset) in SECTOR_LAYOUTS {
            disc.sector_size = sector_size;
            disc.data_offset = data_offset;
            if disc
                .read_sector(LBA_PVD)
                .is_some_and(|pvd| pvd[0] == 1 && pvd[1..6] == *b"CD001")
            {
                return Some(disc);
            }
        }
        None
    }

    /// The user data of sector `lba`, or `None` past the end of the image.
    pub fn read_sector(&mut self, lba: u32) -> Option<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let at = lba as u64 * self.sector_size + self.data_offset as u64;
        let mut buf = vec![0u8; ISO_SECTOR];
        self.file.seek(SeekFrom::Start(at)).ok()?;
        self.file.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    /// Whether `marker` appears anywhere in the system area — the 16 sectors
    /// ahead of the filesystem, where a pressed disc keeps its console's
    /// licence data.
    pub fn system_area_has(&mut self, marker: &[u8]) -> bool {
        (0..LBA_PVD).any(|lba| {
            self.read_sector(lba)
                .is_some_and(|sector| sector.windows(marker.len()).any(|w| w == marker))
        })
    }

    /// The names in the root directory, upper cased and with the `;1` version
    /// suffix dropped.
    ///
    /// This walks the directory itself rather than going through a filesystem
    /// crate: every crate on offer reads a plain 2048-byte-sector image, so a
    /// raw MODE2/2352 track — which is what a PlayStation disc actually is —
    /// would need the sector translation above wrapped around it anyway, and
    /// the question here is only which names the root holds.
    pub fn root_names(&mut self) -> Vec<String> {
        let Some(pvd) = self.read_sector(LBA_PVD) else {
            return vec![];
        };
        // The root's own directory record sits inside the volume descriptor, at
        // the fixed offset every reader picks it up from. See [`dir_record`] for
        // the layout of the fields read here.
        let root = &pvd[156..190];
        let field = |at: usize| u32::from_le_bytes(root[at..at + 4].try_into().unwrap());
        let lba = field(2);
        let sectors = (field(10) as usize)
            .div_ceil(ISO_SECTOR)
            .min(MAX_ROOT_SECTORS);

        let mut names = vec![];
        for i in 0..sectors as u32 {
            let Some(sector) = self.read_sector(lba + i) else {
                break;
            };
            let mut at = 0;
            // Records never straddle a sector; the leftover is zeroed, and a
            // zero length is what marks the end of the ones in this sector.
            while at + 33 < ISO_SECTOR {
                let rec_len = sector[at] as usize;
                let name_len = sector[at + 32] as usize;
                if rec_len < 34 || at + rec_len > ISO_SECTOR || 33 + name_len > rec_len {
                    break;
                }
                // The one-byte names 0 and 1 are `.` and `..`, which every
                // directory has and no file is called.
                if name_len > 1 {
                    let name = String::from_utf8_lossy(&sector[at + 33..at + 33 + name_len]);
                    names.push(name.split(';').next().unwrap_or_default().to_uppercase());
                }
                at += rec_len;
            }
        }
        names
    }
}

/// One `FILE "<name>" <kind>` line from a cue sheet.
pub struct CueFile<'a> {
    pub line: &'a str,
    pub name: String,
    pub kind: String,
}

/// Pull the `FILE` lines out of a cue sheet. Handles both quoted and bare names
/// (scene sheets use either); the kind is always the last token on the line.
pub fn parse_cue_files(text: &str) -> Vec<CueFile<'_>> {
    let mut out = vec![];
    for line in text.lines() {
        let rest = match line.trim().strip_prefix("FILE ") {
            Some(rest) => rest.trim(),
            None => continue,
        };
        let (name, kind) = if let Some(after) = rest.strip_prefix('"') {
            match after.split_once('"') {
                Some((name, kind)) => (name.to_string(), kind.trim().to_string()),
                None => continue,
            }
        } else {
            match rest.rsplit_once(char::is_whitespace) {
                Some((name, kind)) => (name.trim().to_string(), kind.trim().to_string()),
                None => continue,
            }
        };
        out.push(CueFile { line, name, kind });
    }
    out
}

/// The data tracks a cue sheet names, as paths on disk. Audio tracks are left
/// out: the question the callers ask is which console the disc belongs to, and
/// only a data track can answer it.
pub fn cue_data_tracks(cue_path: &Path) -> Vec<PathBuf> {
    let Ok(text) = fs::read_to_string(cue_path) else {
        return vec![];
    };
    let dir = cue_path.parent().unwrap_or(Path::new("."));
    parse_cue_files(&text)
        .iter()
        .filter(|f| {
            ["BINARY", "MOTOROLA"]
                .iter()
                .any(|k| f.kind.eq_ignore_ascii_case(k))
        })
        .filter_map(|f| resolve_ignoring_case(dir, &f.name))
        .collect()
}

/// Find `name` in `dir` however it happens to be cased on disk. Cue sheets are
/// written against the burned disc's upper-case names, which is rarely how the
/// files were left lying around.
pub fn resolve_ignoring_case(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(name))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_files(files: &[(&str, usize)]) -> Vec<(String, Vec<u8>)> {
        files
            .iter()
            .map(|(name, len)| (iso_name(name).unwrap(), vec![b'x'; *len]))
            .collect()
    }

    /// What the writer puts down, the reader has to find again — including the
    /// case where the root directory outgrows a single sector, which is the one
    /// place the record packing can go wrong.
    #[test]
    fn round_trips_a_root_directory() {
        let names: Vec<String> = (0..80).map(|i| format!("FILE{i:04}.BIN")).collect();
        let files = spec_files(&names.iter().map(|n| (n.as_str(), 100)).collect::<Vec<_>>());
        let image = build_iso(&IsoSpec {
            system_id: "TEST",
            volume_id: "TEST",
            files: &files,
            ..Default::default()
        });
        // 80 records of 46 bytes plus the two 34-byte ones is past 2048.
        assert!(image.len() / ISO_SECTOR > 20 + 1 + 80);

        let dir = std::env::temp_dir().join("demarc_iso_roundtrip");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.iso");
        fs::write(&path, &image).unwrap();

        let mut disc = DiscImage::open(&path).expect("written image is a data disc");
        assert_eq!(disc.root_names(), names);
    }

    /// Level 1 is 8.3 and upper case, and a name that misses is one the boot
    /// ROM would never see — so it has to come back as `None`, not mangled.
    #[test]
    fn checks_iso_names() {
        assert_eq!(iso_name("Test.prg").as_deref(), Some("TEST.PRG"));
        assert_eq!(iso_name("SOUND9V3.z80").as_deref(), Some("SOUND9V3.Z80"));
        assert_eq!(iso_name("IPL").as_deref(), Some("IPL"));
        assert_eq!(iso_name("toolonganame.bin"), None);
        assert_eq!(iso_name("name.toolong"), None);
        assert_eq!(iso_name("has space.bin"), None);
        assert_eq!(iso_name(".prg"), None);
    }
}
