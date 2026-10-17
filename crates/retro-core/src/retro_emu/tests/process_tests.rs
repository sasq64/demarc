use super::*;

fn pair() -> (Channel, Channel) {
    let (a, b) = UnixStream::pair().unwrap();
    (Channel::new(a), Channel::new(b))
}

fn shm_file(slots: usize) -> File {
    let file = shared_file().unwrap();
    file.set_len((HEADER_SIZE + slots * SLOT_SIZE) as u64).unwrap();
    file
}

#[test]
fn msg_round_trips() {
    let msg = Msg {
        tag: MSG_CLICK,
        a: 7,
        b: u32::MAX,
        c: 0x1234_5678,
        x: -1.5,
        y: f32::INFINITY,
    };
    let back = Msg::decode(&msg.encode());
    assert_eq!(back.tag, msg.tag);
    assert_eq!(back.a, msg.a);
    assert_eq!(back.b, msg.b);
    assert_eq!(back.c, msg.c);
    assert_eq!(back.x, msg.x);
    assert_eq!(back.y, msg.y);
}

#[test]
fn msg_to_cmd() {
    let cmd = |tag, a, b, c| Msg::new(tag, a, b, c).to_cmd();
    assert!(matches!(cmd(MSG_RESET, 0, 0, 0), Some(RetroCmd::Reset)));
    assert!(matches!(
        cmd(MSG_PRESS_KEY, 65, 1, 0x1_0003),
        Some(RetroCmd::PressKey { code: 65, down: true, mods: 3 })
    ));
    assert!(matches!(
        cmd(MSG_MOUSE_BUTTONS, 1, 0, 2),
        Some(RetroCmd::SetMouseButtons { left: true, right: false, middle: true })
    ));
    assert!(matches!(
        cmd(MSG_JOYPAD, 1, 4, 0),
        Some(RetroCmd::SetJoypad { port: 1, id: 4, down: false })
    ));
    assert!(matches!(cmd(MSG_SET_DISK, 2, 0, 0), Some(RetroCmd::SetDisk { no: 2 })));
    assert!(matches!(cmd(MSG_UNLOAD, 0, 0, 0), Some(RetroCmd::Unload)));
    assert!(matches!(cmd(MSG_SKIP, 30, 0, 0), Some(RetroCmd::Skip { frames: 30 })));

    let motion = Msg {
        tag: MSG_MOUSE_MOTION,
        x: 2.0,
        y: -3.0,
        ..Default::default()
    };
    assert!(matches!(
        motion.to_cmd(),
        Some(RetroCmd::AddMouseMotion { dx: 2.0, dy: -3.0 })
    ));
    let position = Msg {
        tag: MSG_MOUSE_POSITION,
        x: 0.25,
        y: 0.75,
        ..Default::default()
    };
    assert!(matches!(
        position.to_cmd(),
        Some(RetroCmd::SetMousePosition { x: 0.25, y: 0.75 })
    ));

    // Handled by the worker loop itself, never as a `RetroCmd`.
    for tag in [MSG_SEND_EVENTS, MSG_KEY, MSG_RELEASE, MSG_CLICK, MSG_LATENCY, MSG_FRAME, 0] {
        assert!(cmd(tag, 0, 0, 0).is_none(), "tag {tag}");
    }
}

#[test]
fn focus_matches_backend_encoding() {
    let focus = |a| match Msg::new(MSG_FOCUS, a, 0, 0).to_cmd() {
        Some(RetroCmd::SetFocus { focus }) => focus,
        _ => panic!("not a focus command"),
    };
    assert_eq!(focus(0), ViewFocus::Invisible);
    assert_eq!(focus(1), ViewFocus::Visible);
    assert_eq!(focus(2), ViewFocus::Focus);
}

#[test]
fn channel_passes_messages_in_order() {
    let (mut a, mut b) = pair();
    for i in 0..100 {
        a.send(Msg::new(MSG_FRAME, i, 0, 0)).unwrap();
    }
    for i in 0..100 {
        let msg = b.recv_blocking().unwrap();
        assert_eq!((msg.tag, msg.a), (MSG_FRAME, i));
    }
    assert!(b.recv(false).unwrap().is_none());
}

#[test]
fn channel_waits_for_a_whole_message() {
    let (mut a, mut b) = pair();
    let bytes = Msg::new(MSG_SKIP, 9, 0, 0).encode();
    a.stream.write_all(&bytes[..10]).unwrap();
    assert!(b.recv(false).unwrap().is_none());
    a.stream.write_all(&bytes[10..]).unwrap();
    let msg = b.recv(false).unwrap().unwrap();
    assert_eq!((msg.tag, msg.a), (MSG_SKIP, 9));
}

#[test]
fn channel_reports_closed_peer() {
    let (a, mut b) = pair();
    drop(a);
    let Err(err) = b.recv(false) else {
        panic!("recv from a closed peer succeeded")
    };
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn slots_do_not_overlap() {
    let slots = 3;
    let shm = Shm::map(&shm_file(slots), slots).unwrap();
    let base = shm.ptr as usize;
    assert!(base + HEADER_SIZE <= shm.slot(0) as usize);
    for i in 0..slots {
        let slot = shm.slot(i) as usize;
        let pixels = shm.pixels(i) as usize;
        let audio = shm.audio(i) as usize;
        assert_eq!(slot % 8, 0);
        assert!(slot + size_of::<SlotHeader>() <= pixels);
        assert_eq!(audio - pixels, MAX_PIXELS * 4);
        assert!(audio + MAX_AUDIO * 2 <= base + HEADER_SIZE + (i + 1) * SLOT_SIZE);
    }
    assert_eq!(HEADER_SIZE + slots * SLOT_SIZE, shm.len);
}

#[test]
#[should_panic]
fn slot_out_of_range_panics() {
    let shm = Shm::map(&shm_file(2), 2).unwrap();
    shm.slot(2);
}

#[test]
fn mappings_share_memory() {
    let file = shm_file(2);
    let a = Shm::map(&file, 2).unwrap();
    let b = Shm::map(&file, 2).unwrap();
    a.atomics().0.store(42, Ordering::Relaxed);
    unsafe { *a.pixels(1).add(100) = 0xdead_beef };
    assert_eq!(b.atomics().0.load(Ordering::Relaxed), 42);
    assert_eq!(unsafe { *b.pixels(1).add(100) }, 0xdead_beef);
}

#[test]
fn worker_reports_setup_error() {
    let slots = 2;
    let file = shm_file(slots);
    let shm = Shm::map(&file, slots).unwrap();
    let (ours, theirs) = UnixStream::pair().unwrap();
    let worker_file = file.try_clone().unwrap();
    let worker = std::thread::spawn(move || run_worker(theirs, worker_file));

    let mut chan = Channel::new(ours);
    let meta = HashMap::from([("info".to_string(), "x".to_string())]);
    send_setup(
        &mut chan,
        Path::new("/nonexistent/core_libretro.so"),
        Path::new("/nonexistent"),
        None,
        &meta,
        false,
        slots,
    )
    .unwrap();
    assert_eq!(chan.recv_blocking().unwrap().tag, MSG_SETUP_ERR);
    worker.join().unwrap().unwrap();

    let h = unsafe { &*shm.header() };
    assert!(h.error_len > 0);
    assert!(h.error_len as usize <= ERROR_LEN);
}

#[test]
fn worker_rejects_truncated_setup() {
    let (mut ours, theirs) = UnixStream::pair().unwrap();
    let worker = std::thread::spawn(move || run_worker(theirs, shm_file(1)));
    let blob = b"core\0system";
    ours.write_all(&(blob.len() as u32).to_ne_bytes()).unwrap();
    ours.write_all(blob).unwrap();
    let err = worker.join().unwrap().unwrap_err();
    assert!(err.to_string().contains("truncated setup"), "{err}");
}
