//! Drives the setup dialog nearly every PC demo shows before it starts.
//!
//! Runs inside the same wine prefix/desktop as the demo, so `EnumWindows` sees
//! the demo's windows. Walks the Win32 control tree rather than clicking
//! pixels: that way the resolution can be *chosen* (demarc wants a fixed
//! capture size) instead of whatever the demo defaults to, and options like
//! "Fullscreen" can be turned off by name on the demos that offer them.
//!
//! Real dialogs vary more than they look. Resolution is a radio group in some
//! (Equinox, We Cell), a combo box in others (Conspiracy), a *list* box in
//! others still (Haujobb's *You Should*), and in others again (Fairlight's
//! *Panic Room*) a row of *checkboxes* the demo makes exclusive itself — see
//! [`drive`]. Button labels carry decoration — `GO!`, `&Run` — so every
//! comparison goes through `norm`.
//!
//! Everything printed here is for whoever is reading the log, bar the lines
//! [`report`] writes, which is how demarc learns what the demo is doing.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

type Hwnd = isize;
type Bool32 = i32;

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(cb: unsafe extern "system" fn(Hwnd, isize) -> Bool32, l: isize) -> Bool32;
    fn EnumChildWindows(
        p: Hwnd,
        cb: unsafe extern "system" fn(Hwnd, isize) -> Bool32,
        l: isize,
    ) -> Bool32;
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
    fn SetWindowPos(h: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> Bool32;
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
    fn ResumeThread(h: isize) -> u32;
    fn VirtualAllocEx(h: isize, addr: usize, size: usize, kind: u32, protect: u32) -> usize;
    fn ReadProcessMemory(h: isize, addr: usize, buf: *mut u8, size: usize, read: *mut usize) -> Bool32;
    fn WriteProcessMemory(h: isize, addr: usize, buf: *const u8, size: usize, written: *mut usize) -> Bool32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
    fn Process32FirstW(snap: isize, entry: *mut ProcessEntry32W) -> Bool32;
    fn Process32NextW(snap: isize, entry: *mut ProcessEntry32W) -> Bool32;
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn GetProcAddress(module: isize, name: *const u8) -> usize;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegSetKeyValueW(
        key: isize,
        subkey: *const u16,
        name: *const u16,
        kind: u32,
        data: *const u8,
        len: u32,
    ) -> i32;
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
struct ProcessEntry32W {
    size: u32,
    usage: u32,
    process_id: u32,
    heap_id: usize,
    module_id: u32,
    threads: u32,
    parent_process_id: u32,
    pri_class_base: i32,
    flags: u32,
    exe_file: [u16; 260],
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
const CREATE_SUSPENDED: u32 = 4;
const MEM_COMMIT_RESERVE: u32 = 0x3000;
const PAGE_READWRITE: u32 = 4;

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

/// Tell demarc what the demo is doing: `started`, `exited` or `failed`, and
/// `hiding`/`visible` around the setup dialog, which the gamescope core turns
/// into black frames so nobody has to watch the dialog being answered.
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

/// `root` and every process it started, directly or not: Adelanto's dialog
/// is a separate `e3d_setup.exe` the demo runs.
fn process_tree(root: u32) -> Vec<u32> {
    const TH32CS_SNAPPROCESS: u32 = 2;
    let mut parents = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == -1 {
            return vec![root];
        }
        let mut e: ProcessEntry32W = std::mem::zeroed();
        e.size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            parents.push((e.process_id, e.parent_process_id));
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    let mut tree = vec![root];
    let mut i = 0;
    while i < tree.len() {
        for &(child, parent) in &parents {
            if parent == tree[i] && !tree.contains(&child) {
                tree.push(child);
            }
        }
        i += 1;
    }
    tree
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn read_u32s(process: isize, addr: u32, count: usize) -> Option<Vec<u32>> {
    let mut buf = vec![0u8; count * 4];
    let ok = unsafe {
        ReadProcessMemory(process, addr as usize, buf.as_mut_ptr(), buf.len(), std::ptr::null_mut())
    };
    (ok != 0).then(|| {
        buf.chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    })
}

/// Windows reads an import's thunks from the IAT when `OriginalFirstThunk`
/// points outside the image, which a 1k intro's overlapping header relies on;
/// wine follows the pointer. Maps a copy of the IAT where it points.
fn fix_import_thunks(process: isize, exe: &str) -> Option<()> {
    let file = std::fs::read(exe).ok()?;
    let at = |offset: usize| {
        let bytes = file.get(offset..offset + 4)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };
    let opt = at(0x3C)? as usize + 24;
    if file.get(opt..opt + 2)? != [0x0B, 0x01] {
        return None;
    }
    let (base, image_size, headers_size) = (at(opt + 28)?, at(opt + 56)?, at(opt + 60)?);
    let mut descriptor = base.wrapping_add(at(opt + 104)?);
    for _ in 0..64 {
        let d = read_u32s(process, descriptor, 5)?;
        let (lookup, name, iat) = (d[0], d[3], d[4]);
        if name == 0 || iat == 0 {
            break;
        }
        descriptor = descriptor.wrapping_add(20);
        if lookup == 0 || (headers_size..image_size).contains(&lookup) {
            continue;
        }
        let mut thunks = Vec::new();
        while thunks.len() < 4096 {
            let thunk = read_u32s(process, base.wrapping_add(iat).wrapping_add(thunks.len() as u32), 1)?;
            thunks.extend(thunk[0].to_le_bytes());
            if thunk[0] == 0 {
                break;
            }
        }
        let target = base.wrapping_add(lookup) as usize;
        let page = target & !0xFFFF;
        let done = unsafe {
            VirtualAllocEx(process, page, target - page + thunks.len(), MEM_COMMIT_RESERVE, PAGE_READWRITE) != 0
                && WriteProcessMemory(process, target, thunks.as_ptr(), thunks.len(), std::ptr::null_mut()) != 0
        };
        println!("import thunks at {target:#x} outside the image: {}", if done { "mapped" } else { "not mapped" });
    }
    Some(())
}

/// Start `exe` (in its own directory) and return its process handle and pid.
///
/// Launching the demo from here rather than from the shell is what lets both it
/// and this driver share one `explorer /desktop=` virtual desktop: the desktop
/// hosts a single command, and a child process inherits it.
fn launch(exe: &str) -> Option<(isize, u32)> {
    let exe = &dos_path(exe);
    let dir = std::path::Path::new(exe)
        .parent()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut cmdline = wide(&format!("\"{exe}\""));
    let app = wide(exe);
    let cwd = wide(&dir);
    // Some demos hand wShowWindow straight to ShowWindow without checking the
    // flag, and a zero there hides their setup dialog (winnerdemo.exe).
    let mut si = StartupInfoW {
        cb: std::mem::size_of::<StartupInfoW>() as u32,
        flags: STARTF_USESHOWWINDOW,
        show_window: SW_SHOWNORMAL,
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
            CREATE_SUSPENDED,
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
    fix_import_thunks(pi.process, exe);
    unsafe {
        ResumeThread(pi.thread);
        CloseHandle(pi.thread);
    }
    Some((pi.process, pi.process_id))
}

/// `path` as wine's DOS name when it is a Unix one. A demo that restarts itself
/// from argv[0] needs a Windows path there (Monstra Kosa).
fn dos_path(path: &str) -> String {
    if !path.starts_with('/') {
        return path.to_string();
    }
    let unix = format!("{path}\0");
    let dos = unsafe {
        let get = GetProcAddress(GetModuleHandleW(wide("kernel32").as_ptr()), c"wine_get_dos_file_name".as_ptr().cast());
        if get == 0 {
            return path.to_string();
        }
        let get: unsafe extern "system" fn(*const u8) -> *const u16 = std::mem::transmute(get);
        get(unix.as_ptr())
    };
    if dos.is_null() {
        return path.to_string();
    }
    let len = (0..).take_while(|&i| unsafe { *dos.add(i) } != 0).count();
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(dos, len) })
}

/// Turn on wine's `EmulateModeset` for the demo about to be launched. The
/// sandboxed prefix is thrown away afterwards, so the write does not outlive the run.
fn emulate_modeset() {
    const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
    const REG_SZ: u32 = 1;
    let key = wide(r"Software\Wine\X11 Driver");
    let name = wide("EmulateModeset");
    let value = wide("Y");
    let err = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            value.as_ptr() as *const u8,
            (value.len() * 2) as u32,
        )
    };
    if err != 0 {
        eprintln!("could not set EmulateModeset: error {err}");
    }
}

const STARTF_USESHOWWINDOW: u32 = 0x1;
const SW_SHOWNORMAL: u16 = 1;
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
const LB_GETCURSEL: u32 = 0x0188;
const LB_GETCOUNT: u32 = 0x018B;
const LB_GETTEXT: u32 = 0x0189;
const LB_GETTEXTLEN: u32 = 0x018A;
const LB_SETCURSEL: u32 = 0x0186;
const LBN_SELCHANGE: usize = 1;
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
        // Adelanto's setup offers `640*480`.
        .map(|c| if c == '*' { 'x' } else { c })
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
    /// A list box, which is a combo box that is already open: same list, same
    /// one-of-many selection, a different set of message numbers — see
    /// [`Ctl::list_msgs`]. Haujobb's *You Should* offers its resolutions in one.
    List,
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
        // .NET dialogs (Still's *Square*) name their controls
        // `WindowsForms10.BUTTON.app.0.<hash>`.
        let base = class
            .strip_prefix("WindowsForms10.")
            .and_then(|rest| rest.split('.').next())
            .unwrap_or(&class);
        // Delphi's VCL subclasses the stock controls under names of its own.
        let base = match base {
            "TButton" | "TBitBtn" | "TCheckBox" | "TRadioButton" | "TGroupBox" => "Button",
            "TComboBox" => "ComboBox",
            "TListBox" => "ListBox",
            other => other,
        };
        // Low nibble of a button's style says which kind it is.
        let kind = if base.eq_ignore_ascii_case("Button") {
            match style & 0x0F {
                // WinForms owner-draws every button, check boxes included,
                // so those cannot be told apart.
                0 | 1 | 0xB => Kind::Push,
                2 | 3 | 5 | 6 => Kind::Check,
                4 | 8 | 9 => Kind::Radio,
                7 => Kind::Group,
                _ => Kind::Other,
            }
        } else if base.eq_ignore_ascii_case("ComboBox") {
            Kind::Combo
        } else if base.eq_ignore_ascii_case("ListBox") {
            Kind::List
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

    fn is_default_push(&self) -> bool {
        self.kind == Kind::Push && unsafe { GetWindowLongW(self.hwnd, GWL_STYLE) } & 0x0F == 1
    }

    fn checked(&self) -> bool {
        unsafe { SendMessageW(self.hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED }
    }

    fn click(&self) {
        unsafe { SendMessageW(self.hwnd, BM_CLICK, 0, 0) };
    }

    /// The messages that read and write a list of entries, for the two controls
    /// that hold one.
    ///
    /// A combo box and a list box are the same control asked a different way —
    /// one list, one selected entry — and the only thing that differs between
    /// them is the numbers: `CB_GETCOUNT` against `LB_GETCOUNT`, and so on down
    /// the line. So the numbers are all that is picked here, and [`items`] and
    /// [`select`] are written once.
    ///
    /// `(count, textlen, text, setcursel, selchange)`, or nothing for a control
    /// that is not a list at all.
    ///
    /// [`items`]: Ctl::items
    /// [`select`]: Ctl::select
    fn list_msgs(&self) -> Option<(u32, u32, u32, u32, usize)> {
        match self.kind {
            Kind::Combo => Some((
                CB_GETCOUNT,
                CB_GETLBTEXTLEN,
                CB_GETLBTEXT,
                CB_SETCURSEL,
                CBN_SELCHANGE,
            )),
            Kind::List => Some((
                LB_GETCOUNT,
                LB_GETTEXTLEN,
                LB_GETTEXT,
                LB_SETCURSEL,
                LBN_SELCHANGE,
            )),
            _ => None,
        }
    }

    /// The entries of a combo box or list box, in order. Empty for anything
    /// else — asking a push button for its list is a question with no answer,
    /// not an error.
    fn items(&self) -> Vec<String> {
        let Some((get_count, get_len, get_text, _, _)) = self.list_msgs() else {
            return Vec::new();
        };
        let count = unsafe { SendMessageW(self.hwnd, get_count, 0, 0) };
        (0..count.max(0))
            .map(|i| {
                let len = unsafe { SendMessageW(self.hwnd, get_len, i as usize, 0) };
                let mut buf = vec![0u16; (len.max(0) as usize) + 1];
                let n = unsafe {
                    SendMessageW(self.hwnd, get_text, i as usize, buf.as_mut_ptr() as isize)
                };
                String::from_utf16_lossy(&buf[..n.max(0) as usize])
            })
            .collect()
    }

    /// Which entry is selected, or -1 for none.
    fn selected(&self) -> isize {
        match self.kind {
            Kind::Combo => unsafe { SendMessageW(self.hwnd, CB_GETCURSEL, 0, 0) },
            Kind::List => unsafe { SendMessageW(self.hwnd, LB_GETCURSEL, 0, 0) },
            _ => -1,
        }
    }

    /// Select entry `index`, and tell the dialog it changed — some demos track
    /// the selection from the notification rather than reading it back when the
    /// start button is pressed.
    fn select(&self, index: usize) {
        let Some((_, _, _, set_cursel, selchange)) = self.list_msgs() else {
            return;
        };
        unsafe {
            SendMessageW(self.hwnd, set_cursel, index, 0);
            let id = GetDlgCtrlID(self.hwnd) as usize & 0xffff;
            SendMessageW(
                GetParent(self.hwnd),
                WM_COMMAND,
                id | (selchange << 16),
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
    unsafe { collect(h, l) }
}

fn pid_of(h: Hwnd) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    pid
}

fn children(parent: Hwnd) -> Vec<Ctl> {
    let mut v: Vec<Ctl> = Vec::new();
    unsafe { EnumChildWindows(parent, collect, &mut v as *mut _ as isize) };
    v
}

fn top_levels() -> Vec<Ctl> {
    let mut v: Vec<Ctl> = Vec::new();
    unsafe { EnumWindows(collect_top, &mut v as *mut _ as isize) };
    let target = TARGET_PID.load(Ordering::Relaxed);
    if target != 0 {
        let tree = process_tree(target);
        v.retain(|c| tree.contains(&pid_of(c.hwnd)));
    }
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
    /// What `wine_dialog_res=pick` asks for. The driver is still here — it is what
    /// starts the demo, and what tells demarc when the demo is over — but the
    /// dialog belongs to whoever is sitting in front of it.
    no_go: bool,
    emulate_modeset: bool,
    timeout: f64,
    dialog_wait: Duration,
    /// One fallback chain per `--prefer`: the alternatives of a chain are tried
    /// in order and the first one the dialog actually offers wins, so a demo
    /// that has no 800x600 can still be asked for 640x480 instead.
    prefer: Vec<Vec<String>>,
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
        emulate_modeset: false,
        timeout: 20.0,
        dialog_wait: NO_DIALOG,
        prefer: vec![],
        check: vec![],
        uncheck: vec![],
        go: ["RUN", "OK", "START", "GO", "LAUNCH", "PLAY", "YES", "DEMO", "INTRO"]
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
            "--emulate-modeset" => {
                args.emulate_modeset = true;
                takes_value = false;
            }
            "--launch" => args.launch = Some(value),
            "--timeout" => args.timeout = value.parse().unwrap_or(20.0),
            "--dialog-wait" => {
                args.dialog_wait = value.parse().map(Duration::from_millis).unwrap_or(NO_DIALOG)
            }
            // Comma-separated substrings of a radio button or combo box
            // entry to select, best first. Entries that normalise to nothing
            // are dropped: an empty one would match every control there is.
            "--prefer" => {
                let chain: Vec<String> = value
                    .split(',')
                    .filter(|s| !norm(s).is_empty())
                    .map(|s| s.to_string())
                    .collect();
                if !chain.is_empty() {
                    args.prefer.push(chain);
                }
            }
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
            Kind::Combo | Kind::List => {
                format!(" items={:?} selected={}", c.items(), c.selected())
            }
            _ => String::new(),
        };
        println!(
            "  hwnd={:#x} class={:?} kind={:?} text={:?}{state}",
            c.hwnd, c.class, c.kind, c.text
        );
    }
}

/// Select the option matching `want` wherever this dialog offers it, and say
/// whether it was offered at all.
///
/// That answer is what makes a `--prefer` list a fallback chain: the next
/// alternative is only tried when this one appears on no control. A radio
/// button or checkbox that already carries the wanted label counts as offered
/// even though there is nothing to click.
fn select_preferred(kids: &[Ctl], want: &str) -> bool {
    let want = norm(want);
    let mut found = false;
    for c in kids {
        match c.kind {
            Kind::Radio | Kind::Check if norm(&c.text).contains(&want) => {
                found = true;
                if !c.checked() {
                    println!("select {:?}", c.text);
                    c.click();
                }
            }
            Kind::Combo | Kind::List => {
                let items = c.items();
                if let Some(i) = items.iter().position(|it| norm(it).contains(&want)) {
                    found = true;
                    println!("select {:?} from {:?}", items[i], c.class);
                    c.select(i);
                }
            }
            _ => {}
        }
    }
    found
}

/// Apply every requested option to one dialog, then press its start button.
/// Returns false if there was no button to press and no fallback was sent.
///
/// `--prefer` takes a checkbox as readily as a radio button, because a dialog
/// that offers its resolutions as `BS_AUTOCHECKBOX` is offering a radio group
/// in all but style: *Panic Room* draws eleven of them and unticks the other
/// ten itself the moment one is clicked. Only the wanted box is ever ticked, so
/// a dialog that does *not* do that is left with two ticked and picks whichever
/// it likes — which is no worse than the default it would otherwise have kept.
/// Unticking the rest is not an option here: `VSync` and `Aspect: 16:9` sit in
/// the same list and are nobody's peers.
fn drive(top: &Ctl, kids: &[Ctl], args: &Args) -> bool {
    unsafe { SetForegroundWindow(top.hwnd) };

    // Switches first, because one of them can rewrite the very lists the
    // resolution is then chosen from: *Finally Inside* offers twenty display
    // modes while `FullScreen` is ticked and five windowed sizes when it is
    // not, rebuilding the list — and resetting its selection — the moment the
    // box is clicked. A mode chosen before that is silently thrown away.
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
    // Then the resolution, against whatever the dialog offers now. Each
    // `--prefer` is a list of alternatives: the first one this dialog offers is
    // taken and the rest are left alone, so the caller can name the mode it
    // wants and then the ones it would settle for.
    for chain in &args.prefer {
        if !chain.iter().any(|want| select_preferred(kids, want)) {
            println!("nothing here matches {chain:?}");
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
    // Tentura has no start button: each resolution is a push button of its own.
    for want in args.prefer.iter().flatten().map(|w| norm(w)) {
        if let Some(c) = kids
            .iter()
            .find(|c| c.kind == Kind::Push && norm(&c.text).contains(&want))
        {
            println!("click {:?}", c.text);
            c.click();
            return true;
        }
    }
    if args.no_fallback {
        return false;
    }
    // A window that is not a real dialog ignores Return, so press its
    // BS_DEFPUSHBUTTON directly when it has one.
    if let Some(c) = kids.iter().find(|c| c.is_default_push()) {
        println!("click default button {:?}", c.text);
        c.click();
        return true;
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

/// How long a window with no buttons on it has to stay the only thing there
/// before it counts as "this demo has no dialog".
const NO_DIALOG: Duration = Duration::from_millis(500);

fn main() {
    let args = parse_args();
    // Whoever is capturing the session shows black between these two, so the
    // dialog never reaches the screen.
    let hiding = !args.no_go && !args.list;
    if hiding {
        report("hiding");
    }
    if args.emulate_modeset {
        emulate_modeset();
    }
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
    // Since when the demo has had a window with no buttons on it.
    let mut plain: Option<Instant> = None;
    while !args.no_go && !driven && start.elapsed() < Duration::from_secs_f64(args.timeout) {
        let mut any_plain = false;
        for top in top_levels() {
            let kids = children(top.hwnd);
            // A dialog worth driving has at least one button on it; this skips
            // splash windows and the demo's own render window.
            if !kids.iter().any(|c| c.kind == Kind::Push) {
                any_plain = true;
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
        if driven {
            break;
        }
        // The demo is already drawing and no dialog has turned up beside it:
        // waiting out the timeout would only keep the session black.
        match plain {
            _ if !any_plain => plain = None,
            Some(since) if since.elapsed() >= args.dialog_wait => break,
            Some(_) => {}
            None => plain = Some(Instant::now()),
        }
        unsafe { Sleep(100) };
    }
    if !args.no_go && !driven && plain.is_none() {
        eprintln!("no dialog appeared within {}s", args.timeout);
    }

    if !args.list && !args.no_fill {
        wait_and_fill(args.timeout);
    }

    if hiding {
        report("visible");
    }

    // Outlive the demo, and say so when it goes: this is the only place that
    // can — see `report`. Without a launch there is nothing to wait for.
    match demo {
        Some(handle) => {
            unsafe { WaitForSingleObject(handle, INFINITE) };
            // Monstra Kosa's launcher restarts the exe and exits.
            let pid = TARGET_PID.load(Ordering::Relaxed);
            while process_tree(pid).len() > 1 {
                unsafe { Sleep(500) };
            }
            report("exited");
        }
        None if !driven => std::process::exit(1),
        None => {}
    }
}
