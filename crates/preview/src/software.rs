//! CPU renderer (tiny-skia). Draws the toolpath once into a cached base
//! image per view and size; the tool marker and live trail go on a copy.

use crate::machine::{Layer, Line, Solid};
use crate::{Kind, Toolpath, P4};
use tiny_skia::{Color, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Iso,
    Top,
    Front,
}

/// Op colors, matching the mockup.
pub const OP_COLORS: [[u8; 3]; 6] =
    [[0x6d, 0x7c, 0xff], [0xf3, 0xc4, 0x63], [0x7f, 0xd6, 0xa0], [0xe0, 0x7a, 0x7a], [0x8c, 0xc4, 0xef], [0xc4, 0x9c, 0xf0]];

const BG: [u8; 3] = [0x0f, 0x12, 0x14];

fn project(v: View, p: P4) -> (f64, f64) {
    match v {
        View::Top => (p[0], p[1]),
        View::Front => (p[0], p[2]),
        // Isometric: X right-down, Y right-up, Z up.
        View::Iso => ((p[0] - p[1]) * 0.7071, (p[0] + p[1]) * 0.4082 + p[2] * 0.8165),
    }
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
        let mut pts: Vec<(f64, f64)> = vec![project(view, [0.0; 4])];
        if let Some((lo, hi)) = tp.bounds() {
            for x in [lo[0], hi[0]] {
                for y in [lo[1], hi[1]] {
                    for z in [lo[2], hi[2]] {
                        pts.push(project(view, [x, y, z, 0.0]));
                    }
                }
            }
        }
        let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for (x, y) in pts {
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        let margin = 40.0;
        let sw = (x1 - x0).max(1.0);
        let sh = (y1 - y0).max(1.0);
        let scale = ((w as f64 - 2.0 * margin) / sw).min((h as f64 - 2.0 * margin) / sh);
        let ox = (w as f64 - sw * scale) / 2.0 - x0 * scale;
        let oy = (h as f64 - sh * scale) / 2.0 - y0 * scale;
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
        self.base.as_ref().expect("base").compose(trail, tool, None)
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
    /// under the toolpath, the moving parts see-through over it), the trail
    /// and the tool marker.
    pub fn compose(&self, trail: &[P4], tool: Option<P4>, machine: Option<&(Vec<Solid>, Vec<Line>)>) -> Pixmap {
        let mut pm = Pixmap::new(self.pm.width(), self.pm.height()).expect("pixmap size");
        pm.fill(Color::from_rgba8(BG[0], BG[1], BG[2], 255));
        let fit = self.fit;
        if let Some((solids, lines)) = machine {
            draw_solids(&mut pm, &fit, solids, Layer::Under);
            let mut pb = PathBuilder::new();
            for (a, b) in lines {
                let (a, b) = (fit.map([a[0], a[1], a[2], 0.0]), fit.map([b[0], b[1], b[2], 0.0]));
                pb.move_to(a.0, a.1);
                pb.line_to(b.0, b.1);
            }
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &paint([0x12, 0x15, 0x18], 255), &stroke(3.0), Transform::identity(), None);
            }
        }
        pm.draw_pixmap(0, 0, self.pm.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
        if let Some((solids, _)) = machine {
            if fit.view != View::Top {
                draw_solids(&mut pm, &fit, solids, Layer::Over);
            }
            draw_solids(&mut pm, &fit, solids, Layer::Tool);
        }
        if trail.len() > 1 {
            let mut pb = PathBuilder::new();
            let a = fit.map(trail[0]);
            pb.move_to(a.0, a.1);
            for p in &trail[1..] {
                let b = fit.map(*p);
                pb.line_to(b.0, b.1);
            }
            if let Some(path) = pb.finish() {
                pm.stroke_path(&path, &paint([0xff, 0xff, 0xff], 200), &stroke(2.0), Transform::identity(), None);
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
    match v {
        View::Top => [0.0, 0.0, 1.0],
        View::Front => [0.0, -1.0, 0.0],
        View::Iso => [-1.0, -1.0, 1.0],
    }
}

/// Boxes as flat-shaded faces, farthest first. The table is solid; the
/// moving parts are see-through so the toolpath stays readable.
fn draw_solids(pm: &mut Pixmap, fit: &Fit, solids: &[Solid], layer: Layer) {
    let n = toward_viewer(fit.view);
    let depth = |s: &Solid| (0..3).map(|i| (s.lo[i] + s.hi[i]) / 2.0 * n[i]).sum::<f64>();
    let mut list: Vec<&Solid> = solids.iter().filter(|s| s.layer == layer).collect();
    list.sort_by(|a, b| depth(a).total_cmp(&depth(b)));
    let (fill, edge) = match layer {
        Layer::Under | Layer::Tool => (255, 255),
        Layer::Over => (0, 80),
    };
    for s in list {
        for axis in 0..3 {
            for side in [-1.0, 1.0] {
                if n[axis] * side <= 0.0 {
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
                if fill > 0 {
                    pm.fill_path(&path, &paint(rgb, fill), tiny_skia::FillRule::Winding, Transform::identity(), None);
                }
                pm.stroke_path(&path, &paint(s.rgb.map(|c| c.saturating_add(30)), edge), &stroke(1.0), Transform::identity(), None);
            }
        }
    }
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
                base.compose(&[], Some([50.0, 30.0, 5.0, 0.0]), if with { Some(&scene) } else { None });
            }
            eprintln!("machine={with}: {:?}/frame", t.elapsed() / 20);
        }
    }
}
