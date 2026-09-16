//! A `dcomp.dll` shim that gives DirectComposition demos a swapchain under wine.
//!
//! Demos that render through DirectComposition ask DXGI for a composition swapchain
//! (`IDXGIFactory2::CreateSwapChainForComposition`) and hand it to a dcomp visual.
//! Neither DXVK nor wined3d implements that call - both return `E_NOTIMPL` - and the
//! demos generally don't check, so the first `Present` faults on a null swapchain.
//!
//! The fix is to hand them an ordinary swapchain bound to the window they already
//! created: `CreateSwapChainForHwnd` on the process's own top level window, with
//! `AlphaMode` forced to UNSPECIFIED (hwnd swapchains reject premultiplied alpha).
//! Whatever the demo then does with dcomp - target, visual, `SetContent` - becomes a
//! decorative no-op, because the frames already go straight to the window.
//!
//! That leaves the dcomp calls themselves, and wine's `DCompositionCreateDevice` is a
//! stub that fails without writing an out pointer - the same unchecked-null crash one
//! call later. So this takes wine's dcomp's place in the prefix (`dcomp=n,b`) and hands
//! back a device whose visuals and targets do nothing, which is all the demo needs once
//! its frames go to the window.
//!
//! Being asked for a device is also the moment to patch slot 24 of DXVK's
//! `IDXGIFactory2` vtable: the demo has a D3D device by then - it is the argument -
//! and cannot have asked for a swapchain yet. The vtable is per class, so patching it
//! through a factory of our own also patches the one the demo fishes out with
//! `GetParent`.
//!
//! `n,b` means nothing falls through to wine's builtin, so everything it exports is
//! exported here too, the rest returning `E_NOTIMPL` as its stubs do. Those take no
//! arguments, which only holds where the caller cleans the stack: x86-64 only.

#![allow(non_snake_case)]

use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

type HRESULT = i32;
type HMODULE = *mut c_void;
type HWND = *mut c_void;
type BOOL = i32;

const S_OK: HRESULT = 0;
const E_NOTIMPL: HRESULT = 0x8000_4001u32 as i32;
const PAGE_READWRITE: u32 = 0x04;
const DLL_PROCESS_ATTACH: u32 = 1;

/// `IDXGIFactory2` vtable slots, counting from `IUnknown`.
const VT_CREATE_SWAPCHAIN_FOR_HWND: usize = 15;
const VT_CREATE_SWAPCHAIN_FOR_COMPOSITION: usize = 24;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> HMODULE;
    fn GetModuleHandleW(name: *const u16) -> HMODULE;
    fn GetProcAddress(module: HMODULE, name: *const u8) -> *const c_void;
    fn VirtualProtect(addr: *mut c_void, size: usize, new: u32, old: *mut u32) -> BOOL;
    fn GetCurrentProcessId() -> u32;
    fn OutputDebugStringA(msg: *const u8);
}

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(cb: unsafe extern "system" fn(HWND, isize) -> BOOL, param: isize) -> BOOL;
    fn GetWindowThreadProcessId(hwnd: HWND, pid: *mut u32) -> u32;
    fn IsWindowVisible(hwnd: HWND) -> BOOL;
    fn GetClientRect(hwnd: HWND, rect: *mut Rect) -> BOOL;
}

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SwapChainDesc1 {
    width: u32,
    height: u32,
    format: u32,
    stereo: BOOL,
    sample_count: u32,
    sample_quality: u32,
    buffer_usage: u32,
    buffer_count: u32,
    scaling: u32,
    swap_effect: u32,
    alpha_mode: u32,
    flags: u32,
}

#[repr(C)]
pub struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}

/// IID_IDXGIFactory2
const IID_FACTORY2: Guid = Guid {
    a: 0x50c8_3a1c,
    b: 0xe072,
    c: 0x4c48,
    d: [0x87, 0xb0, 0x36, 0x30, 0xfa, 0x36, 0xa6, 0xd0],
};

type PfnCreateForComposition = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *const SwapChainDesc1,
    *mut c_void,
    *mut *mut c_void,
) -> HRESULT;

type PfnCreateForHwnd = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    HWND,
    *const SwapChainDesc1,
    *const c_void,
    *mut c_void,
    *mut *mut c_void,
) -> HRESULT;

type PfnCreateDXGIFactory1 = unsafe extern "system" fn(*const Guid, *mut *mut c_void) -> HRESULT;

/// The real `CreateSwapChainForComposition`, kept so we can fall back to its
/// `E_NOTIMPL` when there is no window to present to.
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

fn log(msg: &str) {
    let mut buf = [0u8; 256];
    let bytes = msg.as_bytes();
    let n = bytes.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&bytes[..n]);
    unsafe { OutputDebugStringA(buf.as_ptr()) };
}

fn wide(s: &str) -> [u16; 128] {
    let mut buf = [0u16; 128];
    for (i, c) in s.encode_utf16().enumerate().take(127) {
        buf[i] = c;
    }
    buf
}

/// The demo's own window: top level, visible, belongs to this process, biggest one.
/// Composition swapchains carry no hwnd, so this is the only way to find the target.
unsafe extern "system" fn pick_window(hwnd: HWND, param: isize) -> BOOL {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != GetCurrentProcessId() || IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let mut rect = Rect::default();
        if GetClientRect(hwnd, &mut rect) == 0 {
            return 1;
        }
        let area = (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64;
        let best = &mut *(param as *mut (HWND, i64));
        if area > best.1 {
            *best = (hwnd, area);
        }
        1
    }
}

fn main_window() -> HWND {
    let mut best: (HWND, i64) = (ptr::null_mut(), 0);
    unsafe { EnumWindows(pick_window, &mut best as *mut _ as isize) };
    best.0
}

unsafe extern "system" fn create_swapchain_for_composition(
    factory: *mut c_void,
    device: *mut c_void,
    desc: *const SwapChainDesc1,
    restrict_to_output: *mut c_void,
    swapchain: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        let hwnd = main_window();
        if hwnd.is_null() || desc.is_null() {
            log("compshim: no window to present to, passing through");
            let original = ORIGINAL.load(Ordering::Relaxed);
            if original == 0 {
                return E_NOTIMPL;
            }
            let original: PfnCreateForComposition = core::mem::transmute(original);
            return original(factory, device, desc, restrict_to_output, swapchain);
        }

        let mut desc = *desc;
        // DXGI only accepts premultiplied alpha for composition swapchains.
        desc.alpha_mode = 0;

        let vtable = *(factory as *const *const usize);
        let create_for_hwnd: PfnCreateForHwnd =
            core::mem::transmute(*vtable.add(VT_CREATE_SWAPCHAIN_FOR_HWND));
        let hr = create_for_hwnd(
            factory,
            device,
            hwnd,
            &desc,
            ptr::null(),
            restrict_to_output,
            swapchain,
        );
        if hr < 0 {
            log("compshim: CreateSwapChainForHwnd failed");
        } else {
            log("compshim: composition swapchain redirected to hwnd");
        }
        hr
    }
}

/// Point slot 24 of DXVK's `IDXGIFactory2` vtable at our implementation.
fn install_hook() {
    unsafe {
        let mut dxgi = GetModuleHandleW(wide("dxgi.dll").as_ptr());
        if dxgi.is_null() {
            dxgi = LoadLibraryW(wide("C:\\windows\\system32\\dxgi.dll").as_ptr());
        }
        if dxgi.is_null() {
            log("compshim: no dxgi.dll");
            return;
        }
        let create_factory = GetProcAddress(dxgi, c"CreateDXGIFactory1".as_ptr() as *const u8);
        if create_factory.is_null() {
            log("compshim: no CreateDXGIFactory1");
            return;
        }
        let create_factory: PfnCreateDXGIFactory1 = core::mem::transmute(create_factory);

        let mut factory: *mut c_void = ptr::null_mut();
        if create_factory(&IID_FACTORY2, &mut factory) < 0 || factory.is_null() {
            log("compshim: could not create an IDXGIFactory2");
            return;
        }

        let vtable = *(factory as *const *mut usize);
        let slot = vtable.add(VT_CREATE_SWAPCHAIN_FOR_COMPOSITION);
        let mut old_protect = 0u32;
        if VirtualProtect(
            slot as *mut c_void,
            core::mem::size_of::<usize>(),
            PAGE_READWRITE,
            &mut old_protect,
        ) != 0
        {
            ORIGINAL.store(*slot, Ordering::Relaxed);
            *slot = create_swapchain_for_composition as usize;
            VirtualProtect(
                slot as *mut c_void,
                core::mem::size_of::<usize>(),
                old_protect,
                &mut old_protect,
            );
            log("compshim: CreateSwapChainForComposition hooked");
        } else {
            log("compshim: could not unprotect the factory vtable");
        }

        // Release: the vtable we patched is shared by every factory instance.
        let vtable = *(factory as *const *const usize);
        let release: unsafe extern "system" fn(*mut c_void) -> u32 =
            core::mem::transmute(*vtable.add(2));
        release(factory);
    }
}

// ---------------------------------------------------------------------------
// The dcomp side: objects that exist only so the demo has something to talk to.
// ---------------------------------------------------------------------------

#[repr(C)]
struct ComObject<T: 'static> {
    vtable: &'static T,
}

// Every method is a no-op, so one shared instance of each kind is enough.
unsafe impl<T> Sync for ComObject<T> {}

unsafe extern "system" fn stub_query_interface(
    _this: *mut c_void,
    _iid: *const Guid,
    out: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        if !out.is_null() {
            *out = ptr::null_mut();
        }
    }
    E_NOTIMPL
}

unsafe extern "system" fn stub_add_ref(_this: *mut c_void) -> u32 {
    1
}

unsafe extern "system" fn stub_release(_this: *mut c_void) -> u32 {
    1
}

unsafe extern "system" fn stub_ok(_this: *mut c_void) -> HRESULT {
    S_OK
}

unsafe extern "system" fn stub_not_implemented() -> HRESULT {
    log("compshim: unimplemented dcomp method called");
    E_NOTIMPL
}

#[repr(C)]
struct TargetVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    set_root: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
}

unsafe extern "system" fn target_set_root(_this: *mut c_void, _visual: *mut c_void) -> HRESULT {
    S_OK
}

static TARGET_VTBL: TargetVtbl = TargetVtbl {
    query_interface: stub_query_interface,
    add_ref: stub_add_ref,
    release: stub_release,
    set_root: target_set_root,
};

static TARGET: ComObject<TargetVtbl> = ComObject { vtable: &TARGET_VTBL };

#[repr(C)]
struct VisualVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// Slots 3..15: the offset, transform, effect, interpolation, border and clip
    /// setters, none of which mean anything for a swapchain we present ourselves.
    setters: [unsafe extern "system" fn() -> HRESULT; 12],
    set_content: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    /// Slots 16..20: AddVisual, RemoveVisual, RemoveAllVisuals, SetCompositeMode.
    children: [unsafe extern "system" fn() -> HRESULT; 4],
}

unsafe extern "system" fn visual_set_content(_this: *mut c_void, _content: *mut c_void) -> HRESULT {
    S_OK
}

static VISUAL_VTBL: VisualVtbl = VisualVtbl {
    query_interface: stub_query_interface,
    add_ref: stub_add_ref,
    release: stub_release,
    setters: [stub_not_implemented; 12],
    set_content: visual_set_content,
    children: [stub_not_implemented; 4],
};

static VISUAL: ComObject<VisualVtbl> = ComObject { vtable: &VISUAL_VTBL };

#[repr(C)]
struct DeviceVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    commit: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    wait_for_commit_completion: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    get_frame_statistics: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    create_target_for_hwnd:
        unsafe extern "system" fn(*mut c_void, HWND, BOOL, *mut *mut c_void) -> HRESULT,
    create_visual: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    /// Slots 8 onwards: surfaces, transforms, clips, animations. A demo that needs
    /// those needs a real DirectComposition, not this.
    rest: [unsafe extern "system" fn() -> HRESULT; 32],
}

unsafe extern "system" fn device_create_target_for_hwnd(
    _this: *mut c_void,
    _hwnd: HWND,
    _topmost: BOOL,
    target: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        if target.is_null() {
            return E_NOTIMPL;
        }
        *target = &TARGET as *const _ as *mut c_void;
    }
    S_OK
}

unsafe extern "system" fn device_create_visual(
    _this: *mut c_void,
    visual: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        if visual.is_null() {
            return E_NOTIMPL;
        }
        *visual = &VISUAL as *const _ as *mut c_void;
    }
    S_OK
}

static DEVICE_VTBL: DeviceVtbl = DeviceVtbl {
    query_interface: stub_query_interface,
    add_ref: stub_add_ref,
    release: stub_release,
    commit: stub_ok,
    wait_for_commit_completion: stub_ok,
    get_frame_statistics: stub_ok,
    create_target_for_hwnd: device_create_target_for_hwnd,
    create_visual: device_create_visual,
    rest: [stub_not_implemented; 32],
};

static DEVICE: ComObject<DeviceVtbl> = ComObject { vtable: &DEVICE_VTBL };

fn create_device(device: *mut *mut c_void) -> HRESULT {
    if device.is_null() {
        return E_NOTIMPL;
    }
    // Only for a demo that loads dcomp by hand, after the loader ran.
    if ORIGINAL.load(Ordering::Relaxed) == 0 {
        install_hook();
    }
    unsafe { *device = &DEVICE as *const _ as *mut c_void };
    log("compshim: handed out a no-op DirectComposition device");
    S_OK
}

/// The demos that need this import dcomp statically, so the loader brings us in
/// at process start - after dxgi, which is all `install_hook` needs, and before
/// the demo can ask for anything. Waiting for a dcomp call of our own is too
/// late: they create the composition swapchain first and the device after it.
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(_module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        install_hook();
    }
    1
}

// ---------------------------------------------------------------------------
// The exports, wine's dcomp export table. 2 and 3 take an IUnknown instead of
// an IDXGIDevice; same shape otherwise, and we look at neither.
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn DCompositionCreateDevice(
    _rendering_device: *mut c_void,
    _iid: *const Guid,
    device: *mut *mut c_void,
) -> HRESULT {
    create_device(device)
}

#[unsafe(no_mangle)]
pub extern "system" fn DCompositionCreateDevice2(
    _rendering_device: *mut c_void,
    _iid: *const Guid,
    device: *mut *mut c_void,
) -> HRESULT {
    create_device(device)
}

#[unsafe(no_mangle)]
pub extern "system" fn DCompositionCreateDevice3(
    _rendering_device: *mut c_void,
    _iid: *const Guid,
    device: *mut *mut c_void,
) -> HRESULT {
    create_device(device)
}

macro_rules! wine_stubs {
    ($($name:ident),* $(,)?) => {$(
        #[unsafe(no_mangle)]
        pub extern "system" fn $name() -> HRESULT {
            unsafe { stub_not_implemented() }
        }
    )*};
}

wine_stubs!(
    CompileEffectDescription,
    CreateEffectDescription,
    DCompositionAttachMouseDragToHwnd,
    DCompositionAttachMouseWheelToHwnd,
    DCompositionCreateSurfaceHandle,
    DeserializeEffectDescription,
    DllCanUnloadNow,
    DllGetActivationFactory,
    DllGetClassObject,
    DwmEnableMMCSS,
    DwmFlush,
    DwmpEnableDDASupport,
    SerializeEffectDescription,
);
