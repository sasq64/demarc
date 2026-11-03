use std::path::Path;

use super::*;

#[test]
fn claims_video_files_only() {
    if !ffmpeg_emu::is_available() {
        return;
    }
    let sys = FfmpegSystem {};
    assert!(sys.can_load(Path::new("demo.MP4")));
    assert!(sys.can_load(Path::new("capture.avi")));
    assert!(!sys.can_load(Path::new("readme.txt")));
}
