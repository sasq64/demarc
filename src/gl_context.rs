//! Offscreen OpenGL context for hardware-rendered libretro cores.
//!
//! Cores that answer `RETRO_ENVIRONMENT_SET_HW_RENDER` (mupen64plus_next via
//! GLideN64, for one) don't hand the frontend a pixel buffer. They expect a live
//! GL context plus an FBO to draw into, then signal a finished frame by calling
//! `video_refresh` with the sentinel `RETRO_HW_FRAME_BUFFER_VALID` instead of a
//! pointer. This module supplies that context.
//!
//! Bevy's wgpu device is not usable here — it may not even be a GL backend, and
//! it lives on the render thread — so we create a private EGL context and read
//! the finished frame back to the CPU, which then flows through the exact same
//! `RetroState::frame` path as every software-rendered core. Readback costs a
//! stall and a copy, but it keeps the whole HW path behind the existing
//! `RetroEmu` interface: the shader chain, screenshots, and grid mode need no
//! knowledge that a core rendered on the GPU. Sharing the texture with wgpu
//! without a copy would mean dmabuf/external-memory interop, which is
//! platform-specific and a much larger change.
//!
//! ## Threading
//!
//! A GL context belongs to whichever thread called `eglMakeCurrent`, and the
//! core issues its GL calls from inside `retro_run` — i.e. on the `retro-emu`
//! worker thread ([`crate::retro_emu::RetroCoreThreaded`]). So the context lives
//! in a thread-local, owned by the thread that drives the core. That also solves
//! the callback problem: libretro's `get_proc_address` and
//! `get_current_framebuffer` take no user-data argument, so they have no way to
//! reach a specific instance and must look the context up per-thread anyway.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use glow::HasContext;
use khronos_egl as egl;
use tracing::{debug, info, warn};

use crate::libretro::{
    RETRO_HW_CONTEXT_OPENGL, RETRO_HW_CONTEXT_OPENGL_CORE, RETRO_HW_CONTEXT_OPENGLES2,
    RETRO_HW_CONTEXT_OPENGLES3, RETRO_HW_CONTEXT_OPENGLES_VERSION, retro_hw_context_type,
};

/// `EGL_CONTEXT_OPENGL_PROFILE_MASK` and its two profile bits. These are EGL 1.5
/// constants (also provided by `EGL_KHR_create_context` on 1.4), but we load EGL
/// through the 1.4 interface for portability, so name them here.
const EGL_CONTEXT_OPENGL_PROFILE_MASK: egl::Int = 0x30FD;
const EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT: egl::Int = 0x0000_0001;
const EGL_CONTEXT_OPENGL_COMPATIBILITY_PROFILE_BIT: egl::Int = 0x0000_0002;

/// What the core asked for in `retro_hw_render_callback`, reduced to the plain
/// data needed to build a matching context.
///
/// Deliberately POD: this is stored in `RetroCoreDirect`, which is asserted
/// `Send` so it can be constructed on the worker thread. The context itself —
/// which is *not* `Send` — stays in this module's thread-local.
#[derive(Clone, Copy, Debug)]
pub struct HwRenderConfig {
    pub context_type: retro_hw_context_type,
    pub version_major: u32,
    pub version_minor: u32,
    pub depth: bool,
    pub stencil: bool,
    /// The core renders with the GL convention (origin bottom-left), so the
    /// readback has to flip rows to produce our top-left-origin frame.
    pub bottom_left_origin: bool,
}

impl HwRenderConfig {
    /// Whether we can actually service this request.
    ///
    /// Checked at `SET_HW_RENDER` time, which is the only moment declining is
    /// useful: a core told "no" falls back to its software renderer, whereas
    /// failing later — once we've already promised a context — leaves it with no
    /// way to recover and takes the whole load down with it. So this also
    /// verifies EGL is actually loadable, not just that the API is one we
    /// implement; that is what makes a GL core degrade gracefully on a machine
    /// with no EGL rather than refusing to start.
    pub fn is_supported(context_type: retro_hw_context_type) -> bool {
        let known = matches!(
            context_type,
            RETRO_HW_CONTEXT_OPENGL
                | RETRO_HW_CONTEXT_OPENGL_CORE
                | RETRO_HW_CONTEXT_OPENGLES2
                | RETRO_HW_CONTEXT_OPENGLES3
                | RETRO_HW_CONTEXT_OPENGLES_VERSION
        );
        known && egl_instance().is_ok()
    }

    fn is_gles(&self) -> bool {
        matches!(
            self.context_type,
            RETRO_HW_CONTEXT_OPENGLES2
                | RETRO_HW_CONTEXT_OPENGLES3
                | RETRO_HW_CONTEXT_OPENGLES_VERSION
        )
    }
}

thread_local! {
    /// The calling thread's context, if it drives a hardware-rendered core.
    static CONTEXT: RefCell<Option<GlContext>> = const { RefCell::new(None) };
}

/// The process-wide libEGL handle.
///
/// Global rather than per-context because libEGL is loaded once per process and
/// `eglGetProcAddress` resolves independently of the current context — which
/// matters, since [`load_symbol`] has to work *while* a context is being built
/// and stored, before the thread-local is populated.
static EGL: OnceLock<Option<egl::DynamicInstance<egl::EGL1_4>>> = OnceLock::new();

fn egl_instance() -> Result<&'static egl::DynamicInstance<egl::EGL1_4>> {
    EGL.get_or_init(
        || match unsafe { egl::DynamicInstance::<egl::EGL1_4>::load_required() } {
            Ok(instance) => Some(instance),
            Err(e) => {
                warn!("could not load libEGL, hardware-rendered cores unavailable: {e}");
                None
            }
        },
    )
    .as_ref()
    .ok_or_else(|| anyhow!("libEGL is not available"))
}

/// A live EGL context plus the FBO the core renders into.
struct GlContext {
    display: egl::Display,
    context: egl::Context,
    /// A 1x1 pbuffer. Nothing is ever drawn to it; it exists only because
    /// `eglMakeCurrent` wants a draw surface on drivers lacking
    /// `EGL_KHR_surfaceless_context`.
    surface: egl::Surface,
    gl: glow::Context,
    fbo: glow::Framebuffer,
    color: glow::Texture,
    /// Combined depth/stencil attachment, present when the core asked for either.
    depth: Option<glow::Renderbuffer>,
    /// Current FBO dimensions; the core may report a smaller viewport than this.
    width: u32,
    height: u32,
}

impl GlContext {
    fn new(cfg: &HwRenderConfig) -> Result<Self> {
        // Loaded at runtime rather than linked: a machine with no EGL should
        // still run every software core, just not this one.
        let egl = egl_instance()?;

        let display = unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }
            .ok_or_else(|| anyhow!("eglGetDisplay returned no display"))?;
        let (major, minor) = egl.initialize(display)?;
        debug!("EGL {major}.{minor} initialized");

        let api = if cfg.is_gles() {
            egl::OPENGL_ES_API
        } else {
            egl::OPENGL_API
        };
        egl.bind_api(api)?;

        // Ask for a pbuffer-capable RGBA8 config. Depth/stencil live on our own
        // renderbuffer, not on this config's surface, so we don't request them
        // here — the pbuffer is never rendered to.
        let renderable = if cfg.is_gles() {
            // OPENGL_ES3_BIT; the ES2 bit is a subset accepted by ES3 configs.
            if cfg.context_type == RETRO_HW_CONTEXT_OPENGLES2 {
                egl::OPENGL_ES2_BIT
            } else {
                0x0000_0040
            }
        } else {
            egl::OPENGL_BIT
        };
        let config_attribs = [
            egl::SURFACE_TYPE,
            egl::PBUFFER_BIT,
            egl::RENDERABLE_TYPE,
            renderable,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &config_attribs)?
            .ok_or_else(|| anyhow!("no EGL config matched an RGBA8 pbuffer"))?;

        // A libretro `version_major` of 0 means "any"; ask for 3.3 core, the
        // baseline GLideN64 and friends expect.
        let (want_major, want_minor) = if cfg.version_major == 0 && !cfg.is_gles() {
            (3, 3)
        } else {
            (cfg.version_major.max(1) as egl::Int, cfg.version_minor as egl::Int)
        };

        let mut context_attribs = vec![
            egl::CONTEXT_MAJOR_VERSION,
            want_major,
            egl::CONTEXT_MINOR_VERSION,
            want_minor,
        ];
        if !cfg.is_gles() {
            // RETRO_HW_CONTEXT_OPENGL means legacy/compatibility; _CORE means a
            // core profile. Requesting the wrong one breaks cores that still use
            // fixed-function calls.
            context_attribs.push(EGL_CONTEXT_OPENGL_PROFILE_MASK);
            context_attribs.push(if cfg.context_type == RETRO_HW_CONTEXT_OPENGL_CORE {
                EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT
            } else {
                EGL_CONTEXT_OPENGL_COMPATIBILITY_PROFILE_BIT
            });
        }
        context_attribs.push(egl::NONE);

        let context = egl
            .create_context(display, config, None, &context_attribs)
            .map_err(|e| {
                anyhow!("eglCreateContext for GL {want_major}.{want_minor} failed: {e}")
            })?;

        let surface_attribs = [egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE];
        let surface = egl
            .create_pbuffer_surface(display, config, &surface_attribs)
            .map_err(|e| anyhow!("eglCreatePbufferSurface failed: {e}"))?;

        egl.make_current(display, Some(surface), Some(surface), Some(context))
            .map_err(|e| anyhow!("eglMakeCurrent failed: {e}"))?;

        // Resolve GL entry points through the same loader the core will use, so
        // our FBO calls and the core's rendering agree on one implementation.
        let gl = unsafe { glow::Context::from_loader_function(load_symbol) };

        unsafe {
            info!(
                "HW render context: {} | {} | GLSL {}",
                gl.get_parameter_string(glow::VERSION),
                gl.get_parameter_string(glow::RENDERER),
                gl.get_parameter_string(glow::SHADING_LANGUAGE_VERSION),
            );

            let fbo = gl
                .create_framebuffer()
                .map_err(|e| anyhow!("glGenFramebuffers failed: {e}"))?;
            let color = gl
                .create_texture()
                .map_err(|e| anyhow!("glGenTextures failed: {e}"))?;
            let depth = if cfg.depth || cfg.stencil {
                Some(
                    gl.create_renderbuffer()
                        .map_err(|e| anyhow!("glGenRenderbuffers failed: {e}"))?,
                )
            } else {
                None
            };

            let mut ctx = Self {
                display,
                context,
                surface,
                gl,
                fbo,
                color,
                depth,
                width: 0,
                height: 0,
            };
            ctx.resize(640, 480)?;
            Ok(ctx)
        }
    }

    /// Grow the render target to at least `width` x `height`. The FBO is only
    /// ever enlarged: cores change resolution mid-run (N64 titles switch between
    /// 320x240 and 640x480), and reallocating both attachments on every wobble
    /// would churn GPU memory for no benefit.
    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let width = width.max(self.width).max(1);
        let height = height.max(self.height).max(1);
        if width == self.width && height == self.height {
            return Ok(());
        }
        let gl = &self.gl;
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.color));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                width as i32,
                height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            // No mipmaps: the default min filter would leave the texture
            // incomplete, and cores sample this only through their own FBO.
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAX_LEVEL, 0);
            gl.bind_texture(glow::TEXTURE_2D, None);

            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(self.color),
                0,
            );
            if let Some(depth) = self.depth {
                // One combined DEPTH24_STENCIL8 buffer covers both requests;
                // separate depth and stencil renderbuffers are not universally
                // supported as attachments, the packed format is.
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
                gl.renderbuffer_storage(
                    glow::RENDERBUFFER,
                    glow::DEPTH24_STENCIL8,
                    width as i32,
                    height as i32,
                );
                gl.framebuffer_renderbuffer(
                    glow::FRAMEBUFFER,
                    glow::DEPTH_STENCIL_ATTACHMENT,
                    glow::RENDERBUFFER,
                    Some(depth),
                );
                gl.bind_renderbuffer(glow::RENDERBUFFER, None);
            }

            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(anyhow!(
                    "hw render FBO incomplete ({width}x{height}): status 0x{status:x}"
                ));
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        debug!("HW render FBO resized to {width}x{height}");
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Copy the rendered `width` x `height` region back into `dst` as packed
    /// RGBA, matching the byte order `RetroState::frame` uses elsewhere.
    fn read_frame(&self, dst: &mut [u32], width: usize, height: usize, flip: bool) {
        if width == 0 || height == 0 || dst.len() < width * height {
            return;
        }
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(dst.as_mut_ptr() as *mut u8, width * height * 4)
        };
        unsafe {
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.fbo));
            // The core leaves its own pack state behind; RGBA8 rows are always
            // 4-byte aligned, but row length must be reset or a core that set it
            // would corrupt our stride.
            self.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            self.gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
            self.gl.read_pixels(
                0,
                0,
                width as i32,
                height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(bytes)),
            );
            self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
        }
        if flip {
            // glReadPixels returns rows bottom-up; our frame buffer is top-down.
            for y in 0..height / 2 {
                let (top, rest) = dst.split_at_mut((y + 1) * width);
                let top = &mut top[y * width..];
                let bottom = &mut rest[(height - 2 * y - 2) * width..][..width];
                top.swap_with_slice(bottom);
            }
        }
    }
}

impl Drop for GlContext {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_texture(self.color);
            if let Some(depth) = self.depth {
                self.gl.delete_renderbuffer(depth);
            }
        }
        let Ok(egl) = egl_instance() else { return };
        let _ = egl.make_current(self.display, None, None, None);
        let _ = egl.destroy_surface(self.display, self.surface);
        let _ = egl.destroy_context(self.display, self.context);
    }
}

/// Resolve a GL entry point by name, for both glow and the core itself.
///
/// `eglGetProcAddress` is authoritative on modern libglvnd/Mesa (it resolves
/// core functions, not just extensions), but older drivers only return
/// extensions, so fall back to `dlsym` on the already-loaded GL library.
fn load_symbol(sym: &str) -> *const c_void {
    let Ok(egl) = egl_instance() else {
        return std::ptr::null();
    };
    egl.get_proc_address(sym)
        .map_or(std::ptr::null(), |f| f as *const c_void)
}

/// libretro's `get_proc_address`: the core calls this from `context_reset` to
/// bind the GL functions it needs.
pub unsafe extern "C" fn get_proc_address(sym: *const c_char) -> Option<unsafe extern "C" fn()> {
    if sym.is_null() {
        return None;
    }
    let Ok(name) = (unsafe { CStr::from_ptr(sym) }).to_str() else {
        return None;
    };
    let addr = load_symbol(name);
    if addr.is_null() {
        debug!("core asked for unavailable GL symbol {name}");
        return None;
    }
    Some(unsafe { std::mem::transmute::<*const c_void, unsafe extern "C" fn()>(addr) })
}

/// libretro's `get_current_framebuffer`: the core binds this FBO name before
/// each frame instead of rendering to the default framebuffer (which, for our
/// offscreen context, is a 1x1 pbuffer).
pub unsafe extern "C" fn get_current_framebuffer() -> usize {
    CONTEXT.with_borrow(|ctx| ctx.as_ref().map_or(0, |c| c.fbo.0.get() as usize))
}

/// Build this thread's context from what the core requested. Replaces any
/// existing one, since a core may renegotiate on reload.
pub fn create(cfg: &HwRenderConfig) -> Result<()> {
    CONTEXT.with_borrow_mut(|slot| {
        *slot = None;
        let ctx = GlContext::new(cfg)?;
        *slot = Some(ctx);
        Ok(())
    })
}

/// Tear down this thread's context, if any. Called before the core's
/// `context_destroy` would become meaningless (i.e. at unload).
pub fn destroy() {
    CONTEXT.with_borrow_mut(|slot| *slot = None);
}

/// Whether this thread currently drives a hardware-rendered core.
pub fn is_active() -> bool {
    CONTEXT.with_borrow(|slot| slot.is_some())
}

/// Grow the render target so the core has room for a `width` x `height` frame.
pub fn ensure_size(width: u32, height: u32) {
    CONTEXT.with_borrow_mut(|slot| {
        if let Some(ctx) = slot.as_mut()
            && let Err(e) = ctx.resize(width, height)
        {
            warn!("could not resize hw render target: {e}");
        }
    });
}

/// Read the core's finished frame out of the FBO into `dst`.
pub fn read_frame(dst: &mut [u32], width: usize, height: usize, flip: bool) {
    CONTEXT.with_borrow(|slot| {
        if let Some(ctx) = slot.as_ref() {
            ctx.read_frame(dst, width, height, flip);
        }
    });
}
