use super::*;

/// Opens a path through the VFS the way a core would, panicking if it fails.
fn open_read(path: &std::path::Path) -> *mut retro_vfs_file_handle {
    let c = CString::new(path.to_str().unwrap()).unwrap();
    let h = unsafe { open(c.as_ptr(), RETRO_VFS_FILE_ACCESS_READ, 0) };
    assert!(!h.is_null(), "open failed for {}", path.display());
    h
}

fn c(path: &std::path::Path) -> CString {
    CString::new(path.to_str().unwrap()).unwrap()
}

/// The one that bit us: `seek` follows `fseek`, returning 0 on success and
/// *not* the new position, whatever `retro_vfs_seek_t`'s docs say. A core
/// doing `if (fseek(f, off, SEEK_SET)) return -1;` reads any other non-zero
/// value as a failure — which is how a working CD image came back
/// "unsupported/invalid".
#[test]
fn seek_returns_zero_on_success_not_the_position() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rom.bin");
    std::fs::write(&path, vec![0xABu8; 4096]).unwrap();

    let h = open_read(&path);
    unsafe {
        // A non-zero offset is where returning the position would show up.
        assert_eq!(seek(h, 2048, RETRO_VFS_SEEK_POSITION_START as c_int), 0);
        // ...but the seek really did move the cursor.
        assert_eq!(tell(h), 2048);

        assert_eq!(seek(h, 0, RETRO_VFS_SEEK_POSITION_END as c_int), 0);
        assert_eq!(tell(h), 4096);
        assert_eq!(seek(h, -96, RETRO_VFS_SEEK_POSITION_CURRENT as c_int), 0);
        assert_eq!(tell(h), 4000);

        // An unknown whence is an error, and leaves the cursor alone.
        assert_eq!(seek(h, 0, 99), -1);
        assert_eq!(tell(h), 4000);

        assert_eq!(close(h), 0);
    }
}

#[test]
fn reads_seek_and_size_agree_with_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rom.bin");
    let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    std::fs::write(&path, &data).unwrap();

    let h = open_read(&path);
    unsafe {
        assert_eq!(size(h), 1000);
        assert_eq!(get_path(h), get_path(h)); // stable pointer
        assert_eq!(
            CStr::from_ptr(get_path(h)).to_str().unwrap(),
            path.to_str().unwrap()
        );

        let mut buf = [0u8; 16];
        assert_eq!(read(h, buf.as_mut_ptr() as *mut c_void, 16), 16);
        assert_eq!(&buf[..], &data[..16]);

        assert_eq!(seek(h, 990, RETRO_VFS_SEEK_POSITION_START as c_int), 0);
        // A short read at EOF is a count, not an error...
        assert_eq!(read(h, buf.as_mut_ptr() as *mut c_void, 16), 10);
        // ...and reading at EOF gives 0, which is how callers detect it.
        assert_eq!(read(h, buf.as_mut_ptr() as *mut c_void, 16), 0);

        assert_eq!(close(h), 0);
    }
}

#[test]
fn write_modes_truncate_unless_asked_to_preserve() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("save.srm");
    std::fs::write(&path, b"0123456789").unwrap();
    let cp = c(&path);

    unsafe {
        // WRITE alone discards what was there.
        let h = open(cp.as_ptr(), RETRO_VFS_FILE_ACCESS_WRITE, 0);
        assert!(!h.is_null());
        assert_eq!(write(h, b"ab".as_ptr() as *const c_void, 2), 2);
        assert_eq!(flush(h), 0);
        assert_eq!(close(h), 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"ab");

        // WRITE | UPDATE_EXISTING keeps it and writes over the front.
        std::fs::write(&path, b"0123456789").unwrap();
        let h = open(
            cp.as_ptr(),
            RETRO_VFS_FILE_ACCESS_WRITE | RETRO_VFS_FILE_ACCESS_UPDATE_EXISTING,
            0,
        );
        assert!(!h.is_null());
        assert_eq!(write(h, b"ab".as_ptr() as *const c_void, 2), 2);
        assert_eq!(truncate(h, 5), 0);
        assert_eq!(close(h), 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"ab234");

        // UPDATE_EXISTING on a file that isn't there fails rather than
        // creating one, matching the "r+b" the reference implementation uses.
        let missing = c(&dir.path().join("nope.srm"));
        assert!(
            open(
                missing.as_ptr(),
                RETRO_VFS_FILE_ACCESS_WRITE | RETRO_VFS_FILE_ACCESS_UPDATE_EXISTING,
                0,
            )
            .is_null()
        );
        // Neither READ nor WRITE is not a valid request.
        assert!(open(cp.as_ptr(), 0, 0).is_null());
    }
}

#[test]
fn stat_reports_size_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rom.bin");
    std::fs::write(&path, vec![7u8; 321]).unwrap();

    unsafe {
        let mut len = -1i32;
        let flags = stat(c(&path).as_ptr(), &mut len);
        assert_eq!(
            flags as u32 & RETRO_VFS_STAT_IS_VALID,
            RETRO_VFS_STAT_IS_VALID
        );
        assert_eq!(flags as u32 & RETRO_VFS_STAT_IS_DIRECTORY, 0);
        assert_eq!(len, 321);

        let flags = stat(c(dir.path()).as_ptr(), std::ptr::null_mut());
        assert_eq!(
            flags as u32 & (RETRO_VFS_STAT_IS_VALID | RETRO_VFS_STAT_IS_DIRECTORY),
            RETRO_VFS_STAT_IS_VALID | RETRO_VFS_STAT_IS_DIRECTORY
        );

        // A missing path is 0, not an error code — this is the call Stella
        // uses to decide a ROM path names a file at all.
        assert_eq!(
            stat(c(&dir.path().join("gone")).as_ptr(), std::ptr::null_mut()),
            0
        );
    }
}

#[test]
fn directories_list_once_and_then_stay_finished() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("b.rom"), b"x").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join(".hidden"), b"x").unwrap();

    unsafe {
        let d = opendir(c(dir.path()).as_ptr(), false);
        assert!(!d.is_null());
        let mut seen = Vec::new();
        while readdir(d) {
            let name = CStr::from_ptr(dirent_get_name(d))
                .to_str()
                .unwrap()
                .to_owned();
            seen.push((name, dirent_is_dir(d)));
        }
        seen.sort();
        assert_eq!(
            seen,
            vec![("b.rom".to_owned(), false), ("sub".to_owned(), true)]
        );
        // Past the end it stays past the end rather than wrapping around.
        assert!(!readdir(d));
        assert!(dirent_get_name(d).is_null());
        assert_eq!(closedir(d), 0);

        // Hidden entries appear only when asked for.
        let d = opendir(c(dir.path()).as_ptr(), true);
        let mut n = 0;
        while readdir(d) {
            n += 1;
        }
        assert_eq!(n, 3);
        assert_eq!(closedir(d), 0);

        assert!(opendir(c(&dir.path().join("gone")).as_ptr(), false).is_null());
    }
}

#[test]
fn mkdir_distinguishes_created_from_already_there() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("saves").join("nested");
    unsafe {
        assert_eq!(mkdir(c(&sub).as_ptr()), 0);
        // -2 rather than -1: callers branch on it to mean "fine, it exists".
        assert_eq!(mkdir(c(&sub).as_ptr()), -2);
    }
}

#[test]
fn rename_and_remove_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("a.bin");
    let to = dir.path().join("b.bin");
    std::fs::write(&from, b"x").unwrap();
    unsafe {
        assert_eq!(rename(c(&from).as_ptr(), c(&to).as_ptr()), 0);
        assert!(to.exists() && !from.exists());
        assert_eq!(remove(c(&to).as_ptr()), 0);
        assert!(!to.exists());
        assert_eq!(remove(c(&to).as_ptr()), -1);
    }
}

/// Every entry point has to survive the null and nonsense a core can hand
/// it, since a panic here would unwind into C.
#[test]
fn null_arguments_are_errors_not_crashes() {
    unsafe {
        assert!(open(std::ptr::null(), RETRO_VFS_FILE_ACCESS_READ, 0).is_null());
        assert!(get_path(std::ptr::null_mut()).is_null());
        assert_eq!(close(std::ptr::null_mut()), -1);
        assert_eq!(size(std::ptr::null_mut()), -1);
        assert_eq!(tell(std::ptr::null_mut()), -1);
        assert_eq!(seek(std::ptr::null_mut(), 0, 0), -1);
        assert_eq!(flush(std::ptr::null_mut()), -1);
        assert_eq!(truncate(std::ptr::null_mut(), 0), -1);
        assert_eq!(remove(std::ptr::null()), -1);
        assert_eq!(rename(std::ptr::null(), std::ptr::null()), -1);
        assert_eq!(stat(std::ptr::null(), std::ptr::null_mut()), 0);
        assert_eq!(mkdir(std::ptr::null()), -1);
        assert!(opendir(std::ptr::null(), false).is_null());
        assert!(!readdir(std::ptr::null_mut()));
        assert!(dirent_get_name(std::ptr::null_mut()).is_null());
        assert!(!dirent_is_dir(std::ptr::null_mut()));
        assert_eq!(closedir(std::ptr::null_mut()), -1);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.bin");
        std::fs::write(&path, b"xy").unwrap();
        let h = open_read(&path);
        // A null buffer is an error; a zero length is simply nothing to do.
        assert_eq!(read(h, std::ptr::null_mut(), 4), -1);
        assert_eq!(read(h, [0u8; 4].as_mut_ptr() as *mut c_void, 0), 0);
        assert_eq!(write(h, std::ptr::null(), 4), -1);
        assert_eq!(close(h), 0);

        // A directory is opendir's job, not open's.
        assert!(open(c(dir.path()).as_ptr(), RETRO_VFS_FILE_ACCESS_READ, 0).is_null());
    }
}

/// One shared table, handed out as often as asked.
#[test]
fn the_interface_is_built_once() {
    assert_eq!(interface(), interface());
    assert!(unsafe { (*interface()).stat }.is_some());
}
