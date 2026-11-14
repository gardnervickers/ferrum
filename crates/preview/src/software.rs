//! CPU renderer (tiny-skia). Draws the toolpath once into a cached base
//! image per view and size; the tool marker and live trail go on a copy.

use crate::machine::{Cylinder, Scene, Solid};
use crate::{Kind, Toolpath, P4};
use tiny_skia::{Color, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// The camera direction, in whole degrees so it can key the image cache.
/// `yaw` turns about Z (0 looks from the front, along +Y; 45 from front
/// left); `pitch` raises the eye (0 level, 90 straight down).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct View {
    pub yaw: i16,
    pub pitch: i16,
}

#[allow(non_upper_case_globals)]
impl View {
    /// True isometric: the eye along (-1, -1, 1).
    pub const Iso: View = View { yaw: 45, pitch: 35 };
    pub const Top: View = View { yaw: 0, pitch: 90 };
    pub const Front: View = View { yaw: 0, pitch: 0 };

    /// Turned by `dyaw` and raised by `dpitch` degrees; pitch stays between
    /// straight up and straight down, yaw wraps.
    pub fn orbited(self, dyaw: f64, dpitch: f64) -> View {
        let yaw = (self.yaw as f64 + dyaw).round() as i32;
        let yaw = ((yaw + 180).rem_euclid(360) - 180) as i16;
        let pitch = (self.pitch as f64 + dpitch).round().clamp(-90.0, 90.0) as i16;
        View { yaw, pitch }
    }

    /// Screen right, screen up and toward the eye, as unit vectors.
    pub(crate) fn basis(self) -> ([f64; 3], [f64; 3], [f64; 3]) {
        // The exact isometric pitch, so Iso matches the classic projection.
        let iso = 35.264_389_682_754_654_f64;
        let pitch = match self.pitch { 35 => iso, -35 => -iso, p => p as f64 };
        let (sy, cy) = (self.yaw as f64).to_radians().sin_cos();
        let (sp, cp) = pitch.to_radians().sin_cos();
        ([cy, -sy, 0.0], [sy * sp, cy * sp, cp], [-sy * cp, -cy * cp, sp])
    }
}

/// Op colors, matching the mockup.
pub const OP_COLORS: [[u8; 3]; 6] =
    [[0x6d, 0x7c, 0xff], [0xf3, 0xc4, 0x63], [0x7f, 0xd6, 0xa0], [0xe0, 0x7a, 0x7a], [0x8c, 0xc4, 0xef], [0xc4, 0x9c, 0xf0]];

const BG: [u8; 3] = [0x0f, 0x12, 0x14];

fn project(v: View, p: P4) -> (f64, f64) {
    let (r, u, _) = v.basis();
    (r[0] * p[0] + r[1] * p[1] + r[2] * p[2], u[0] * p[0] + u[1] * p[1] + u[2] * p[2])
}

/// The user's zoom and pan on top of the fitted view: `zoom` scales about
/// the image center, then the image moves by `pan` pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoom {
    pub zoom: f64,
    pub pan: (f64, f64),
}

impl Default for Zoom {
    fn default() -> Self {
        Self { zoom: 1.0, pan: (0.0, 0.0) }
    }
}

impl Zoom {
    pub const MIN: f64 = 0.5;
    pub const MAX: f64 = 200.0;

    /// Zoom by `factor` keeping the image point `at` (pixels) fixed.
    pub fn zoom_at(self, factor: f64, at: (f64, f64), size: (u32, u32)) -> Zoom {
        let z1 = (self.zoom * factor).clamp(Self::MIN, Self::MAX);
        let (cx, cy) = (size.0 as f64 / 2.0, size.1 as f64 / 2.0);
        // Fitted-image point under `at`, then where it lands at the new zoom.
        let ax = cx + (at.0 - self.pan.0 - cx) / self.zoom;
        let ay = cy + (at.1 - self.pan.1 - cy) / self.zoom;
        Zoom { zoom: z1, pan: (at.0 - cx - (ax - cx) * z1, at.1 - cy - (ay - cy) * z1) }
    }

    pub fn panned(self, dx: f64, dy: f64) -> Zoom {
        Zoom { zoom: self.zoom, pan: (self.pan.0 + dx, self.pan.1 + dy) }
    }
}

/// Screen mapping that fits the toolpath (and the work origin) in the image.
#[derive(Clone, Copy)]
struct Fit {
    view: View,
    scale: f64,
    ox: f64,
    oy: f64,
    h: f64,
}

impl Fit {
    fn new(tp: &Toolpath, view: View, w: u32, h: u32, z: Zoom) -> Self {
        // The toolpath box, grown to take in the work origin.
        let (mut lo, mut hi) = ([0.0f64; 3], [0.0f64; 3]);
        if let Some((a, b)) = tp.bounds() {
            for i in 0..3 {
                lo[i] = a[i].min(0.0);
                hi[i] = b[i].max(0.0);
            }
        }
        // Fit the box's bounding sphere, not its outline in this view, so
        // the scale holds still while the view orbits.
        let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0, 0.0];
        let d = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt().max(1.0);
        let margin = 40.0;
        let scale = ((w as f64 - 2.0 * margin) / d).min((h as f64 - 2.0 * margin) / d);
        let (px, py) = project(view, c);
        let ox = w as f64 / 2.0 - px * scale;
        let oy = h as f64 / 2.0 - py * scale;
        // Apply the zoom about the center and the pan (y points down on screen).
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let scale_z = scale * z.zoom;
        let ox = cx + (ox - cx) * z.zoom + z.pan.0;
        let oy = cy + (oy - cy) * z.zoom - z.pan.1;
        Self { view, scale: scale_z, ox, oy, h: h as f64 }
    }

    fn map(&self, p: P4) -> (f32, f32) {
        let (x, y) = project(self.view, p);
        ((self.ox + x * self.scale) as f32, (self.h - (self.oy + y * self.scale)) as f32)
    }
}

fn paint(rgb: [u8; 3], alpha: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(rgb[0], rgb[1], rgb[2], alpha));
    p.anti_alias = true;
    p
}

fn stroke(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, ..Default::default() }
}

/// Builds a path from segments in screen space: consecutive segments join
/// into one polyline, and moves shorter than a fraction of a pixel merge into
/// the next, so a dense program costs about as much as the pixels it covers.
#[derive(Default)]
struct Polyline {
    pb: PathBuilder,
    /// Last point emitted, and the latest point not yet emitted.
    drawn: Option<(f32, f32)>,
    pending: Option<(f32, f32)>,
}

impl Polyline {
    const MERGE: f32 = 0.6;

    fn pen(&self) -> Option<(f32, f32)> {
        self.pending.or(self.drawn)
    }

    fn seg(&mut self, a: (f32, f32), b: (f32, f32)) {
        let joined = self.pen().is_some_and(|p| (p.0 - a.0).abs() < 0.05 && (p.1 - a.1).abs() < 0.05);
        if !joined {
            self.flush();
            self.pb.move_to(a.0, a.1);
            self.drawn = Some(a);
        }
        let d = self.drawn.expect("pen");
        if (b.0 - d.0).abs() < Self::MERGE && (b.1 - d.1).abs() < Self::MERGE {
            self.pending = Some(b);
        } else {
            self.pb.line_to(b.0, b.1);
            self.drawn = Some(b);
            self.pending = None;
        }
    }

    fn flush(&mut self) {
        if let Some(p) = self.pending.take() {
            self.pb.line_to(p.0, p.1);
            self.drawn = Some(p);
        }
    }

    fn finish(mut self) -> Option<tiny_skia::Path> {
        // A lone point (all moves merged) still needs a line to show its cap.
        if let (Some(d), None) = (self.drawn, self.pending) {
            if self.pb.len() == 1 {
                self.pb.line_to(d.0 + 0.01, d.1);
            }
        }
        self.flush();
        self.pb.finish()
    }
}

/// The toolpath drawn for one view and size. Drawing it is the slow part
/// (proportional to the program), so callers can do it off the UI thread and
/// `compose` the live marker and trail onto it each frame.
// (see `load_color` for the trail's load colours)
pub struct Base {
    pm: Pixmap,
    fit: Fit,
}

pub struct Software {
    key: Option<(usize, View, u32, u32, Option<usize>)>,
    base: Option<Base>,
}

impl Default for Software {
    fn default() -> Self {
        Self::new()
    }
}

impl Software {
    pub fn new() -> Self {
        Self { key: None, base: None }
    }

    /// Render to RGBA8 (opaque). `tool` is the current work position;
    /// `trail` is where the tool has been this session.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        tp: &Toolpath,
        tp_id: usize,
        view: View,
        w: u32,
        h: u32,
        highlight: Option<usize>,
        trail: &[P4],
        tool: Option<P4>,
    ) -> Pixmap {
        let key = (tp_id, view, w, h, highlight);
        if self.key != Some(key) {
            self.base = Some(Base::draw(tp, view, w, h, highlight, Zoom::default()));
            self.key = Some(key);
        }
        self.base.as_ref().expect("base").compose(trail, &[], tool, None)
    }
}

impl Base {
    pub fn draw(tp: &Toolpath, view: View, w: u32, h: u32, highlight: Option<usize>, zoom: Zoom) -> Base {
        let w = w.max(16);
        let h = h.max(16);
        // Transparent: `compose` puts it over the background and the table.
        let mut pm = Pixmap::new(w, h).expect("pixmap size");
        let fit = Fit::new(tp, view, w, h, zoom);

        // Work origin axes.
        let o = fit.map([0.0; 4]);
        for (dir, rgb) in [([20.0, 0.0, 0.0, 0.0], [0xe0, 0x7a, 0x7a]), ([0.0, 20.0, 0.0, 0.0], [0x7f, 0xd6, 0xa0]), ([0.0, 0.0, 20.0, 0.0], [0x6d, 0x7c, 0xff])] {
            let len = 40.0 / fit.scale;
            let e = fit.map([dir[0] / 20.0 * len, dir[1] / 20.0 * len, dir[2] / 20.0 * len, 0.0]);
            if (e.0 - o.0).abs() + (e.1 - o.1).abs() < 1.0 {
                continue;
            }
            let mut pb = PathBuilder::new();
            pb.move_to(o.0, o.1);
            pb.line_to(e.0, e.1);
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &paint(rgb, 255), &stroke(2.0), Transform::identity(), None);
            }
        }

        // Rapids dashed-looking (thin, dim), then feeds per op.
        let mut rapids = Polyline::default();
        let mut feeds: Vec<Polyline> = (0..tp.ops.len().max(1)).map(|_| Polyline::default()).collect();
        for s in &tp.segs {
            let a = fit.map(s.from);
            let b = fit.map(s.to);
            let pl = if s.kind == Kind::Rapid { &mut rapids } else { &mut feeds[s.op] };
            pl.seg(a, b);
        }
        if let Some(path) = rapids.finish() {
            pm.stroke_path(&path, &paint([0x6b, 0x74, 0x7b], 150), &stroke(1.0), Transform::identity(), None);
        }
        for (i, pb) in feeds.into_iter().enumerate() {
            let dim = highlight.is_some_and(|h| h != i);
            if let Some(path) = pb.finish() {
                let rgb = OP_COLORS[i % OP_COLORS.len()];
                pm.stroke_path(&path, &paint(rgb, if dim { 40 } else { 230 }), &stroke(1.5), Transform::identity(), None);
            }
        }
        Base { pm, fit }
    }

    /// The base over the background, with the machine (if given: the table
    /// under the toolpath, the tool over it), the trail and the tool marker.
    ///
    /// `loads` colours the trail: one value per trail point, 0 (idle) to 1
    /// (full load), NaN where none was measured. Empty draws it plain.
    pub fn compose(&self, trail: &[P4], loads: &[f32], tool: Option<P4>, machine: Option<&Scene>) -> Pixmap {
        let mut pm = Pixmap::new(self.pm.width(), self.pm.height()).expect("pixmap size");
        pm.fill(Color::from_rgba8(BG[0], BG[1], BG[2], 255));
        let fit = self.fit;
        if let Some(scene) = machine {
            draw_box(&mut pm, &fit, &scene.table);
            let mut pb = PathBuilder::new();
            for (a, b) in &scene.slots {
                let (a, b) = (fit.map([a[0], a[1], a[2], 0.0]), fit.map([b[0], b[1], b[2], 0.0]));
                pb.move_to(a.0, a.1);
                pb.line_to(b.0, b.1);
            }
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &paint([0x12, 0x15, 0x18], 255), &stroke(3.0), Transform::identity(), None);
            }
        }
        pm.draw_pixmap(0, 0, self.pm.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
        if let Some(c) = machine.and_then(|m| m.tool.as_ref()) {
            draw_cylinder(&mut pm, &fit, c);
        }
        if trail.len() > 1 {
            // One path per colour step, so a noisy signal costs at most
            // LOAD_STEPS + 1 strokes however long the trail is.
            let step = |i: usize| if loads.len() == trail.len() { load_step(loads[i]) } else { None };
            let mut paths: Vec<(PathBuilder, usize)> = (0..=LOAD_STEPS).map(|_| (PathBuilder::new(), usize::MAX)).collect();
            for i in 1..trail.len() {
                let slot = step(i).unwrap_or(LOAD_STEPS);
                let (pb, last) = &mut paths[slot];
                if *last != i - 1 {
                    let a = fit.map(trail[i - 1]);
                    pb.move_to(a.0, a.1);
                }
                let b = fit.map(trail[i]);
                pb.line_to(b.0, b.1);
                *last = i;
            }
            for (slot, (pb, _)) in paths.into_iter().enumerate() {
                let Some(path) = pb.finish() else { continue };
                let (rgb, alpha, width) = if slot < LOAD_STEPS { (load_color(slot as f32 / (LOAD_STEPS - 1) as f32), 255, 3.0) } else { ([0xff, 0xff, 0xff], 200, 2.0) };
                pm.stroke_path(&path, &paint(rgb, alpha), &stroke(width), Transform::identity(), None);
            }
        }
        if let Some(t) = tool {
            let c = fit.map(t);
            if let Some(circle) = PathBuilder::from_circle(c.0, c.1, 9.0) {
                pm.stroke_path(&circle, &paint([0xf2, 0xc1, 0x4e], 255), &stroke(3.0), Transform::identity(), None);
            }
            let mut pb = PathBuilder::new();
            pb.move_to(c.0 - 15.0, c.1);
            pb.line_to(c.0 + 15.0, c.1);
            pb.move_to(c.0, c.1 - 15.0);
            pb.line_to(c.0, c.1 + 15.0);
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &paint([0xf2, 0xc1, 0x4e], 255), &stroke(2.0), Transform::identity(), None);
            }
        }
        pm
    }
}

/// Toward the viewer, for back-face culling and depth order.
fn toward_viewer(v: View) -> [f64; 3] {
    v.basis().2
}

/// A box as flat-shaded faces (the ones facing the viewer).
fn draw_box(pm: &mut Pixmap, fit: &Fit, s: &Solid) {
    let n = toward_viewer(fit.view);
    for axis in 0..3 {
        for side in [-1.0, 1.0] {
            if n[axis] * side <= 1e-6 {
                continue;
            }
            // The face's four corners, in order around it.
            let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
            let at = if side > 0.0 { s.hi[axis] } else { s.lo[axis] };
            let mut pb = PathBuilder::new();
            for (k, (a, b)) in [(0, 0), (1, 0), (1, 1), (0, 1)].into_iter().enumerate() {
                let mut p = [0.0; 4];
                p[axis] = at;
                p[u] = if a == 0 { s.lo[u] } else { s.hi[u] };
                p[w] = if b == 0 { s.lo[w] } else { s.hi[w] };
                let q = fit.map(p);
                if k == 0 { pb.move_to(q.0, q.1) } else { pb.line_to(q.0, q.1) }
            }
            pb.close();
            let Some(path) = pb.finish() else { continue };
            // Tops lightest, then the sides.
            let shade = if axis == 2 { 1.0 } else if axis == 0 { 0.78 } else { 0.62 };
            let rgb = s.rgb.map(|c| (c as f64 * shade) as u8);
            pm.fill_path(&path, &paint(rgb, 255), tiny_skia::FillRule::Winding, Transform::identity(), None);
            pm.stroke_path(&path, &paint(s.rgb.map(|c| c.saturating_add(30)), 255), &stroke(1.0), Transform::identity(), None);
        }
    }
}

/// The tool: a cylinder's outline (the hull of its end circles under the
/// parallel projection) with the top rim, see-through so the path under a
/// large cutter stays visible.
fn draw_cylinder(pm: &mut Pixmap, fit: &Fit, c: &Cylinder) {
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
    let poly = |pts: &[(f32, f32)]| {
        let mut pb = PathBuilder::new();
        for (k, p) in pts.iter().enumerate() {
            if k == 0 { pb.move_to(p.0, p.1) } else { pb.line_to(p.0, p.1) }
        }
        pb.close();
        pb.finish()
    };
    let gold = [0xf2, 0xc1, 0x4e];
    if let Some(path) = poly(&hull) {
        pm.fill_path(&path, &paint([0x9a, 0x7a, 0x30], 110), tiny_skia::FillRule::Winding, Transform::identity(), None);
        pm.stroke_path(&path, &paint(gold, 255), &stroke(1.5), Transform::identity(), None);
    }
    if toward_viewer(fit.view)[2] > 0.05 {
        if let Some(path) = poly(&top) {
            pm.fill_path(&path, &paint([0xc8, 0x9e, 0x40], 90), tiny_skia::FillRule::Winding, Transform::identity(), None);
            pm.stroke_path(&path, &paint(gold, 255), &stroke(1.0), Transform::identity(), None);
        }
    }
}

/// Monotone chain; returns the hull counter-clockwise.
fn convex_hull(mut pts: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut lower: Vec<(f32, f32)> = Vec::new();
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<(f32, f32)> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_something_in_each_view() {
        let tp = crate::mini::parse("G0 X0 Y0 Z5\nG1 Z0 F100\nX50\nY30\nX0\nY0\n", 100.0);
        let mut r = Software::new();
        for v in [View::Iso, View::Top, View::Front] {
            let pm = r.render(&tp, 1, v, 200, 150, None, &[], Some([10.0, 10.0, 0.0, 0.0]));
            let lit = pm.data().chunks(4).filter(|p| p[0] > 0x40 || p[1] > 0x40 || p[2] > 0x40).count();
            assert!(lit > 100, "{v:?}: {lit}");
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    #[test]
    #[ignore]
    fn compose_time() {
        let tp = crate::mini::parse("G0 X0 Y0 Z5\nG1 Z0 F100\nX100\nY60\nX0\nY0\n", 100.0);
        let base = Base::draw(&std::sync::Arc::new(tp), View::Iso, 1100, 700, None, Default::default());
        let m = crate::machine::Machine::pm728vt();
        let pose = crate::machine::Pose { tip: [50.0, 30.0, 5.0, 0.0], tool_len: 80.0, tool_dia: 10.0, offset: [100.0, -90.0, -200.0] };
        let scene = m.scene(&pose);
        for with in [false, true] {
            let t = std::time::Instant::now();
            for _ in 0..20 {
                base.compose(&[], &[], Some([50.0, 30.0, 5.0, 0.0]), if with { Some(&scene) } else { None });
            }
            eprintln!("machine={with}: {:?}/frame", t.elapsed() / 20);
        }
    }
}

/// Colour steps for the load-coloured trail: few enough that a long cut is
/// a handful of strokes.
const LOAD_STEPS: usize = 8;

fn load_step(load: f32) -> Option<usize> {
    load.is_finite().then(|| (load.clamp(0.0, 1.0) * (LOAD_STEPS - 1) as f32).round() as usize)
}

/// Green at no load, amber at half, red at full (the theme's status tones).
pub fn load_color(load: f32) -> [u8; 3] {
    const LOW: [f32; 3] = [0x6f as f32, 0xdc as f32, 0x9c as f32];
    const MID: [f32; 3] = [0xf3 as f32, 0xc4 as f32, 0x63 as f32];
    const HIGH: [f32; 3] = [0xee as f32, 0x5a as f32, 0x54 as f32];
    let t = load.clamp(0.0, 1.0);
    let (a, b, k) = if t < 0.5 { (LOW, MID, t * 2.0) } else { (MID, HIGH, t * 2.0 - 1.0) };
    [0, 1, 2].map(|i| (a[i] + (b[i] - a[i]) * k).round() as u8)
}

#[cfg(test)]
mod load_tests {
    use super::*;

    #[test]
    fn load_colours_run_green_amber_red() {
        assert_eq!(load_color(0.0), [0x6f, 0xdc, 0x9c]);
        assert_eq!(load_color(0.5), [0xf3, 0xc4, 0x63]);
        assert_eq!(load_color(1.0), [0xee, 0x5a, 0x54]);
        assert_eq!(load_color(7.0), load_color(1.0));
    }

    #[test]
    fn unmeasured_points_have_no_step() {
        assert_eq!(load_step(f32::NAN), None);
        assert_eq!(load_step(0.0), Some(0));
        assert_eq!(load_step(1.0), Some(LOAD_STEPS - 1));
    }
}
