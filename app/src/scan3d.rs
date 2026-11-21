//! PROBE › BOSS with no size hint (it scans): the 3D view of what the scan touched.
//!
//! The scan macro (`macros/ferrum_span_scan.ngc`) reports every touch as
//! `ferrum_scan.receive_point{kind, x, y, z}`; when the routine ends the
//! boss results give the fitted part (centre, widths, top). This turns
//! those into SVG path data and spots in the PROBE picture's 400 x 300 box,
//! seen from a direction the operator turns by dragging. Only lines, flat
//! fills and round spots, so it costs the same as the 2D picture on the Pi.

/// One touch, in the work coordinates the routine ran in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch {
    pub kind: Kind,
    pub p: [f64; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A touch-down that found nothing within its reach: past the top.
    Miss,
    /// A touch-down on the top.
    Top,
    /// A side touch: the face point, tip radius taken out.
    Side,
}

impl Kind {
    pub fn from_code(c: f64) -> Option<Kind> {
        match c.round() as i64 {
            0 => Some(Kind::Miss),
            1 => Some(Kind::Top),
            2 => Some(Kind::Side),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Kind::Miss => "miss",
            Kind::Top => "top",
            Kind::Side => "side",
        }
    }
}

/// The part the boss results describe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub center: (f64, f64),
    pub size: (f64, f64),
    pub top: f64,
    pub round: bool,
}

/// Which way the picture looks: yaw about Z and the angle down from level,
/// in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub yaw: f64,
    pub pitch: f64,
}

impl Default for View {
    fn default() -> Self {
        View { yaw: -30.0, pitch: 35.0 }
    }
}

impl View {
    /// Turned by a drag of (dx, dy) picture units.
    pub fn dragged(self, dx: f64, dy: f64) -> View {
        View { yaw: (self.yaw - dx * 0.6).rem_euclid(360.0), pitch: (self.pitch + dy * 0.6).clamp(0.0, 90.0) }
    }
}

/// A spot or a label at a place in the picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub x: f64,
    pub y: f64,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drawn {
    /// The fitted part's top, filled faintly.
    pub face: String,
    /// The fitted part's outline.
    pub edges: String,
    /// The part's sides below the deepest touch: how far down it goes is
    /// not measured, so they are drawn quieter.
    pub below: String,
    /// The scan's path between the touches (while it runs).
    pub path: String,
    /// Touches: `text` is the kind's id ("top", "side", "miss").
    pub touches: Vec<Mark>,
    /// Sizes beside the edges they measure.
    pub labels: Vec<Mark>,
}

const W: f64 = 400.0;
const H: f64 = 300.0;

struct Cam {
    v: View,
    c: [f64; 3],
    s: f64,
}

impl Cam {
    fn new(v: View, lo: [f64; 3], hi: [f64; 3]) -> Cam {
        let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
        // Fit the bounding box's corners as seen from this side.
        let unit = Cam { v, c, s: 1.0 };
        let (mut wx, mut wy) = (1e-6_f64, 1e-6_f64);
        for i in 0..8 {
            let p = [if i & 1 == 0 { lo[0] } else { hi[0] }, if i & 2 == 0 { lo[1] } else { hi[1] }, if i & 4 == 0 { lo[2] } else { hi[2] }];
            let (x, y, _) = unit.at(p);
            wx = wx.max((x - W / 2.0).abs());
            wy = wy.max((y - H / 2.0).abs());
        }
        let s = (W * 0.42 / wx).min(H * 0.40 / wy);
        Cam { v, c, s }
    }
    /// Picture x, y and depth (larger is nearer the viewer).
    fn at(&self, p: [f64; 3]) -> (f64, f64, f64) {
        let (y, t) = (self.v.yaw.to_radians(), self.v.pitch.to_radians());
        let (x, yy, z) = (p[0] - self.c[0], p[1] - self.c[1], p[2] - self.c[2]);
        let x1 = x * y.cos() - yy * y.sin();
        let y1 = x * y.sin() + yy * y.cos();
        let up = z * t.cos() + y1 * t.sin();
        let depth = -y1 * t.cos() + z * t.sin();
        (W / 2.0 + x1 * self.s, H / 2.0 - up * self.s, depth)
    }
    fn pt(&self, p: [f64; 3]) -> (f64, f64) {
        let (x, y, _) = self.at(p);
        (x, y)
    }
}

fn poly(out: &mut String, pts: &[(f64, f64)], close: bool) {
    for (i, (x, y)) in pts.iter().enumerate() {
        out.push_str(&format!("{}{:.1} {:.1} ", if i == 0 { "M " } else { "L " }, x, y));
    }
    if close && !pts.is_empty() {
        out.push_str("Z ");
    }
}

/// The outline of the fitted part at height z (a rectangle, or 48 points
/// round an ellipse).
fn ring(f: &Fit, z: f64) -> Vec<[f64; 3]> {
    let (cx, cy) = f.center;
    let (hx, hy) = (f.size.0 / 2.0, f.size.1 / 2.0);
    if f.round {
        (0..48).map(|i| {
            let a = i as f64 / 48.0 * std::f64::consts::TAU;
            [cx + hx * a.cos(), cy + hy * a.sin(), z]
        })
        .collect()
    } else {
        vec![[cx - hx, cy - hy, z], [cx + hx, cy - hy, z], [cx + hx, cy + hy, z], [cx - hx, cy + hy, z]]
    }
}

pub fn draw(touches: &[Touch], fit: Option<&Fit>, view: View, units: &str, decimals: usize) -> Drawn {
    let mut d = Drawn::default();
    if touches.is_empty() && fit.is_none() {
        return d;
    }
    // How deep the sides were touched, and how far below that to draw.
    let top = fit.map(|f| f.top).or_else(|| touches.iter().filter(|t| t.kind == Kind::Top).map(|t| t.p[2]).reduce(f64::max)).unwrap_or(0.0);
    let deepest = touches.iter().filter(|t| t.kind == Kind::Side).map(|t| t.p[2]).reduce(f64::min);
    let touched = deepest.map_or(0.0, |z| (top - z).max(0.0));
    let span = fit.map_or(0.0, |f| f.size.0.max(f.size.1));
    let base = top - (touched * 1.6).max(span * 0.25).max(1e-3);

    // Bounds: every touch and the drawn part.
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    let mut grow = |p: [f64; 3]| {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    };
    for t in touches {
        grow(t.p);
    }
    if let Some(f) = fit {
        for p in ring(f, top).into_iter().chain(ring(f, base)) {
            grow(p);
        }
    }
    let cam = Cam::new(view, lo, hi);

    if let Some(f) = fit {
        let top_ring: Vec<(f64, f64)> = ring(f, top).into_iter().map(|p| cam.pt(p)).collect();
        let mid = touched.max((top - base) * 0.5);
        let mid_ring: Vec<(f64, f64)> = ring(f, top - mid).into_iter().map(|p| cam.pt(p)).collect();
        let base_ring: Vec<(f64, f64)> = ring(f, base).into_iter().map(|p| cam.pt(p)).collect();
        poly(&mut d.face, &top_ring, true);
        poly(&mut d.edges, &top_ring, true);
        poly(&mut d.below, &base_ring, true);
        if f.round {
            // The outline's sides: the leftmost and rightmost points of the top.
            for pick in [true, false] {
                let i = (0..top_ring.len())
                    .max_by(|a, b| {
                        let (xa, xb) = (top_ring[*a].0, top_ring[*b].0);
                        if pick { xb.total_cmp(&xa) } else { xa.total_cmp(&xb) }
                    })
                    .unwrap_or(0);
                poly(&mut d.edges, &[top_ring[i], mid_ring[i]], false);
                poly(&mut d.below, &[mid_ring[i], base_ring[i]], false);
            }
        } else {
            for i in 0..4 {
                poly(&mut d.edges, &[top_ring[i], mid_ring[i]], false);
                poly(&mut d.below, &[mid_ring[i], base_ring[i]], false);
            }
        }
        let n = |v: f64| format!("{v:.decimals$}");
        let (cx, cy) = f.center;
        let (hx, hy) = (f.size.0 / 2.0, f.size.1 / 2.0);
        // Sizes beside the near edges: X along the front (-Y), Y along
        // whichever side faces the viewer.
        let yside = if view.yaw.rem_euclid(360.0) < 180.0 { -1.0 } else { 1.0 };
        // Kept inside the picture, clear of its edge.
        let inside = |(x, y): (f64, f64)| (x.clamp(40.0, W - 40.0), y.clamp(16.0, H - 16.0));
        let (fx, fy) = inside(cam.pt([cx, cy - hy * 1.25 - span * 0.04, top]));
        let (sx, sy) = inside(cam.pt([cx + yside * (hx * 1.25 + span * 0.04), cy, top]));
        if f.round && (f.size.0 - f.size.1).abs() < f.size.0.max(f.size.1) * 0.01 {
            d.labels.push(Mark { x: fx, y: fy, text: format!("⌀ {} {units}", n((f.size.0 + f.size.1) / 2.0)) });
        } else {
            d.labels.push(Mark { x: fx, y: fy, text: format!("X {}", n(f.size.0)) });
            d.labels.push(Mark { x: sx, y: sy, text: format!("Y {}", n(f.size.1)) });
        }
    }

    // While it runs, the probe's way between touches in the order it made
    // them; the finished part has its outline instead.
    if fit.is_none() {
        let path: Vec<(f64, f64)> = touches.iter().map(|t| cam.pt(t.p)).collect();
        poly(&mut d.path, &path, false);
    }
    // Far ones first, so the near ones are drawn over them.
    let mut ts: Vec<(f64, &Touch)> = touches.iter().map(|t| (cam.at(t.p).2, t)).collect();
    ts.sort_by(|a, b| a.0.total_cmp(&b.0));
    d.touches = ts.into_iter().map(|(_, t)| {
        let (x, y) = cam.pt(t.p);
        Mark { x, y, text: t.kind.id().into() }
    })
    .collect();
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block() -> (Vec<Touch>, Fit) {
        let t = |k, x, y, z| Touch { kind: k, p: [x, y, z] };
        let touches = vec![
            t(Kind::Top, 0.0, 0.0, 0.0),
            t(Kind::Top, -10.0, 0.0, 0.0),
            t(Kind::Miss, -60.0, 0.0, -2.0),
            t(Kind::Side, -50.0, 0.0, -5.0),
            t(Kind::Side, 50.0, 0.0, -5.0),
            t(Kind::Side, 0.0, -25.0, -5.0),
            t(Kind::Side, 0.0, 25.0, -5.0),
        ];
        (touches, Fit { center: (0.0, 0.0), size: (100.0, 50.0), top: 0.0, round: false })
    }

    #[test]
    fn everything_fits_the_picture_from_any_side() {
        let (touches, fit) = block();
        for yaw in (0..360).step_by(30) {
            for pitch in [0.0, 35.0, 90.0] {
                let d = draw(&touches, Some(&fit), View { yaw: yaw as f64, pitch }, "mm", 3);
                for m in d.touches.iter().chain(d.labels.iter()) {
                    assert!((0.0..=W).contains(&m.x) && (0.0..=H).contains(&m.y), "{m:?} at yaw {yaw} pitch {pitch}");
                }
                assert_eq!(d.touches.len(), touches.len());
            }
        }
    }

    #[test]
    fn labels_say_the_sizes() {
        let (touches, mut fit) = block();
        let d = draw(&touches, Some(&fit), View::default(), "mm", 3);
        let text: Vec<&str> = d.labels.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text, ["X 100.000", "Y 50.000"]);
        fit.round = true;
        fit.size = (40.0, 40.0);
        let d = draw(&touches, Some(&fit), View::default(), "mm", 3);
        assert_eq!(d.labels[0].text, "⌀ 40.000 mm");
    }

    #[test]
    fn points_only_while_running() {
        let (touches, _) = block();
        let d = draw(&touches[..2], None, View::default(), "mm", 3);
        assert!(d.face.is_empty() && d.edges.is_empty());
        assert_eq!(d.touches.len(), 2);
        assert!(draw(&[], None, View::default(), "mm", 3).touches.is_empty());
    }

    #[test]
    fn drag_turns_and_keeps_pitch_in_range() {
        let v = View::default().dragged(10.0, 500.0);
        assert_eq!(v.pitch, 90.0);
        assert!((v.yaw - 324.0).abs() < 1e-9);
    }
}
