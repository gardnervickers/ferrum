//! Toolpath preview.
//!
//! The toolpath comes from LinuxCNC's own interpreter: `rs274` reads the
//! program with the machine's INI, tool table and parameters and prints its
//! canonical machining calls, which `canon` turns into segments. So the
//! preview sees subroutines, O-words and parameters as the machine will.
//! `mini` is a small stand-in for Macs without LinuxCNC. Renderers draw a
//! `Toolpath`; `software` works without a GPU (the Pi has only llvmpipe).

pub mod canon;
pub mod mini;
pub mod rs274;
pub mod software;

/// X Y Z A in work coordinates (mm, degrees).
pub type P4 = [f64; 4];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Rapid,
    Feed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub kind: Kind,
    pub from: P4,
    pub to: P4,
    /// Index into `Toolpath::ops`.
    pub op: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Op {
    pub name: String,
    pub tool: i32,
    /// Estimated: feed length over feed rate plus rapids at the rapid rate,
    /// without acceleration, so it reads short on short moves.
    pub seconds: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Toolpath {
    pub segs: Vec<Seg>,
    pub ops: Vec<Op>,
    /// Interpreter errors; the path up to the error is still drawn.
    pub error: Option<String>,
}

impl Toolpath {
    pub fn seconds(&self) -> f64 {
        self.ops.iter().map(|o| o.seconds).sum()
    }

    /// (min, max) over feed moves, or over all moves when there are none.
    pub fn bounds(&self) -> Option<(P4, P4)> {
        let feeds = self.segs.iter().any(|s| s.kind == Kind::Feed);
        let mut it = self.segs.iter().filter(|s| !feeds || s.kind == Kind::Feed).flat_map(|s| [s.from, s.to]);
        let first = it.next()?;
        Some(it.fold((first, first), |(mut lo, mut hi), p| {
            for i in 0..4 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
            (lo, hi)
        }))
    }
}

/// Builds a toolpath from interpreter events, splitting it into ops at
/// program comments (the CAM post names each op in one) and tool changes.
pub struct Builder {
    tp: Toolpath,
    pos: P4,
    tool: i32,
    feed: f64,
    rapid: f64,
    /// Comment waiting to name the next op that moves.
    pending: Option<String>,
    op_has_motion: bool,
    /// The JOB page's `(MYMILL-OP n/N ID=... name)` markers were seen: ops
    /// follow them exactly (manual steps included) and other comments,
    /// such as `(MSG, ...)`, don't name or split ops.
    marked: bool,
}

impl Builder {
    /// `rapid_mm_s`: [TRAJ]MAX_LINEAR_VELOCITY, for time estimates.
    pub fn new(start: P4, rapid_mm_s: f64) -> Self {
        Self {
            tp: Toolpath::default(),
            pos: start,
            tool: 0,
            feed: 0.0,
            rapid: rapid_mm_s.max(1.0) * 60.0,
            pending: None,
            op_has_motion: false,
            marked: false,
        }
    }

    pub fn comment(&mut self, text: &str) {
        let t = text.trim();
        if t.is_empty() || t.starts_with("interpreter:") {
            return;
        }
        if let Some(rest) = t.strip_prefix("MYMILL-OP") {
            // "1/22 ID=A62CA28A FACE 100 X 60 T14" -> "FACE 100 X 60 T14"
            let name = rest.split_whitespace().skip(2).collect::<Vec<_>>().join(" ");
            self.tp.ops.push(Op { name, tool: 0, seconds: 0.0 });
            self.op_has_motion = true;
            self.marked = true;
            self.pending = None;
            return;
        }
        if self.marked || t.starts_with("MYMILL-") || t.starts_with("MSG") {
            return;
        }
        self.pending = Some(t.to_string());
        if self.op_has_motion {
            self.op_has_motion = false;
        }
    }

    pub fn tool(&mut self, t: i32) {
        if self.marked {
            // The marked op's own tool change comes right after its marker.
            if let Some(op) = self.tp.ops.last_mut() {
                if op.tool == 0 {
                    op.tool = t;
                }
            }
            self.tool = t;
            return;
        }
        if t != self.tool && self.op_has_motion {
            self.op_has_motion = false;
        }
        self.tool = t;
    }

    pub fn feed_rate(&mut self, f: f64) {
        self.feed = f;
    }

    fn current_op(&mut self) -> usize {
        if !self.op_has_motion || self.tp.ops.is_empty() {
            let name = self.pending.take().unwrap_or_else(|| format!("OP {}", self.tp.ops.len() + 1));
            self.tp.ops.push(Op { name, tool: self.tool, seconds: 0.0 });
            self.op_has_motion = true;
        }
        self.tp.ops.len() - 1
    }

    pub fn line(&mut self, kind: Kind, to: P4) {
        if to == self.pos {
            return;
        }
        let op = self.current_op();
        let d = dist(self.pos, to);
        let rate = if kind == Kind::Rapid || self.feed <= 0.0 { self.rapid } else { self.feed };
        self.tp.ops[op].seconds += d / rate * 60.0;
        self.tp.segs.push(Seg { kind, from: self.pos, to, op });
        self.pos = to;
    }

    /// Arc in the XY plane (other planes map their axes before calling).
    /// `turns` > 0 is counter-clockwise; |turns| > 1 adds full circles.
    pub fn arc_xy(&mut self, end: P4, center: (f64, f64), turns: i32) {
        let (cx, cy) = center;
        let s = self.pos;
        let r0 = ((s[0] - cx).powi(2) + (s[1] - cy).powi(2)).sqrt();
        let a0 = (s[1] - cy).atan2(s[0] - cx);
        let mut a1 = (end[1] - cy).atan2(end[0] - cx);
        let ccw = turns > 0;
        let tau = std::f64::consts::TAU;
        if ccw {
            while a1 <= a0 + 1e-9 {
                a1 += tau;
            }
            a1 += tau * (turns.abs() - 1) as f64;
        } else {
            while a1 >= a0 - 1e-9 {
                a1 -= tau;
            }
            a1 -= tau * (turns.abs() - 1) as f64;
        }
        let r1 = ((end[0] - cx).powi(2) + (end[1] - cy).powi(2)).sqrt();
        let sweep = a1 - a0;
        // About 1 degree per step, at most 720 steps per arc.
        let n = ((sweep.abs() / tau * 360.0).ceil() as usize).clamp(2, 720);
        for i in 1..=n {
            let t = i as f64 / n as f64;
            let a = a0 + sweep * t;
            let r = r0 + (r1 - r0) * t;
            let p = if i == n {
                end
            } else {
                [cx + r * a.cos(), cy + r * a.sin(), s[2] + (end[2] - s[2]) * t, s[3] + (end[3] - s[3]) * t]
            };
            self.line(Kind::Feed, p);
        }
    }

    pub fn position(&self) -> P4 {
        self.pos
    }

    pub fn set_position(&mut self, p: P4) {
        self.pos = p;
    }

    pub fn error(&mut self, e: String) {
        self.tp.error.get_or_insert(e);
    }

    pub fn finish(self) -> Toolpath {
        self.tp
    }
}

fn dist(a: P4, b: P4) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_split_at_comments_and_tools() {
        let mut b = Builder::new([0.0; 4], 100.0);
        b.comment("T1 D=6 header");
        b.comment("FACE1");
        b.tool(1);
        b.feed_rate(600.0);
        b.line(Kind::Feed, [60.0, 0.0, 0.0, 0.0]);
        b.comment("CONTOUR1");
        b.line(Kind::Rapid, [60.0, 0.0, 10.0, 0.0]);
        b.tool(2);
        b.line(Kind::Feed, [60.0, 10.0, 10.0, 0.0]);
        let tp = b.finish();
        let names: Vec<_> = tp.ops.iter().map(|o| (o.name.as_str(), o.tool)).collect();
        assert_eq!(names, [("FACE1", 1), ("CONTOUR1", 1), ("OP 3", 2)]);
        assert!((tp.ops[0].seconds - 6.0).abs() < 1e-9);
    }

    #[test]
    fn full_circle_arc() {
        let mut b = Builder::new([10.0, 0.0, 0.0, 0.0], 100.0);
        b.feed_rate(100.0);
        b.arc_xy([10.0, 0.0, 0.0, 0.0], (0.0, 0.0), 1);
        let tp = b.finish();
        assert!(tp.segs.len() >= 360);
        let (lo, hi) = tp.bounds().unwrap();
        assert!((lo[0] + 10.0).abs() < 0.01 && (hi[1] - 10.0).abs() < 0.01);
    }
}
