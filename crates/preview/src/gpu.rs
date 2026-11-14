//! OpenGL renderer for the backplot, used when Slint draws with the GPU.
//!
//! The toolpath goes to the GPU once per program as one instance per move;
//! pan, zoom, the live tool and the trail only change uniforms or small
//! buffers, so a frame costs about the same for 1k or 1M moves. Lines are
//! quads widened in the vertex shader and antialiased in the fragment
//! shader, so they look like the software renderer's strokes. The picture
//! (colors, widths, fit, machine faces) matches `software`, which stays the
//! fallback when there is no usable GPU.
//!
//! Works on desktop GL 3.2 core (macOS) and GL ES 3.0 (Raspberry Pi 5).

use crate::machine::{Cylinder, Scene, Solid};
use crate::software::{convex_hull, load_color, load_step, project, toward_viewer, Fit, View, Zoom, BG, LOAD_STEPS, OP_COLORS};
use crate::{Kind, Toolpath, P4};
use glow::HasContext;

const RAPID: [u8; 4] = [0x6b, 0x74, 0x7b, 150];
const TOOL: [u8; 3] = [0xf2, 0xc1, 0x4e];

/// Bytes per line instance: from xyz, to xyz (f32), RGBA8.
const STRIDE: i32 = 28;
const ATTRS: [(&str, i32, u32, bool, i32); 3] = [("a_from", 3, glow::FLOAT, false, 0), ("a_to", 3, glow::FLOAT, false, 12), ("a_rgba", 4, glow::UNSIGNED_BYTE, true, 24)];

#[derive(Clone, Copy)]
#[repr(C)]
struct Inst {
    a: [f32; 3],
    b: [f32; 3],
    rgba: [u8; 4],
}

fn inst(a: [f64; 3], b: [f64; 3], rgba: [u8; 4]) -> Inst {
    Inst { a: a.map(|v| v as f32), b: b.map(|v| v as f32), rgba }
}

fn xyz(p: P4) -> [f64; 3] {
    [p[0], p[1], p[2]]
}

const LINE_VS: &str = r#"
in vec3 a_from;
in vec3 a_to;
in vec4 a_rgba;
uniform mat4 u_mvp;
uniform vec2 u_px;
uniform float u_width;
out vec4 v_rgba;
out float v_across;
void main() {
    vec4 ca = u_mvp * vec4(a_from, 1.0);
    vec4 cb = u_mvp * vec4(a_to, 1.0);
    vec2 sa = ca.xy / ca.w * 0.5 * u_px;
    vec2 sb = cb.xy / cb.w * 0.5 * u_px;
    vec2 d = sb - sa;
    float len = length(d);
    vec2 dir = len > 1e-4 ? d / len : vec2(1.0, 0.0);
    vec2 n = vec2(-dir.y, dir.x);
    int id = gl_VertexID;
    float along = (id == 1 || id == 3) ? 1.0 : 0.0;
    float side = id >= 2 ? 1.0 : -1.0;
    // Half the width plus a pixel for the antialiased edge; the ends grow by
    // half the width too, like a round cap.
    float hw = u_width * 0.5 + 1.0;
    vec2 p = mix(sa, sb, along) + n * side * hw + dir * (along * 2.0 - 1.0) * (u_width * 0.5);
    v_across = side * hw;
    v_rgba = a_rgba;
    gl_Position = vec4(p / (0.5 * u_px), 0.0, 1.0);
}
"#;

const LINE_FS: &str = r#"
in vec4 v_rgba;
in float v_across;
uniform float u_width;
out vec4 frag;
void main() {
    float cover = clamp(u_width * 0.5 + 0.5 - abs(v_across), 0.0, 1.0);
    frag = vec4(v_rgba.rgb, v_rgba.a * cover);
}
"#;

const FILL_VS: &str = r#"
in vec3 a_pos;
in vec4 a_rgba;
uniform mat4 u_mvp;
out vec4 v_rgba;
void main() {
    v_rgba = a_rgba;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#;

const FILL_FS: &str = r#"
in vec4 v_rgba;
out vec4 frag;
void main() { frag = v_rgba; }
"#;

/// Everything one frame draws besides the toolpath.
pub struct Frame<'a> {
    pub view: View,
    pub zoom: Zoom,
    pub width: u32,
    pub height: u32,
    pub trail: &'a [P4],
    /// Cutting load per trail point (0 to 1, NaN: none); empty draws it plain.
    pub loads: &'a [f32],
    pub tool: Option<P4>,
    pub machine: Option<&'a Scene>,
}

struct Lines {
    vao: glow::VertexArray,
    buf: glow::Buffer,
    count: i32,
    capacity: usize,
}

pub struct Gpu {
    gl: glow::Context,
    line_prog: glow::Program,
    fill_prog: glow::Program,
    /// Toolpath: rapids, then feeds.
    rapids: Lines,
    feeds: Lines,
    /// Rebuilt every frame: axes, machine edges, trail, tool marker.
    scratch: Lines,
    fill_vao: glow::VertexArray,
    fill_buf: glow::Buffer,
    tp_id: Option<usize>,
    /// Two targets, drawn alternately, so the one on screen isn't redrawn.
    targets: [Option<Target>; 2],
    next: usize,
}

struct Target {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    size: (u32, u32),
}

/// GL state the renderer touches, put back after drawing so Slint's own
/// renderer finds what it left.
struct Saved {
    fbo: Option<glow::Framebuffer>,
    viewport: [i32; 4],
    program: Option<glow::Program>,
    vao: Option<glow::VertexArray>,
    array_buf: Option<glow::Buffer>,
    tex: Option<glow::Texture>,
    active_tex: i32,
    blend: bool,
    scissor: bool,
    depth: bool,
    cull: bool,
    blend_func: [i32; 4],
}

unsafe fn save(gl: &glow::Context) -> Saved {
    let mut viewport = [0; 4];
    gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
    let active_tex = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
    gl.active_texture(glow::TEXTURE0);
    Saved {
        fbo: gl.get_parameter_framebuffer(glow::DRAW_FRAMEBUFFER_BINDING),
        viewport,
        program: gl.get_parameter_program(glow::CURRENT_PROGRAM),
        vao: gl.get_parameter_vertex_array(glow::VERTEX_ARRAY_BINDING),
        array_buf: gl.get_parameter_buffer(glow::ARRAY_BUFFER_BINDING),
        tex: gl.get_parameter_texture(glow::TEXTURE_BINDING_2D),
        active_tex,
        blend: gl.is_enabled(glow::BLEND),
        scissor: gl.is_enabled(glow::SCISSOR_TEST),
        depth: gl.is_enabled(glow::DEPTH_TEST),
        cull: gl.is_enabled(glow::CULL_FACE),
        blend_func: [
            gl.get_parameter_i32(glow::BLEND_SRC_RGB),
            gl.get_parameter_i32(glow::BLEND_DST_RGB),
            gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
            gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
        ],
    }
}

unsafe fn restore(gl: &glow::Context, s: Saved) {
    gl.bind_framebuffer(glow::FRAMEBUFFER, s.fbo);
    gl.viewport(s.viewport[0], s.viewport[1], s.viewport[2], s.viewport[3]);
    gl.use_program(s.program);
    gl.bind_vertex_array(s.vao);
    gl.bind_buffer(glow::ARRAY_BUFFER, s.array_buf);
    gl.bind_texture(glow::TEXTURE_2D, s.tex);
    gl.active_texture(s.active_tex as u32);
    let set = |cap, on: bool| if on { gl.enable(cap) } else { gl.disable(cap) };
    set(glow::BLEND, s.blend);
    set(glow::SCISSOR_TEST, s.scissor);
    set(glow::DEPTH_TEST, s.depth);
    set(glow::CULL_FACE, s.cull);
    let f = s.blend_func.map(|v| v as u32);
    gl.blend_func_separate(f[0], f[1], f[2], f[3]);
}

unsafe fn program(gl: &glow::Context, header: &str, vs: &str, fs: &str) -> Result<glow::Program, String> {
    let prog = gl.create_program()?;
    let mut shaders = Vec::new();
    for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
        let sh = gl.create_shader(kind)?;
        gl.shader_source(sh, &format!("{header}{src}"));
        gl.compile_shader(sh);
        if !gl.get_shader_compile_status(sh) {
            return Err(gl.get_shader_info_log(sh));
        }
        gl.attach_shader(prog, sh);
        shaders.push(sh);
    }
    gl.link_program(prog);
    if !gl.get_program_link_status(prog) {
        return Err(gl.get_program_info_log(prog));
    }
    for sh in shaders {
        gl.detach_shader(prog, sh);
        gl.delete_shader(sh);
    }
    Ok(prog)
}

impl Lines {
    unsafe fn new(gl: &glow::Context, prog: glow::Program) -> Result<Lines, String> {
        let vao = gl.create_vertex_array()?;
        let buf = gl.create_buffer()?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(buf));
        for (name, size, ty, norm, off) in ATTRS {
            let Some(loc) = gl.get_attrib_location(prog, name) else { continue };
            gl.enable_vertex_attrib_array(loc);
            gl.vertex_attrib_pointer_f32(loc, size, ty, norm, STRIDE, off);
            gl.vertex_attrib_divisor(loc, 1);
        }
        gl.bind_vertex_array(None);
        Ok(Lines { vao, buf, count: 0, capacity: 0 })
    }

    unsafe fn upload(&mut self, gl: &glow::Context, data: &[Inst], usage: u32) {
        let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, std::mem::size_of_val(data));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.buf));
        if data.len() > self.capacity || usage == glow::STATIC_DRAW {
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, usage);
            self.capacity = data.len();
        } else {
            gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, bytes);
        }
        self.count = data.len() as i32;
    }

    /// Draw instances `first..first + count` with the bound line program.
    unsafe fn draw(&self, gl: &glow::Context, prog: glow::Program, first: i32, count: i32) {
        if count <= 0 {
            return;
        }
        gl.bind_vertex_array(Some(self.vao));
        // GL ES has no base-instance draw: point the per-instance attributes
        // at `first` instead, and back after.
        let point = |at: i32| {
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.buf));
            for (name, size, ty, norm, off) in ATTRS {
                if let Some(loc) = gl.get_attrib_location(prog, name) {
                    gl.vertex_attrib_pointer_f32(loc, size, ty, norm, STRIDE, off + at * STRIDE);
                }
            }
        };
        if first != 0 {
            point(first);
        }
        gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, count);
        if first != 0 {
            point(0);
        }
    }

    unsafe fn delete(&self, gl: &glow::Context) {
        gl.delete_vertex_array(self.vao);
        gl.delete_buffer(self.buf);
    }
}

/// Column-major 4x4 taking work coordinates to clip space for `fit`.
fn mvp(fit: &Fit, w: u32, h: u32) -> [f32; 16] {
    // project() is linear: image x/y of each unit axis.
    let px = |i: usize| {
        let mut p = [0.0; 4];
        p[i] = 1.0;
        project(fit.view, p)
    };
    let (ex, ey, ez) = (px(0), px(1), px(2));
    // Pixel x = ox + proj.x * scale; pixel y (down) = h - (oy + proj.y * scale).
    // Clip x = px / w * 2 - 1; clip y (up) = 1 - py / h * 2 = (oy + proj.y * scale) / h * 2 - 1.
    let (sx, sy) = (2.0 * fit.scale / w as f64, 2.0 * fit.scale / h as f64);
    let (tx, ty) = (2.0 * fit.ox / w as f64 - 1.0, 2.0 * fit.oy / h as f64 - 1.0);
    let m = [
        ex.0 * sx, ex.1 * sy, 0.0, 0.0, //
        ey.0 * sx, ey.1 * sy, 0.0, 0.0, //
        ez.0 * sx, ez.1 * sy, 0.0, 0.0, //
        tx, ty, 0.0, 1.0,
    ];
    m.map(|v| v as f32)
}

/// Clip space for image pixels (y down), for the screen-space tool marker.
fn pixels(w: u32, h: u32) -> [f32; 16] {
    let (w, h) = (w as f32, h as f32);
    [2.0 / w, 0.0, 0.0, 0.0, 0.0, -2.0 / h, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 1.0, 0.0, 1.0]
}

fn rgba(rgb: [u8; 3], a: u8) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], a]
}

impl Gpu {
    /// `get_proc_address` from Slint's `GraphicsAPI::NativeOpenGL`.
    ///
    /// # Safety
    /// The GL context must be current, as it is in Slint's rendering notifier.
    pub unsafe fn new(get_proc_address: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void) -> Result<Gpu, String> {
        let gl = glow::Context::from_loader_function_cstr(|s| get_proc_address(s));
        let header = if gl.version().is_embedded { "#version 300 es\nprecision highp float;\n" } else { "#version 150\n" };
        let line_prog = program(&gl, header, LINE_VS, LINE_FS)?;
        let fill_prog = program(&gl, header, FILL_VS, FILL_FS)?;
        let saved = save(&gl);
        let rapids = Lines::new(&gl, line_prog)?;
        let feeds = Lines::new(&gl, line_prog)?;
        let scratch = Lines::new(&gl, line_prog)?;
        let fill_vao = gl.create_vertex_array()?;
        let fill_buf = gl.create_buffer()?;
        gl.bind_vertex_array(Some(fill_vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(fill_buf));
        if let Some(loc) = gl.get_attrib_location(fill_prog, "a_pos") {
            gl.enable_vertex_attrib_array(loc);
            gl.vertex_attrib_pointer_f32(loc, 3, glow::FLOAT, false, 16, 0);
        }
        if let Some(loc) = gl.get_attrib_location(fill_prog, "a_rgba") {
            gl.enable_vertex_attrib_array(loc);
            gl.vertex_attrib_pointer_f32(loc, 4, glow::UNSIGNED_BYTE, true, 16, 12);
        }
        gl.bind_vertex_array(None);
        restore(&gl, saved);
        Ok(Gpu { gl, line_prog, fill_prog, rapids, feeds, scratch, fill_vao, fill_buf, tp_id: None, targets: [None, None], next: 0 })
    }

    /// "renderer (version)" of the GL driver.
    pub fn describe(&self) -> String {
        unsafe { format!("{} ({})", self.gl.get_parameter_string(glow::RENDERER), self.gl.get_parameter_string(glow::VERSION)) }
    }

    fn set_toolpath(&mut self, tp: &Toolpath, id: usize) {
        if self.tp_id == Some(id) {
            return;
        }
        self.tp_id = Some(id);
        let mut rapids = Vec::new();
        let mut feeds = Vec::with_capacity(tp.segs.len());
        for s in &tp.segs {
            if s.kind == Kind::Rapid {
                rapids.push(inst(xyz(s.from), xyz(s.to), RAPID));
            } else {
                feeds.push(inst(xyz(s.from), xyz(s.to), rgba(OP_COLORS[s.op % OP_COLORS.len()], 230)));
            }
        }
        unsafe {
            self.rapids.upload(&self.gl, &rapids, glow::STATIC_DRAW);
            self.feeds.upload(&self.gl, &feeds, glow::STATIC_DRAW);
        }
    }

    unsafe fn target(&mut self, w: u32, h: u32) -> Result<(glow::Framebuffer, glow::Texture), String> {
        let gl = &self.gl;
        let slot = &mut self.targets[self.next];
        if slot.as_ref().is_some_and(|t| t.size != (w, h)) {
            let t = slot.take().expect("target");
            gl.delete_framebuffer(t.fbo);
            gl.delete_texture(t.tex);
        }
        if slot.is_none() {
            let tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w as i32, h as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            for (k, v) in [(glow::TEXTURE_MIN_FILTER, glow::LINEAR), (glow::TEXTURE_MAG_FILTER, glow::LINEAR), (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE), (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE)] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
            }
            let fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
            if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                return Err("backplot framebuffer incomplete".into());
            }
            *slot = Some(Target { fbo, tex, size: (w, h) });
        }
        let t = slot.as_ref().expect("target");
        self.next = 1 - self.next;
        Ok((t.fbo, t.tex))
    }

    /// Draw one frame into a texture and return its GL name. Call from
    /// Slint's `BeforeRendering` notifier (the context is current there).
    pub fn render(&mut self, tp: &Toolpath, tp_id: usize, f: &Frame) -> Result<glow::Texture, String> {
        let (w, h) = (f.width.max(16), f.height.max(16));
        self.set_toolpath(tp, tp_id);
        let fit = Fit::new(tp, f.view, w, h, f.zoom);
        unsafe {
            let saved = save(&self.gl);
            let out = self.draw(&fit, w, h, f);
            restore(&self.gl, saved);
            out
        }
    }

    unsafe fn draw(&mut self, fit: &Fit, w: u32, h: u32, f: &Frame) -> Result<glow::Texture, String> {
        let (fbo, tex) = self.target(w, h)?;
        let gl = &self.gl;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.viewport(0, 0, w as i32, h as i32);
        gl.disable(glow::SCISSOR_TEST);
        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::CULL_FACE);
        gl.enable(glow::BLEND);
        gl.blend_func_separate(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA, glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
        gl.clear_color(BG[0] as f32 / 255.0, BG[1] as f32 / 255.0, BG[2] as f32 / 255.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        let world = mvp(fit, w, h);
        let screen = pixels(w, h);

        // This frame's small line sets share one buffer, and its fills
        // another; each part is a (first, count) range drawn below in the
        // same order as software::Base::compose.
        let mut lines: Vec<Inst> = Vec::new();
        let mut fills: Vec<u8> = Vec::new();
        let range = |v: &mut Vec<Inst>, add: &mut dyn FnMut(&mut Vec<Inst>)| {
            let at = v.len();
            add(v);
            (at as i32, (v.len() - at) as i32)
        };
        let table = f.machine.map(|m| box_faces(fit.view, &m.table)).unwrap_or_default();
        let table_fill = push_fills(&mut fills, 0, table.iter().map(|(c, fill, _)| (c.to_vec(), *fill)));
        let table_edges = range(&mut lines, &mut |v| {
            for (c, _, edge) in &table {
                for k in 0..4 {
                    v.push(inst(c[k], c[(k + 1) % 4], *edge));
                }
            }
        });
        let slots = range(&mut lines, &mut |v| {
            for (a, b) in f.machine.map(|m| m.slots.as_slice()).unwrap_or_default() {
                v.push(inst(*a, *b, [0x12, 0x15, 0x18, 255]));
            }
        });
        let axes = range(&mut lines, &mut |v| {
            let len = 40.0 / fit.scale;
            for (i, rgb) in [[0xe0, 0x7a, 0x7a], [0x7f, 0xd6, 0xa0], [0x6d, 0x7c, 0xff]].into_iter().enumerate() {
                let mut e = [0.0; 3];
                e[i] = len;
                let (a, b) = (fit.map([0.0; 4]), fit.map([e[0], e[1], e[2], 0.0]));
                if (a.0 - b.0).abs() + (a.1 - b.1).abs() >= 1.0 {
                    v.push(inst([0.0; 3], e, rgba(rgb, 255)));
                }
            }
        });
        // The tool: its outline and top rim in image pixels.
        let mut cyl = None;
        if let Some(c) = f.machine.and_then(|m| m.tool.as_ref()) {
            cyl = Some(cylinder(fit, c));
        }
        let fills_world = fills.len() / 16;
        let mut tool_fills = (0, 0, 0, 0);
        let mut tool_lines = ((0, 0), (0, 0));
        if let Some((hull, top)) = &cyl {
            let at = (fills.len() / 16) as i32;
            let n_hull = push_fan(&mut fills, hull, [0x9a, 0x7a, 0x30, 110]);
            let n_top = top.as_ref().map(|t| push_fan(&mut fills, t, [0xc8, 0x9e, 0x40, 90])).unwrap_or(0);
            tool_fills = (at, n_hull, at + n_hull, n_top);
            let gold = rgba(TOOL, 255);
            let ring = |v: &mut Vec<Inst>, pts: &[(f32, f32)]| {
                for k in 0..pts.len() {
                    let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
                    v.push(inst([a.0 as f64, a.1 as f64, 0.0], [b.0 as f64, b.1 as f64, 0.0], gold));
                }
            };
            tool_lines = (range(&mut lines, &mut |v| ring(v, hull)), range(&mut lines, &mut |v| if let Some(t) = top { ring(v, t) }));
        }
        // The trail: load-coloured where measured (3 px), white elsewhere (2 px).
        let step = |i: usize| if f.loads.len() == f.trail.len() { load_step(f.loads[i]) } else { None };
        let loaded = range(&mut lines, &mut |v| {
            for i in 1..f.trail.len() {
                if let Some(s) = step(i) {
                    v.push(inst(xyz(f.trail[i - 1]), xyz(f.trail[i]), rgba(load_color(s as f32 / (LOAD_STEPS - 1) as f32), 255)));
                }
            }
        });
        let plain = range(&mut lines, &mut |v| {
            for i in 1..f.trail.len() {
                if step(i).is_none() {
                    v.push(inst(xyz(f.trail[i - 1]), xyz(f.trail[i]), [0xff, 0xff, 0xff, 200]));
                }
            }
        });
        let (mut circle, mut cross) = ((0, 0), (0, 0));
        if let Some(t) = f.tool {
            let c = fit.map(t);
            let c = (c.0 as f64, c.1 as f64);
            circle = range(&mut lines, &mut |v| {
                let n = 32;
                for i in 0..n {
                    let a0 = i as f64 / n as f64 * std::f64::consts::TAU;
                    let a1 = (i + 1) as f64 / n as f64 * std::f64::consts::TAU;
                    v.push(inst([c.0 + 9.0 * a0.cos(), c.1 + 9.0 * a0.sin(), 0.0], [c.0 + 9.0 * a1.cos(), c.1 + 9.0 * a1.sin(), 0.0], rgba(TOOL, 255)));
                }
            });
            cross = range(&mut lines, &mut |v| {
                v.push(inst([c.0 - 15.0, c.1, 0.0], [c.0 + 15.0, c.1, 0.0], rgba(TOOL, 255)));
                v.push(inst([c.0, c.1 - 15.0, 0.0], [c.0, c.1 + 15.0, 0.0], rgba(TOOL, 255)));
            });
        }
        self.scratch.upload(gl, &lines, glow::STREAM_DRAW);
        if !fills.is_empty() {
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.fill_buf));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &fills, glow::STREAM_DRAW);
        }
        let _ = fills_world;

        let set_lines = |m: &[f32; 16], width: f32| {
            gl.use_program(Some(self.line_prog));
            gl.uniform_matrix_4_f32_slice(gl.get_uniform_location(self.line_prog, "u_mvp").as_ref(), false, m);
            gl.uniform_2_f32(gl.get_uniform_location(self.line_prog, "u_px").as_ref(), w as f32, h as f32);
            gl.uniform_1_f32(gl.get_uniform_location(self.line_prog, "u_width").as_ref(), width);
        };
        let fill = |m: &[f32; 16], first: i32, count: i32| {
            if count > 0 {
                gl.use_program(Some(self.fill_prog));
                gl.uniform_matrix_4_f32_slice(gl.get_uniform_location(self.fill_prog, "u_mvp").as_ref(), false, m);
                gl.bind_vertex_array(Some(self.fill_vao));
                gl.draw_arrays(glow::TRIANGLES, first, count);
            }
        };
        let draw = |l: &Lines, r: (i32, i32)| l.draw(gl, self.line_prog, r.0, r.1);

        fill(&world, 0, table_fill);
        set_lines(&world, 1.0);
        draw(&self.scratch, table_edges);
        set_lines(&world, 3.0);
        draw(&self.scratch, slots);
        set_lines(&world, 2.0);
        draw(&self.scratch, axes);
        set_lines(&world, 1.0);
        draw(&self.rapids, (0, self.rapids.count));
        set_lines(&world, 1.5);
        draw(&self.feeds, (0, self.feeds.count));
        fill(&screen, tool_fills.0, tool_fills.1);
        set_lines(&screen, 1.5);
        draw(&self.scratch, tool_lines.0);
        fill(&screen, tool_fills.2, tool_fills.3);
        set_lines(&screen, 1.0);
        draw(&self.scratch, tool_lines.1);
        set_lines(&world, 3.0);
        draw(&self.scratch, loaded);
        set_lines(&world, 2.0);
        draw(&self.scratch, plain);
        set_lines(&screen, 3.0);
        draw(&self.scratch, circle);
        set_lines(&screen, 2.0);
        draw(&self.scratch, cross);
        Ok(tex)
    }

    /// The last drawn frame's pixels, RGBA top row first (for the web mirror).
    pub fn read_back(&mut self, tex: glow::Texture, w: u32, h: u32) -> Option<Vec<u8>> {
        let t = self.targets.iter().flatten().find(|t| t.tex == tex)?;
        let gl = &self.gl;
        let mut px = vec![0u8; (w * h * 4) as usize];
        unsafe {
            let saved = save(gl);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fbo));
            gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut px)));
            restore(gl, saved);
        }
        // GL rows run bottom to top.
        let row = (w * 4) as usize;
        let mut out = Vec::with_capacity(px.len());
        for r in px.chunks(row).rev() {
            out.extend_from_slice(r);
        }
        Some(out)
    }
}

/// The faces of a box that face the viewer: corners, shaded fill, edge.
fn box_faces(view: View, s: &Solid) -> Vec<([[f64; 3]; 4], [u8; 4], [u8; 4])> {
    let n = toward_viewer(view);
    let mut out = Vec::new();
    for axis in 0..3 {
        for side in [-1.0, 1.0] {
            if n[axis] * side <= 1e-6 {
                continue;
            }
            let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
            let at = if side > 0.0 { s.hi[axis] } else { s.lo[axis] };
            let mut c = [[0.0; 3]; 4];
            for (k, (a, b)) in [(0, 0), (1, 0), (1, 1), (0, 1)].into_iter().enumerate() {
                c[k][axis] = at;
                c[k][u] = if a == 0 { s.lo[u] } else { s.hi[u] };
                c[k][w] = if b == 0 { s.lo[w] } else { s.hi[w] };
            }
            // Tops lightest, then the sides (as software::draw_box).
            let shade = if axis == 2 { 1.0 } else if axis == 0 { 0.78 } else { 0.62 };
            let rgb = s.rgb.map(|v| (v as f64 * shade) as u8);
            let e = s.rgb.map(|v| v.saturating_add(30));
            out.push((c, rgba(rgb, 255), rgba(e, 255)));
        }
    }
    out
}

/// The tool cylinder in image pixels: its outline (the hull of its end
/// circles) and, when the eye is above it, its top rim.
fn cylinder(fit: &Fit, c: &Cylinder) -> (Vec<(f32, f32)>, Option<Vec<(f32, f32)>>) {
    const N: usize = 32;
    let ring = |z: f64| -> Vec<(f32, f32)> {
        (0..N)
            .map(|i| {
                let a = i as f64 * std::f64::consts::TAU / N as f64;
                fit.map([c.center[0] + c.r * a.cos(), c.center[1] + c.r * a.sin(), z, 0.0])
            })
            .collect()
    };
    let (bottom, top) = (ring(c.z0), ring(c.z1));
    let hull = convex_hull(bottom.iter().chain(top.iter()).copied().collect());
    (hull, (toward_viewer(fit.view)[2] > 0.05).then_some(top))
}

fn push_vertex(fills: &mut Vec<u8>, p: [f64; 3], rgba: [u8; 4]) {
    for v in p {
        fills.extend_from_slice(&(v as f32).to_ne_bytes());
    }
    fills.extend_from_slice(&rgba);
}

/// Quads (work coordinates) as triangles; returns the vertices added.
fn push_fills(fills: &mut Vec<u8>, mut n: i32, quads: impl Iterator<Item = (Vec<[f64; 3]>, [u8; 4])>) -> i32 {
    for (c, rgba) in quads {
        for k in [0, 1, 2, 0, 2, 3] {
            push_vertex(fills, c[k], rgba);
            n += 1;
        }
    }
    n
}

/// A convex polygon (image pixels) as a triangle fan; returns the vertices added.
fn push_fan(fills: &mut Vec<u8>, pts: &[(f32, f32)], rgba: [u8; 4]) -> i32 {
    let mut n = 0;
    for k in 1..pts.len().saturating_sub(1) {
        for p in [pts[0], pts[k], pts[k + 1]] {
            push_vertex(fills, [p.0 as f64, p.1 as f64, 0.0], rgba);
            n += 1;
        }
    }
    n
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            let gl = &self.gl;
            self.rapids.delete(gl);
            self.feeds.delete(gl);
            self.scratch.delete(gl);
            gl.delete_vertex_array(self.fill_vao);
            gl.delete_buffer(self.fill_buf);
            gl.delete_program(self.line_prog);
            gl.delete_program(self.fill_prog);
            for t in self.targets.iter().flatten() {
                gl.delete_framebuffer(t.fbo);
                gl.delete_texture(t.tex);
            }
        }
    }
}
