use super::*;

/// A white square at `x` on black.
fn square(x: u32) -> Vec<u32> {
    let (w, h) = (128, 64);
    (0..w * h)
        .map(|i| {
            let (px, py) = (i % w, i / w);
            if (x..x + 16).contains(&px) && (24..40).contains(&py) {
                0xffff_ffff
            } else {
                0xff00_0000
            }
        })
        .collect()
}

#[test]
#[ignore = "needs librife_demarc (`just rife`), the model and a GPU"]
fn a_square_halfway_is_between_its_two_frames() {
    let lib = Path::new(env!("DEMARC_ROOT"))
        .join("target/rife")
        .join(LIB_NAME);
    let model = std::env::var("RIFE_MODEL").expect("RIFE_MODEL=<dir with flownet.param>");
    let mut rife = Rife::load_from(&lib, Path::new(&model)).unwrap();
    let mut out = Vec::new();
    rife.interpolate(&square(32), &square(64), 128, 64, 0.5, &mut out)
        .unwrap();
    let white = |x: u32| out[(32 * 128 + x) as usize] & 0xff > 0x80;
    assert!(white(48 + 8), "centre of the halfway square");
    assert!(
        !white(32 + 4) && !white(64 + 12),
        "outside the halfway square"
    );
}
