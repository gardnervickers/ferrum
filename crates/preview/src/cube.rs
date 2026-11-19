//! The view cube: a small cube turned with the backplot, drawn by the UI from
//! these outlines. Each face splits 3 × 3 like a CAD view cube: the middle
//! snaps to that face, the strips to the edge views and the corners to the
//! corner (isometric) views.

use crate::software::View;

/// Outlines in a 100 × 100 box, y down, for a Slint `Path`.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub label: &'static str,
    /// The face outline.
    pub path: String,
    /// The lines between the face's middle, edge strips and corners.
    pub grid: String,
    /// Where the label goes (0–100), and whether the face is square-on
    /// enough to carry it.
    pub lx: f64,
    pub ly: f64,
    pub labelled: bool,
    /// How squarely the face looks at the viewer (0–1), for shading.
    pub facing: f64,
    /// The view is exactly this face's.
    pub on: bool,
}

/// Normal axis, sign and label of each face. Front looks along +Y.
const FACES: [(usize, f64, &str); 6] =
    [(1, -1.0, "FRONT"), (1, 1.0, "BACK"), (0, -1.0, "LEFT"), (0, 1.0, "RIGHT"), (2, 1.0, "TOP"), (2, -1.0, "BOTTOM")];

/// Where a strip starts, from the face centre (the cube is ±1).
const SPLIT: f64 = 0.55;

fn screen(v: View, p: [f64; 3]) -> (f64, f64) {
    let (r, u, _) = v.basis();
    let x = r[0] * p[0] + r[1] * p[1] + r[2] * p[2];
    let y = u[0] * p[0] + u[1] * p[1] + u[2] * p[2];
    // ±√3 is the cube's reach in any view.
    let s = 46.0 / 3f64.sqrt();
    (50.0 + x * s, 50.0 - y * s)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// A point on face (axis, sign) at in-face coordinates (a, b), each ±1.
fn on_face(axis: usize, sign: f64, a: f64, b: f64) -> [f64; 3] {
    let mut p = [0.0; 3];
    p[axis] = sign;
    p[(axis + 1) % 3] = a;
    p[(axis + 2) % 3] = b;
    p
}

fn normal(axis: usize, sign: f64) -> [f64; 3] {
    let mut n = [0.0; 3];
    n[axis] = sign;
    n
}

/// The faces turned toward the viewer, in no particular order (a cube's
/// visible faces never overlap).
pub fn faces(v: View) -> Vec<Face> {
    let eye = v.basis().2;
    let mut out = Vec::new();
    for (axis, sign, label) in FACES {
        let facing = dot(normal(axis, sign), eye);
        if facing <= 1e-3 {
            continue;
        }
        let pt = |a: f64, b: f64| screen(v, on_face(axis, sign, a, b));
        let mut path = String::new();
        for (k, (a, b)) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].into_iter().enumerate() {
            let (x, y) = pt(a, b);
            path += &format!("{} {x:.2} {y:.2} ", if k == 0 { "M" } else { "L" });
        }
        path += "Z";
        let mut grid = String::new();
        for c in [-SPLIT, SPLIT] {
            for (p, q) in [(pt(c, -1.0), pt(c, 1.0)), (pt(-1.0, c), pt(1.0, c))] {
                grid += &format!("M {:.2} {:.2} L {:.2} {:.2} ", p.0, p.1, q.0, q.1);
            }
        }
        let (lx, ly) = pt(0.0, 0.0);
        out.push(Face {
            label,
            path,
            grid: grid.trim_end().to_string(),
            lx,
            ly,
            labelled: facing > 0.45,
            facing,
            on: facing > 0.9999,
        });
    }
    out
}

/// The view that looks back along `toward` (from the target to the eye).
pub fn view_toward(toward: [f64; 3]) -> View {
    let len = dot(toward, toward).sqrt();
    let d = toward.map(|c| c / len);
    let pitch = d[2].clamp(-1.0, 1.0).asin().to_degrees().round() as i16;
    // Straight down or up: front at the bottom of the screen.
    let yaw = if d[0].abs() + d[1].abs() < 1e-9 { 0 } else { (-d[0]).atan2(-d[1]).to_degrees().round() as i16 };
    View { yaw, pitch }
}

/// The cube direction under (x, y) in the 100 × 100 box: the face, edge
/// or corner, as a sum of face normals (each component -1, 0 or 1).
fn zone(v: View, x: f64, y: f64) -> Option<[f64; 3]> {
    let eye = v.basis().2;
    for (axis, sign, _) in FACES {
        if dot(normal(axis, sign), eye) <= 1e-3 {
            continue;
        }
        // Solve centre + a·eu + b·ev = (x, y) on screen.
        let c = screen(v, on_face(axis, sign, 0.0, 0.0));
        let ea = screen(v, on_face(axis, sign, 1.0, 0.0));
        let eb = screen(v, on_face(axis, sign, 0.0, 1.0));
        let (ux, uy) = (ea.0 - c.0, ea.1 - c.1);
        let (wx, wy) = (eb.0 - c.0, eb.1 - c.1);
        let det = ux * wy - uy * wx;
        if det.abs() < 1e-9 {
            continue;
        }
        let (px, py) = (x - c.0, y - c.1);
        let a = (px * wy - py * wx) / det;
        let b = (ux * py - uy * px) / det;
        if a.abs() > 1.0 || b.abs() > 1.0 {
            continue;
        }
        let z = |t: f64| if t < -SPLIT { -1.0 } else if t > SPLIT { 1.0 } else { 0.0 };
        return Some(on_face(axis, sign, z(a), z(b)));
    }
    None
}

/// The snap for a tap at (x, y) in the 100 × 100 box, if it hits the cube.
pub fn hit(v: View, x: f64, y: f64) -> Option<View> {
    zone(v, x, y).map(view_toward)
}

/// Outline of everything a tap at (x, y) would pick: a face's middle, an
/// edge's two strips or a corner's three squares, as far as they show.
/// Empty off the cube.
pub fn hot(v: View, x: f64, y: f64) -> String {
    let Some(want) = zone(v, x, y) else { return String::new() };
    let eye = v.basis().2;
    let span = |z: f64| if z < 0.0 { (-1.0, -SPLIT) } else if z > 0.0 { (SPLIT, 1.0) } else { (-SPLIT, SPLIT) };
    let mut path = String::new();
    for (axis, sign, _) in FACES {
        if dot(normal(axis, sign), eye) <= 1e-3 || want[axis] != sign {
            continue;
        }
        // This face's cell for `want`, in its own (a, b) coordinates.
        let (a0, a1) = span(want[(axis + 1) % 3]);
        let (b0, b1) = span(want[(axis + 2) % 3]);
        for (k, (a, b)) in [(a0, b0), (a1, b0), (a1, b1), (a0, b1)].into_iter().enumerate() {
            let (x, y) = screen(v, on_face(axis, sign, a, b));
            path += &format!("{} {x:.2} {y:.2} ", if k == 0 { "M" } else { "L" });
        }
        path += "Z ";
    }
    path.trim_end().to_string()
}

/// `t` (0–1) of the way from `a` to `b`, turning the short way round.
pub fn between(a: View, b: View, t: f64) -> View {
    let mut dy = (b.yaw - a.yaw) as f64;
    if dy > 180.0 {
        dy -= 360.0;
    } else if dy < -180.0 {
        dy += 360.0;
    }
    a.orbited(dy * t, (b.pitch - a.pitch) as f64 * t)
}

/// A name for the view, for the backplot's caption: the faces it looks
/// from when it is one of the cube's snaps (FRONT, TOP RIGHT...), ISO for
/// the classic isometric, else ORBIT.
pub fn name(v: View) -> String {
    if v == View::Iso {
        return "ISO".into();
    }
    let snapped = v.yaw % 45 == 0 && matches!(v.pitch.abs(), 0 | 35 | 45 | 90);
    if !snapped {
        return "ORBIT".into();
    }
    let d = v.basis().2;
    let mut parts = Vec::new();
    for (i, neg, pos) in [(1, "FRONT", "BACK"), (0, "LEFT", "RIGHT"), (2, "BOTTOM", "TOP")] {
        if d[i] < -0.3 {
            parts.push(neg);
        } else if d[i] > 0.3 {
            parts.push(pos);
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faces_and_corners_snap() {
        // Front and top views each show one face, square on.
        for (v, label) in [(View::Front, "FRONT"), (View::Top, "TOP")] {
            let f = faces(v);
            assert_eq!(f.len(), 1, "{label}");
            assert!(f[0].on && f[0].label == label);
        }
        // Iso shows front, left and top.
        let mut l: Vec<_> = faces(View::Iso).iter().map(|f| f.label).collect();
        l.sort();
        assert_eq!(l, ["FRONT", "LEFT", "TOP"]);
        // Tapping the middle of the front face from iso snaps to front.
        let c = screen(View::Iso, [0.0, -1.0, 0.0]);
        assert_eq!(hit(View::Iso, c.0, c.1), Some(View::Front));
        // The front-left-top corner is iso again.
        let c = screen(View::Iso, [-0.9, -1.0, 0.9]);
        assert_eq!(hit(View::Iso, c.0, c.1), Some(View::Iso));
        // Off the cube.
        assert_eq!(hit(View::Front, 1.0, 1.0), None);
        // Left is the view from -X.
        assert_eq!(view_toward([-1.0, 0.0, 0.0]), View { yaw: 90, pitch: 0 });
        assert_eq!(name(view_toward([-1.0, 0.0, 0.0])), "LEFT");
        assert_eq!(name(view_toward([0.0, 0.0, -1.0])), "BOTTOM");
        assert_eq!(name(view_toward([0.0, -1.0, 1.0])), "FRONT TOP");
        assert_eq!(name(view_toward([1.0, -1.0, 1.0])), "FRONT RIGHT TOP");
        assert_eq!(name(View { yaw: 10, pitch: 20 }), "ORBIT");
        // The iso corner lights a square on each of the three faces.
        let c = screen(View::Iso, [-0.9, -1.0, 0.9]);
        assert_eq!(hot(View::Iso, c.0, c.1).matches('Z').count(), 3);
        // A face middle lights one cell; off the cube lights nothing.
        let c = screen(View::Iso, [0.0, -1.0, 0.0]);
        assert_eq!(hot(View::Iso, c.0, c.1).matches('Z').count(), 1);
        assert_eq!(hot(View::Front, 1.0, 1.0), "");
        // Turning goes the short way across ±180.
        let a = View { yaw: 170, pitch: 0 };
        let b = View { yaw: -170, pitch: 0 };
        assert_eq!(between(a, b, 0.5), View { yaw: -180, pitch: 0 });
        assert_eq!(between(a, b, 1.0), b);
    }
}
