//! Picks Slint's renderer at startup: the GPU (femtovg on OpenGL) when the
//! driver is a real GPU, software otherwise.
//!
//! On Linux the GL driver is asked for its name through EGL before any
//! window exists (libEGL is loaded at runtime, so a machine without it
//! still starts). Mesa's CPU drivers (llvmpipe, softpipe) count as no GPU:
//! there the software renderer, which repaints only what changed, is
//! faster than OpenGL drawn by the CPU. `[FERRUM]RENDERER` or
//! `FERRUM_RENDERER` = `gpu` | `software` | `auto` (default) overrides it.

use crate::config::Config;

#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    Gpu,
    Software,
}

/// Which renderer to use, and why (for the log and the Settings screen).
pub fn choose(cfg: &Config) -> (Choice, String) {
    let asked = std::env::var("FERRUM_RENDERER").ok().or_else(|| crate::machine_cfg::get().renderer(cfg)).unwrap_or_default().to_lowercase();
    match asked.as_str() {
        "gpu" => return (Choice::Gpu, "GPU (set in config)".into()),
        "software" | "cpu" => return (Choice::Software, "software (set in config)".into()),
        _ => {}
    }
    if cfg!(target_os = "macos") {
        return (Choice::Gpu, "GPU".into());
    }
    match probe() {
        Ok(name) if is_cpu_driver(&name) => (Choice::Software, format!("software (GL is {name}, a CPU driver)")),
        Ok(name) => (Choice::Gpu, format!("GPU ({name})")),
        Err(e) => (Choice::Software, format!("software (no GPU: {e})")),
    }
}

fn is_cpu_driver(name: &str) -> bool {
    let n = name.to_lowercase();
    ["llvmpipe", "softpipe", "swrast", "software rasterizer", "lavapipe"].iter().any(|s| n.contains(s))
}

/// Install the chosen renderer; falls back to software if the GPU one can't
/// be selected. Returns what was installed.
pub fn install(choice: Choice) -> Choice {
    let name = |c: &Choice| if *c == Choice::Gpu { "femtovg" } else { "software" };
    let backend = if cfg!(target_os = "macos") { "winit" } else { "winit-x11" };
    let sel = |c: &Choice| slint::BackendSelector::new().backend_name(backend.into()).renderer_name(name(c).into()).select();
    match sel(&choice) {
        Ok(()) => choice,
        Err(e) if choice == Choice::Gpu => {
            eprintln!("ferrum: GPU renderer unavailable ({e}); using software");
            if let Err(e) = sel(&Choice::Software) {
                eprintln!("ferrum: software renderer: {e}");
            }
            Choice::Software
        }
        Err(e) => {
            eprintln!("ferrum: renderer: {e}");
            choice
        }
    }
}

/// The GL renderer's name through EGL, given at most two seconds (a broken
/// driver must not keep the screen from starting).
#[cfg(target_os = "linux")]
fn probe() -> Result<String, String> {
    // Mesa's EGL warns on stderr about X servers without DRI3 (which is
    // the very case being checked); keep the log to real errors.
    let quiet = std::env::var_os("EGL_LOG_LEVEL").is_none();
    if quiet {
        std::env::set_var("EGL_LOG_LEVEL", "fatal");
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(unsafe { egl::renderer() });
    });
    let out = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or_else(|_| Err("GL driver did not answer".into()));
    if quiet {
        std::env::remove_var("EGL_LOG_LEVEL");
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn probe() -> Result<String, String> {
    Err("no EGL on this platform".into())
}

#[cfg(target_os = "linux")]
mod egl {
    use libloading::{Library, Symbol};
    use std::ffi::{c_char, c_void, CStr};

    type Display = *mut c_void;
    type Config = *mut c_void;
    type Surface = *mut c_void;
    type Context = *mut c_void;
    const NONE: i32 = 0x3038;
    const OPENGL_ES_API: u32 = 0x30A0;
    const RENDERABLE_TYPE: i32 = 0x3040;
    const OPENGL_ES2_BIT: i32 = 0x4;
    const SURFACE_TYPE: i32 = 0x3033;
    const PBUFFER_BIT: i32 = 0x1;
    const WIDTH: i32 = 0x3057;
    const HEIGHT: i32 = 0x3056;
    const CONTEXT_CLIENT_VERSION: i32 = 0x3098;
    const GL_RENDERER: u32 = 0x1F01;

    pub unsafe fn renderer() -> Result<String, String> {
        let lib = Library::new("libEGL.so.1").map_err(|_| "libEGL not installed".to_string())?;
        macro_rules! f {
            ($name:literal, $t:ty) => {{
                let s: Symbol<$t> = lib.get($name).map_err(|e| e.to_string())?;
                *s
            }};
        }
        let get_display = f!(b"eglGetDisplay\0", unsafe extern "C" fn(*mut c_void) -> Display);
        let initialize = f!(b"eglInitialize\0", unsafe extern "C" fn(Display, *mut i32, *mut i32) -> u32);
        let bind_api = f!(b"eglBindAPI\0", unsafe extern "C" fn(u32) -> u32);
        let choose_config = f!(b"eglChooseConfig\0", unsafe extern "C" fn(Display, *const i32, *mut Config, i32, *mut i32) -> u32);
        let create_pbuffer = f!(b"eglCreatePbufferSurface\0", unsafe extern "C" fn(Display, Config, *const i32) -> Surface);
        let create_context = f!(b"eglCreateContext\0", unsafe extern "C" fn(Display, Config, Context, *const i32) -> Context);
        let make_current = f!(b"eglMakeCurrent\0", unsafe extern "C" fn(Display, Surface, Surface, Context) -> u32);
        let get_proc = f!(b"eglGetProcAddress\0", unsafe extern "C" fn(*const c_char) -> *const c_void);
        let destroy_context = f!(b"eglDestroyContext\0", unsafe extern "C" fn(Display, Context) -> u32);
        let destroy_surface = f!(b"eglDestroySurface\0", unsafe extern "C" fn(Display, Surface) -> u32);
        let terminate = f!(b"eglTerminate\0", unsafe extern "C" fn(Display) -> u32);

        let dpy = get_display(std::ptr::null_mut());
        if dpy.is_null() {
            return Err("no EGL display".into());
        }
        let (mut maj, mut min) = (0, 0);
        if initialize(dpy, &mut maj, &mut min) == 0 {
            return Err("EGL would not initialize".into());
        }
        let result = (|| {
            if bind_api(OPENGL_ES_API) == 0 {
                return Err("no OpenGL ES".to_string());
            }
            let attrs = [RENDERABLE_TYPE, OPENGL_ES2_BIT, SURFACE_TYPE, PBUFFER_BIT, NONE];
            let mut cfg: Config = std::ptr::null_mut();
            let mut n = 0;
            if choose_config(dpy, attrs.as_ptr(), &mut cfg, 1, &mut n) == 0 || n < 1 {
                return Err("no OpenGL ES config".into());
            }
            let surf = create_pbuffer(dpy, cfg, [WIDTH, 1, HEIGHT, 1, NONE].as_ptr());
            let ctx = create_context(dpy, cfg, std::ptr::null_mut(), [CONTEXT_CLIENT_VERSION, 2, NONE].as_ptr());
            if surf.is_null() || ctx.is_null() {
                return Err("could not create a GL context".into());
            }
            let out = if make_current(dpy, surf, surf, ctx) == 0 {
                Err("could not use a GL context".into())
            } else {
                let p = get_proc(c"glGetString".as_ptr());
                if p.is_null() {
                    Err("no glGetString".into())
                } else {
                    let get_string: unsafe extern "C" fn(u32) -> *const c_char = std::mem::transmute(p);
                    let s = get_string(GL_RENDERER);
                    if s.is_null() {
                        Err("driver gave no name".into())
                    } else {
                        Ok(CStr::from_ptr(s).to_string_lossy().into_owned())
                    }
                }
            };
            make_current(dpy, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            destroy_context(dpy, ctx);
            destroy_surface(dpy, surf);
            out
        })();
        terminate(dpy);
        result
    }
}
