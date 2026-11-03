use std::process::Command;

use super::*;

#[test]
fn probe_output_is_parsed() {
    let info = parse_probe(
        "codec_name=mpeg4\nwidth=720\nheight=576\nsample_aspect_ratio=16:15\navg_frame_rate=0/0\nr_frame_rate=30000/1001\n",
    )
    .unwrap();
    assert_eq!((info.width, info.height), (720, 576));
    assert!((info.fps - 29.97).abs() < 0.01);
    assert!((info.sar - 16.0 / 15.0).abs() < 1e-9);
    assert_eq!(info.codec, "mpeg4");

    let info = parse_probe("width=64\nheight=48\nsample_aspect_ratio=N/A\n").unwrap();
    assert_eq!(info.fps, 25.0);
    assert_eq!(info.sar, 1.0);

    assert!(parse_probe("height=48\n").is_err());
}

#[test]
fn plays_a_clip_to_the_end() {
    if !is_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mkv");
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=25"])
        .args(["-f", "lavfi", "-i", "sine", "-t", "1"])
        .arg(&clip)
        .status()
        .unwrap();
    assert!(made.success());

    let mut emu = FfmpegEmu::new(&clip).unwrap();
    assert_eq!(emu.get_frame_size(), (64, 48));
    assert_eq!(emu.fps(), 25.0);
    assert_eq!(emu.sample_rate(), 48000.0);

    // Wait for decoding to get going, then check a picture and sound come out.
    std::thread::sleep(std::time::Duration::from_millis(500));
    emu.run();
    let mut lit = false;
    emu.with_frame(&mut |_, _, px| lit = px.iter().any(|&p| p.to_ne_bytes()[..3] != [0, 0, 0]));
    assert!(lit);
    let mut samples = 0;
    emu.with_audio(&mut |a| samples = a.len());
    assert_eq!(samples, 48000 / 25 * 2);

    emu.skip_frames(100);
    emu.run();
    assert!(emu.is_idle());
}
