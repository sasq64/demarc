use std::path::PathBuf;

use super::*;

/// Paths here are rooted at the crate directory rather than left relative:
/// a conversion running in another test switches the process-wide working
/// directory for its duration (see `cbmconvert::CwdGuard`).
fn root(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn get_path(name: &str) -> PathBuf {
    root("testdata/iffILBM").join(name)
}

/// Load a test image from the `iffILBM/` corpus and assert it decodes to the
/// expected size and isn't a single flat colour (which would signal a
/// mis-decode). Returns the decoded image for further checks.
fn check(file: &str, w: u32, h: u32) -> RgbaImage {
    let img = load(get_path(file)).unwrap_or_else(|e| panic!("failed to load {file}: {e}"));
    assert_eq!(img.dimensions(), (w, h), "wrong dimensions for {file}");
    let first = img.get_pixel(0, 0);
    assert!(
        img.pixels().any(|p| p != first),
        "decoded {file} is a single flat colour"
    );
    img
}

#[test]
fn test_ilbm() {
    let img = load(root("testdata/test.iff")).unwrap();
    assert_eq!(img.dimensions(), (640, 512));
    // The image must not be entirely one colour.
    let first = img.get_pixel(0, 0);
    assert!(img.pixels().any(|p| p != first), "decoded image is blank");
}

/// Plain paletted ILBM (interleaved planar, ByteRun1 compressed).
#[test]
fn test_ilbm_paletted() {
    check("abydos.ilbm", 320, 240);
    check("Devilstar_Indyface.iff", 320, 256);
}

/// Uncompressed ILBM body (compression == 0).
#[test]
fn test_ilbm_uncompressed() {
    check("dalton-big-wheel-640x512.ilbm", 640, 512);
}

/// HAM6 and HAM8 hold-and-modify images.
#[test]
fn test_ham() {
    check("AH_Swimmer.iff", 320, 200); // HAM6
    check("FearFace.HAM8", 640, 512); // HAM8
}

/// Extra-HalfBrite (64 registers, upper 32 are half-bright copies). Stored
/// 320x512 (lores interlace), stretched to 640x512 for square pixels.
#[test]
fn test_ehb() {
    check("DECKER-BattleMech.lbm", 640, 512);
}

/// masking == 2 (transparent colour, no extra mask plane) must not offset
/// the plane layout.
#[test]
fn test_masked_transparent_colour() {
    check("ghost", 320, 256);
    check("agony.iff", 288, 192);
}

/// PBM (DeluxePaint PC chunky), both uncompressed and ByteRun1.
#[test]
fn test_pbm() {
    check("4xd_oz_0.ilbm", 800, 600); // compression 0
    check("water.lbm", 640, 480); // compression 1
}

/// ACBM (contiguous bitplanes in an ABIT chunk).
#[test]
fn test_acbm() {
    check("TEST.ACBM", 320, 256); // compression flag 0
    check("cover", 320, 256); // no CAMG
    check("0900.acbm", 248, 511); // HAM ACBM, lores interlace -> double width
}

/// Deep 24-bit ILBM (direct RGB, no palette).
#[test]
fn test_deep_24bit() {
    check("24.iff", 455, 341);
}

/// Impulse RGB8 (24-bit truecolour, RLE-compressed).
#[test]
fn test_rgb8() {
    check("WorldMap2.24", 224, 118);
}

/// Impulse RGBN (12-bit truecolour, RLE-compressed). Stored 320x400 (lores
/// interlace), stretched to 640x400 for square pixels.
#[test]
fn test_rgbn() {
    check("spock.rgbn", 640, 400);
}

/// The indexed path should preserve one index per pixel and a usable palette
/// for a plain paletted image.
#[test]
fn test_load_indexed() {
    let img = load_indexed(get_path("abydos.ilbm")).unwrap();
    assert_eq!((img.width, img.height), (320, 240));
    assert_eq!(img.indices.len(), (320 * 240) as usize);
    assert!(!img.palette.is_empty());
    // Every index must be resolvable in the palette.
    let max = *img.indices.iter().max().unwrap() as usize;
    assert!(max < img.palette.len(), "index {max} outside palette");
}

/// The indexed path must refuse HAM and truecolour images (their pixels are
/// not plain palette lookups) so callers fall back to the RGBA path.
#[test]
fn test_indexed_rejects_non_palette() {
    assert!(load_indexed(get_path("AH_Swimmer.iff")).is_err(), "HAM");
    assert!(load_indexed(get_path("24.iff")).is_err(), "deep");
    assert!(load_indexed(get_path("WorldMap2.24")).is_err(), "RGB8");
}

/// Colour-cycling ranges are collected from CRNG chunks.
#[test]
fn test_cycle_ranges() {
    let img = load_indexed(get_path("water.lbm")).unwrap();
    assert!(!img.ranges.is_empty(), "expected CRNG ranges");
}

/// CTBL "dynamic colour table": a full 16-colour palette per scanline.
#[test]
fn test_ctbl() {
    check("amiga-ferrari.dhr", 704, 512);
    check("Seascape.dr", 704, 480);
}

/// BEAM: like CTBL but combined with HAM (16 base registers per line).
#[test]
fn test_beam_ham() {
    check("Eagle.beam", 320, 200);
}

/// SHAM (sliced HAM): one 16-colour palette per (doubled) scanline.
#[test]
fn test_sham() {
    check("mansion.sham", 320, 200); // one palette per line (lores)
    check("sham.iff", 640, 512); // per two lines; lores interlace -> double width
}

/// PCHG (palette change): per-line register deltas from a base palette.
#[test]
fn test_pchg() {
    check("Lake.mp", 630, 422);
}

/// Dynamic-palette images can't be a single indexed frame, so the indexed
/// path must reject them (callers then use the fixed-RGBA path).
#[test]
fn test_indexed_rejects_dynamic_palette() {
    assert!(load_indexed(root("testdata/iffILBM/amiga-ferrari.dhr")).is_err());
    assert!(load_indexed(root("testdata/iffILBM/Lake.mp")).is_err());
}

/// A dynamic-palette image should genuinely use more than its base 16
/// colours: colours must vary between the top and bottom of the frame.
#[test]
fn test_ctbl_varies_down_screen() {
    let img = load(root("testdata/iffILBM/amiga-ferrari.dhr")).unwrap();
    let top: std::collections::HashSet<_> =
        (0..img.width()).map(|x| img.get_pixel(x, 0).0).collect();
    let bottom: std::collections::HashSet<_> = (0..img.width())
        .map(|x| img.get_pixel(x, img.height() - 1).0)
        .collect();
    assert_ne!(top, bottom, "per-scanline palette had no visible effect");
}

/// Aspect-ratio correction: non-square Amiga pixels are stretched to square.
#[test]
fn test_aspect_correction() {
    // Hires (thin pixels) is stretched vertically: 640x256 -> 640x512.
    check("aplacet.lbm", 640, 512);
    check("blackrbe.lbm", 640, 512);
    // Lores interlace (wide pixels) is stretched horizontally, covered by
    // test_ehb / test_sham (320x512 -> 640x512).
    // Already-square modes are left at their native size:
    check("Devilstar_Indyface.iff", 320, 256); // lores
    check("dalton-big-wheel-640x512.ilbm", 640, 512); // hires interlace
    check("Seascape.dr", 704, 480); // hires interlace
    // PBM (PC DeluxePaint) always has square pixels, even at hires-ish sizes.
    check("water.lbm", 640, 480);
    // A hires mode id on a picture whose BMHD says its pixels are square
    // (10:10) is left at its stored size.
    check("CK_Welcome_To_Omega_6.iff", 640, 256);
}

/// `scale_grid` replicates each cell into an `sx` x `sy` block.
#[test]
fn test_scale_grid() {
    // 2x1 grid [1, 2] doubled horizontally -> [1, 1, 2, 2].
    assert_eq!(scale_grid(&[1u8, 2], 2, 1, 2, 1), vec![1, 1, 2, 2]);
    // 1x2 grid [1; 2] doubled vertically -> [1, 1, 2, 2].
    assert_eq!(scale_grid(&[1u8, 2], 1, 2, 1, 2), vec![1, 1, 2, 2]);
    // A no-op scale returns the input unchanged.
    assert_eq!(scale_grid(&[1u8, 2, 3, 4], 2, 2, 1, 1), vec![1, 2, 3, 4]);
}

/// Super-hires: quarter-width pixels, so the height is quadrupled.
/// `dalton-big-wheel-1280x256` is the same picture as the hires-interlace
/// `dalton-big-wheel-640x512`, and must come out the same shape.
#[test]
fn test_super_hires() {
    check("dalton-big-wheel-1280x256.ilbm", 1280, 1024);
    // Productivity (a Multiscan mode id) sets the SHRES bit on a screen
    // that is already square, and must not be stretched.
    check("attaq.lbm", 640, 480);
}

/// A `BmHeader` of the given size, with the aspect fields left unfilled
/// (as plenty of real writers leave them).
fn hdr(width: u16, height: u16) -> BmHeader {
    BmHeader {
        width,
        height,
        num_planes: 8,
        masking: 0,
        compression: 0,
        x_aspect: 0,
        y_aspect: 0,
    }
}

/// The `display_scale` mode logic, exercised directly.
#[test]
fn test_display_scale() {
    let scale = |w, h, camg| display_scale("ILBM", &hdr(w, h), camg);
    assert_eq!(scale(640, 256, CAMG_HIRES), (1, 2)); // hires
    assert_eq!(scale(320, 512, CAMG_LACE), (2, 1)); // lores lace
    assert_eq!(scale(640, 512, CAMG_HIRES | CAMG_LACE), (1, 1));
    assert_eq!(scale(320, 200, 0), (1, 1)); // lores
    // Super-hires (the mode id sets HIRES too), plain and interlaced.
    let shres = CAMG_HIRES | CAMG_SHRES;
    assert_eq!(scale(1280, 256, shres), (1, 4));
    assert_eq!(scale(1280, 512, shres | CAMG_LACE), (1, 2));
    // A 640-wide screen claiming SHRES is an extended mode id (Productivity
    // here), not a super-hires screen: treat it as hires.
    assert_eq!(scale(640, 480, 0x00039024), (1, 1));
    assert_eq!(scale(640, 256, 0x00039020), (1, 2));
    // Garbage in the high bits must not read as a resolution.
    assert_eq!(scale(320, 200, 0x4800), (1, 1));
    // No CAMG mode: infer from dimensions (the user's fallback rule).
    assert_eq!(scale(320, 512, 0), (2, 1));
    assert_eq!(scale(640, 256, 0), (1, 2));
    // Super-hires is never inferred without a CAMG.
    assert_eq!(scale(1280, 256, 0), (1, 2));
    // PBM is always square, even at hires dimensions.
    assert_eq!(display_scale("PBM ", &hdr(640, 480), 0), (1, 1));
}

/// A BMHD declaring equal x and y aspect means square pixels, and wins over
/// the mode id: a picture drawn square but saved as a hires screen must not
/// be stretched to twice its height.
#[test]
fn test_square_aspect_overrides_mode() {
    let square = |w, h, camg| {
        let mut h = hdr(w, h);
        (h.x_aspect, h.y_aspect) = (10, 10);
        display_scale("ILBM", &h, camg)
    };
    assert_eq!(square(640, 256, CAMG_HIRES), (1, 1));
    assert_eq!(square(320, 512, CAMG_LACE), (1, 1));
    // An unequal ratio is not trusted: too many writers leave a stale
    // default there, so the mode id still decides.
    let mut stale = hdr(640, 256);
    (stale.x_aspect, stale.y_aspect) = (10, 11);
    assert_eq!(display_scale("ILBM", &stale, CAMG_HIRES), (1, 2));
}

/// `palette_bits` tells a 4-bit (OCS/ECS) colour map from an 8-bit (AGA)
/// one, in both of the ways a 4-bit value gets written out.
#[test]
fn test_palette_bits() {
    assert_eq!(palette_bits(&[0xff, 0x11, 0xaa]), Some(4)); // nibble-replicated
    assert_eq!(palette_bits(&[0xf0, 0x10, 0xa0]), Some(4)); // high nibble only
    assert_eq!(palette_bits(&[0xff, 0x11, 0xab]), Some(8)); // needs 8 bits
    assert_eq!(palette_bits(&[]), None);
    // A trailing partial entry is ignored rather than misread.
    assert_eq!(palette_bits(&[0x12]), None);
}

/// `describe` names the colour depth of the palette: AGA images carry full
/// 8-bit components, OCS/ECS ones only 4 bits per component.
#[test]
fn test_describe_chipset() {
    let d = |f: &str| describe(&fs::read(get_path(f)).unwrap());
    assert_eq!(d("aplacet.lbm"), "Amiga AGA 640x512 (256 colors)");
    assert_eq!(d("ghost"), "Amiga OCS 320x256 (16 colors)");
    assert_eq!(d("TEST.ACBM"), "Amiga OCS 320x256 (8 colors)");
    assert_eq!(d("FearFace.HAM8"), "Amiga AGA 640x512 (HAM8)");
    assert_eq!(
        d("DECKER-BattleMech.lbm"),
        "Amiga OCS 640x512 (64 colors/EHB)"
    );
    assert_eq!(d("amiga-ferrari.dhr"), "Amiga OCS 704x512 (16 colors/CTBL)");
    // Truecolour images have no palette to judge, and a PC PBM's palette is
    // not an Amiga one.
    assert_eq!(d("24.iff"), "Amiga 455x341 (True color)");
    assert_eq!(d("water.lbm"), "PC 640x480 (256 colors)");
}

/// Non-IFF and unsupported forms should error rather than panic.
#[test]
fn test_rejects_garbage() {
    assert!(load_from_memory(b"not an iff file at all").is_err());
    assert!(load_from_memory(&[]).is_err());
}

/// Build a minimal 16x2 one-plane ILBM whose BODY chunk declares `body_size`
/// bytes but only carries `body`, for the malformed-chunk tests below.
fn ilbm_with_body(body_size: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"ILBM");
    out.extend_from_slice(b"BMHD");
    out.extend_from_slice(&20u32.to_be_bytes());
    // width, height, x, y, planes, masking, compression, pad, transparent,
    // aspect x/y, page width/height.
    out.extend_from_slice(&16u16.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&[1, 0, 0, 0]);
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(b"BODY");
    out.extend_from_slice(&body_size.to_be_bytes());
    out.extend_from_slice(body);
    let mut form = b"FORM".to_vec();
    form.extend_from_slice(&(out.len() as u32).to_be_bytes());
    form.extend_from_slice(&out);
    form
}

/// A chunk whose size field runs past the end of the file must be rejected,
/// not sliced out of bounds. `describe` reads the same chunks and must
/// survive it too.
#[test]
fn test_rejects_bad_chunk_size() {
    // A sane file built the same way still loads, so the rejections below
    // are about the bad size and nothing else.
    let ok = ilbm_with_body(4, &[0xff, 0x00, 0xff, 0x00]);
    assert!(load_from_memory(&ok).is_ok(), "control image should load");

    for bad in [5u32, 1 << 20, u32::MAX] {
        let bytes = ilbm_with_body(bad, &[0xff, 0x00, 0xff, 0x00]);
        assert!(
            load_from_memory(&bytes).is_err(),
            "BODY declaring {bad} bytes should be rejected"
        );
        describe(&bytes);
    }
}

/// Truncating a real file anywhere must give an error or a valid image,
/// never a panic.
#[test]
fn test_truncated_files_dont_panic() {
    for file in [
        "abydos.ilbm",
        "AH_Swimmer.iff",
        "TEST.ACBM",
        "water.lbm",
        "spock.rgbn",
    ] {
        let bytes = fs::read(get_path(file)).unwrap();
        for len in (0..bytes.len()).step_by(97) {
            let part = &bytes[..len];
            let _ = load_from_memory(part);
            let _ = load_indexed_from_memory(part);
            describe(part);
        }
    }
}
