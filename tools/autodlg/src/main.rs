//! Drives the setup dialog nearly every PC demo shows before it starts.
//!
//! Runs inside the same wine prefix/desktop as the demo, so `EnumWindows` sees
//! the demo's windows. Walks the Win32 control tree rather than clicking
//! pixels: that way the resolution can be *chosen* (demarc wants a fixed
//! capture size) instead of whatever the demo defaults to, and options like
//! "Fullscreen" can be turned off by name on the demos that offer them.
//!
//! Real dialogs vary more than they look. Resolution is a radio group in some
//! (Equinox, We Cell) and a combo box in others (Conspiracy), and button labels
//! carry decoration — `GO!`, `&Run` — so every comparison goes through `norm`.
//!
//! Everything printed here is for whoever is reading the log, bar three lines:
//! see [`report`], which is how demarc learns what the demo is doing.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

type Hwnd = isize;
type Bool32 = i32;

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(cb: unsafe extern "system" fn(Hwnd, isize) -> Bool32, l: isize) -> Bool32;
    fn EnumChildWindows(p: Hwnd, cb: unsafe extern "system" fn(Hwnd, isize) -> Bool32, l: isize) -> Bool32;
    fn IsWindowVisible(h: Hwnd) -> Bool32;
    fn GetWindowTextW(h: Hwnd, buf: *mut u16, n: i32) -> i32;
    fn GetClassNameW(h: Hwnd, buf: *mut u16, n: i32) -> i32;
    fn GetWindowLongW(h: Hwnd, idx: i32) -> i32;
    fn GetWindowThreadProcessId(h: Hwnd, pid: *mut u32) -> u32;
    fn GetDlgCtrlID(h: Hwnd) -> i32;
    fn GetParent(h: Hwnd) -> Hwnd;
    fn GetWindowLongPtrW(h: Hwnd, idx: i32) -> isize;
    fn SetWindowLongPtrW(h: Hwnd, idx: i32, v: isize) -> isize;
    fn GetClientRect(h: Hwnd, r: *mut Rect) -> Bool32;
    fn SetWindowPos(h: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32)
    -> Bool32;
    fn SendMessageW(h: Hwnd, msg: u32, w: usize, l: isize) -> isize;
    fn PostMessageW(h: Hwnd, msg: u32, w: usize, l: isize) -> Bool32;
    fn SetForegroundWindow(h: Hwnd) -> Bool32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn Sleep(ms: u32);
    fn CreateProcessW(
        app: *const u16,
        cmdline: *mut u16,
        proc_attr: *mut u8,
        thread_attr: *mut u8,
        inherit: Bool32,
        flags: u32,
        env: *mut u8,
        cwd: *const u16,
        si: *mut StartupInfoW,
        pi: *mut ProcessInformation,
    ) -> Bool32;
    fn WaitForSingleObject(h: isize, ms: u32) -> u32;
    fn CloseHandle(h: isize) -> Bool32;
}

#[repr(C)]
#[derive(Default)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    reserved2: u16,
    reserved2_ptr: *mut u8,
    stdin: isize,
    stdout: isize,
    stderr: isize,
}

#[repr(C)]
#[derive(Default)]
struct ProcessInformation {
    process: isize,
    thread: isize,
    process_id: u32,
    thread_id: u32,
}

const INFINITE: u32 = 0xFFFF_FFFF;

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

const GWL_EXSTYLE: i32 = -20;
/// Everything that draws a frame around a window, leaving only its client area.
const WS_DECORATION: isize = 0x00C0_0000  // WS_CAPTION (WS_BORDER | WS_DLGFRAME)
    | 0x0004_0000  // WS_THICKFRAME
    | 0x0008_0000  // WS_SYSMENU
    | 0x0002_0000  // WS_MINIMIZEBOX
    | 0x0001_0000; // WS_MAXIMIZEBOX
const WS_EX_DECORATION: isize = 0x0000_0200  // WS_EX_CLIENTEDGE
    | 0x0000_0100  // WS_EX_WINDOWEDGE
    | 0x0000_0001; // WS_EX_DLGMODALFRAME
const SWP_FRAMECHANGED: u32 = 0x0020;
const SWP_NOZORDER: u32 = 0x0004;

/// Strip a window's frame and move its client area to the desktop origin.
///
/// Inside a virtual desktop sized to the capture, a demo's window is the right
/// size but in the wrong place and wearing a title bar — and both land in the
/// captured frame, along with the wine desktop background around them. Only the
/// frame is removed and the window moved: the client area keeps the size the
/// demo chose, so its back buffer is never resized under it and nothing is
/// stretched or re-created.
fn fill_desktop(hwnd: Hwnd) {
    let mut rc = Rect::default();
    unsafe {
        if GetClientRect(hwnd, &mut rc) == 0 {
            return;
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style & !WS_DECORATION);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !WS_EX_DECORATION);
        SetWindowPos(
            hwnd,
            0,
            0,
            0,
            rc.right - rc.left,
            rc.bottom - rc.top,
            SWP_FRAMECHANGED | SWP_NOZORDER,
        );
    }
    println!(
        "undecorated the demo window ({}x{}) at the desktop origin",
        rc.right - rc.left,
        rc.bottom - rc.top
    );
}

/// Prefix of the lines demarc reads rather than logs.
const SENTINEL: &str = "!demarc ";

/// Tell demarc what the demo is doing: `started`, `exited`, or `failed`.
///
/// demarc cannot see any of this for itself. It launches one process — a
/// gamescope with wine inside it — and that process outlives the demo by a long
/// way: gamescope waits on its whole tree, and wine's services (`wineserver`,
/// `services.exe`, `winedevice.exe`) put themselves in sessions of their own and
/// go on running for the life of the prefix. So a gamescope that has finished
/// looks exactly like one that is still showing a demo. This driver is the one
/// thing in there that knows: it starts the demo and holds its handle.
///
/// Written to stdout, which wine passes through to the pipe demarc is draining,
/// a line at a time — that is what the reader on the other end splits on.
fn report(event: &str) {
    println!("{SENTINEL}{event}");
}

/// When non-zero, only windows belonging to this process are considered — set
/// by `--launch` so the driver can never answer some unrelated dialog.
static TARGET_PID: AtomicU32 = AtomicU32::new(0);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Start `exe` (in its own directory) and return its process handle and pid.
///
/// Launching the demo from here rather than from the shell is what lets both it
/// and this driver share one `explorer /desktop=` virtual desktop: the desktop
/// hosts a single command, and a child process inherits it.
fn launch(exe: &str) -> Option<(isize, u32)> {
    let dir = std::path::Path::new(exe)
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut cmdline = wide(&format!("\"{exe}\""));
    let app = wide(exe);
    let cwd = wide(&dir);
    let mut si = StartupInfoW {
        cb: std::mem::size_of::<StartupInfoW>() as u32,
        ..Default::default()
    };
    let mut pi = ProcessInformation::default();
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            cmdline.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            0,
            std::ptr::null_mut(),
            if dir.is_empty() {
                std::ptr::null()
            } else {
                cwd.as_ptr()
            },
            &mut si,
            &mut pi,
        )
    };
    if ok == 0 {
        return None;
    }
    unsafe { CloseHandle(pi.thread) };
    Some((pi.process, pi.process_id))
}

const GWL_STYLE: i32 = -16;
const BM_GETCHECK: u32 = 0x00F0;
const BM_CLICK: u32 = 0x00F5;
const BST_CHECKED: isize = 1;
const CB_GETCURSEL: u32 = 0x0147;
const CB_GETCOUNT: u32 = 0x0146;
const CB_GETLBTEXT: u32 = 0x0148;
const CB_GETLBTEXTLEN: u32 = 0x0149;
const CB_SETCURSEL: u32 = 0x014E;
const CBN_SELCHANGE: usize = 1;
const WM_COMMAND: u32 = 0x0111;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const VK_RETURN: usize = 0x0D;

fn wtext(f: unsafe extern "system" fn(Hwnd, *mut u16, i32) -> i32, h: Hwnd) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { f(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Uppercase alphanumerics only.
///
/// Labels are decorated in every direction — `&Run` carries an accelerator,
/// `GO!` an exclamation mark, `-= START =-` whatever the artist felt like — and
/// none of that changes which button it is. Stripping it all lets the match
/// stay an exact comparison instead of a substring guess that would let `OK`
/// hit a `NOT OK`.
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum Kind {
    Push,
    Check,
    Radio,
    /// A group box: a `Button` by class, but clicking it does nothing useful.
    Group,
    Combo,
    Other,
}

struct Ctl {
    hwnd: Hwnd,
    class: String,
    text: String,
    kind: Kind,
}

impl Ctl {
    fn new(hwnd: Hwnd) -> Self {
        let class = wtext(GetClassNameW, hwnd);
        let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) };
        // Low nibble of a button's style says which kind it is.
        let kind = if class.eq_ignore_ascii_case("Button") {
            match style & 0x0F {
                0 | 1 => Kind::Push,
                2 | 3 => Kind::Check,
                4 | 5 | 8 | 9 => Kind::Radio,
                7 => Kind::Group,
                _ => Kind::Other,
            }
        } else if class.eq_ignore_ascii_case("ComboBox") {
            Kind::Combo
        } else {
            Kind::Other
        };
        Self {
            hwnd,
            class,
            text: wtext(GetWindowTextW, hwnd),
            kind,
        }
    }

    fn checked(&self) -> bool {
        unsafe { SendMessageW(self.hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED }
    }

    fn click(&self) {
        unsafe { SendMessageW(self.hwnd, BM_CLICK, 0, 0) };
    }

    /// The entries of a combo box, in order.
    fn items(&self) -> Vec<String> {
        let count = unsafe { SendMessageW(self.hwnd, CB_GETCOUNT, 0, 0) };
        (0..count.max(0))
            .map(|i| {
                let len = unsafe { SendMessageW(self.hwnd, CB_GETLBTEXTLEN, i as usize, 0) };
                let mut buf = vec![0u16; (len.max(0) as usize) + 1];
                let n = unsafe {
                    SendMessageW(self.hwnd, CB_GETLBTEXT, i as usize, buf.as_mut_ptr() as isize)
                };
                String::from_utf16_lossy(&buf[..n.max(0) as usize])
            })
            .collect()
    }

    /// Select combo box entry `index`, and tell the dialog it changed — some
    /// demos track the selection from the notification rather than reading it
    /// back when the start button is pressed.
    fn select(&self, index: usize) {
        unsafe {
            SendMessageW(self.hwnd, CB_SETCURSEL, index, 0);
            let id = GetDlgCtrlID(self.hwnd) as usize & 0xffff;
            SendMessageW(
                GetParent(self.hwnd),
                WM_COMMAND,
                id | (CBN_SELCHANGE << 16),
                self.hwnd,
            );
        }
    }
}

unsafe extern "system" fn collect(h: Hwnd, l: isize) -> Bool32 {
    let v = unsafe { &mut *(l as *mut Vec<Ctl>) };
    v.push(Ctl::new(h));
    1
}

unsafe extern "system" fn collect_top(h: Hwnd, l: isize) -> Bool32 {
    if unsafe { IsWindowVisible(h) } == 0 {
        return 1;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    if pid == unsafe { GetCurrentProcessId() } {
        return 1;
    }
    let target = TARGET_PID.load(Ordering::Relaxed);
    if target != 0 && pid != target {
        return 1;
    }
    unsafe { collect(h, l) }
}

fn children(parent: Hwnd) -> Vec<Ctl> {
    let mut v: Vec<Ctl> = Vec::new();
    unsafe { EnumChildWindows(parent, collect, &mut v as *mut _ as isize) };
    v
}

fn top_levels() -> Vec<Ctl> {
    let mut v: Vec<Ctl> = Vec::new();
    unsafe { EnumWindows(collect_top, &mut v as *mut _ as isize) };
    v
}

struct Args {
    /// Executable to start before looking for a dialog. Doing this here rather
    /// than from the shell keeps the demo inside whatever virtual desktop this
    /// driver was launched into.
    launch: Option<String>,
    list: bool,
    no_fallback: bool,
    /// Leave the demo's window as it is, frame and all.
    no_fill: bool,
    /// Touch no dialog at all: launch the demo, then only watch it.
    ///
    /// What `wine_res=pick` asks for. The driver is still here — it is what
    /// starts the demo, and what tells demarc when the demo is over — but the
    /// dialog belongs to whoever is sitting in front of it.
    no_go: bool,
    timeout: f64,
    prefer: Vec<String>,
    check: Vec<String>,
    uncheck: Vec<String>,
    go: Vec<String>,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut args = Args {
        launch: None,
        list: false,
        no_fallback: false,
        no_fill: false,
        no_go: false,
        timeout: 20.0,
        prefer: vec![],
        check: vec![],
        uncheck: vec![],
        go: ["RUN", "OK", "START", "GO", "LAUNCH", "PLAY", "YES"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    let mut i = 0;
    while i < argv.len() {
        let name = argv[i].as_str();
        let value = argv.get(i + 1).cloned().unwrap_or_default();
        // Flags consume one argv slot, options two.
        let mut takes_value = true;
        match name {
            "--list" => {
                args.list = true;
                takes_value = false;
            }
            "--no-fallback" => {
                args.no_fallback = true;
                takes_value = false;
            }
            "--no-fill" => {
                args.no_fill = true;
                takes_value = false;
            }
            "--no-go" => {
                args.no_go = true;
                takes_value = false;
            }
            "--launch" => args.launch = Some(value),
            "--timeout" => args.timeout = value.parse().unwrap_or(20.0),
            // Substring of a radio button or combo box entry to select.
            "--prefer" => args.prefer.push(value),
            "--check" => args.check.push(value),
            "--uncheck" => args.uncheck.push(value),
            "--go" => args.go = value.split(',').map(|s| s.to_string()).collect(),
            other => {
                eprintln!("unknown arg: {other}");
                takes_value = false;
            }
        }
        i += if takes_value { 2 } else { 1 };
    }
    args
}

fn print_tree(top: &Ctl, kids: &[Ctl]) {
    println!(
        "window hwnd={:#x} class={:?} text={:?}",
        top.hwnd, top.class, top.text
    );
    for c in kids {
        let state = match c.kind {
            Kind::Check | Kind::Radio => {
                if c.checked() {
                    " [x]".to_string()
                } else {
                    " [ ]".to_string()
                }
            }
            Kind::Combo => {
                let sel = unsafe { SendMessageW(c.hwnd, CB_GETCURSEL, 0, 0) };
                format!(" items={:?} selected={sel}", c.items())
            }
            _ => String::new(),
        };
        println!(
            "  hwnd={:#x} class={:?} kind={:?} text={:?}{state}",
            c.hwnd, c.class, c.kind, c.text
        );
    }
}

/// Apply every requested option to one dialog, then press its start button.
/// Returns false if there was no button to press and no fallback was sent.
fn drive(top: &Ctl, kids: &[Ctl], args: &Args) -> bool {
    unsafe { SetForegroundWindow(top.hwnd) };

    // Options first — resolution, and switches like Fullscreen — so they are
    // all in place before anything starts the demo.
    for want in args.prefer.iter().map(|p| norm(p)) {
        for c in kids {
            match c.kind {
                Kind::Radio if norm(&c.text).contains(&want) => {
                    if !c.checked() {
                        println!("select {:?}", c.text);
                        c.click();
                    }
                }
                Kind::Combo => {
                    let items = c.items();
                    if let Some(i) = items.iter().position(|it| norm(it).contains(&want)) {
                        println!("select {:?} from combo box", items[i]);
                        c.select(i);
                    }
                }
                _ => {}
            }
        }
    }
    for (wanted, labels) in [(true, &args.check), (false, &args.uncheck)] {
        for want in labels.iter().map(|l| norm(l)) {
            for c in kids {
                if c.kind == Kind::Check && norm(&c.text).contains(&want) && c.checked() != wanted {
                    println!("{} {:?}", if wanted { "check" } else { "uncheck" }, c.text);
                    c.click();
                }
            }
        }
    }

    let go: Vec<String> = args.go.iter().map(|g| norm(g)).collect();
    if let Some(c) = kids
        .iter()
        .filter(|c| c.kind == Kind::Push)
        .find(|c| go.contains(&norm(&c.text)))
    {
        println!("click {:?}", c.text);
        c.click();
        return true;
    }
    if args.no_fallback {
        return false;
    }
    // No recognisable label: press the dialog's default button instead.
    println!("no matching button, sending Return");
    unsafe {
        PostMessageW(top.hwnd, WM_KEYDOWN, VK_RETURN, 0);
        PostMessageW(top.hwnd, WM_KEYUP, VK_RETURN, 0);
    }
    true
}

/// Wait for the demo's render window to appear, then undecorate it.
///
/// "The render window" is the first visible top-level the demo owns that is not
/// a dialog — dialogs are the ones carrying push buttons. It is worth waiting
/// for: a demo typically destroys its setup dialog and creates the real window
/// a moment later, and some do it more than once while they pick a mode.
fn wait_and_fill(timeout: f64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs_f64(timeout) {
        for top in top_levels() {
            if children(top.hwnd).iter().any(|c| c.kind == Kind::Push) {
                continue;
            }
            fill_desktop(top.hwnd);
            return;
        }
        unsafe { Sleep(200) };
    }
    eprintln!("no render window appeared to undecorate");
}

fn main() {
    let args = parse_args();
    let demo = args.launch.as_ref().map(|exe| {
        let Some((handle, pid)) = launch(exe) else {
            eprintln!("could not start {exe}");
            report("failed");
            std::process::exit(1);
        };
        println!("launched {exe} as pid {pid}");
        report("started");
        TARGET_PID.store(pid, Ordering::Relaxed);
        handle
    });

    let start = Instant::now();
    let mut driven = false;
    while !args.no_go && !driven && start.elapsed() < Duration::from_secs_f64(args.timeout) {
        for top in top_levels() {
            let kids = children(top.hwnd);
            // A dialog worth driving has at least one button on it; this skips
            // splash windows and the demo's own render window.
            if !kids.iter().any(|c| c.kind == Kind::Push) {
                continue;
            }
            if args.list {
                print_tree(&top, &kids);
            } else {
                drive(&top, &kids, &args);
            }
            driven = true;
            break;
        }
        unsafe { Sleep(100) };
    }
    if !args.no_go && !driven {
        eprintln!("no dialog appeared within {}s", args.timeout);
    }

    if !args.list && !args.no_fill {
        wait_and_fill(args.timeout);
    }

    // Outlive the demo, and say so when it goes: this is the only place that
    // can — see `report`. Without a launch there is nothing to wait for.
    match demo {
        Some(handle) => {
            unsafe { WaitForSingleObject(handle, INFINITE) };
            report("exited");
        }
        None if !driven => std::process::exit(1),
        None => {}
    }
}
