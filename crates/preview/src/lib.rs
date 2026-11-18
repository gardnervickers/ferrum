//! Toolpath preview.
//!
//! The toolpath comes from LinuxCNC's own interpreter: `rs274` reads the
//! program with the machine's INI, tool table and parameters and prints its
//! canonical machining calls, which `canon` turns into segments. So the
//! preview sees subroutines, O-words and parameters as the machine will.
//! `mini` is a small stand-in for Macs without LinuxCNC. Renderers draw a
//! `Toolpath`; `software` works without a GPU (the Pi has only llvmpipe).

pub mod canon;
pub mod cube;
pub mod machine;
pub mod mini;
pub mod preflight;
pub mod rs274;
pub mod raster;
pub mod software;
pub use tiny_skia::Pixmap;
#[cfg(feature = "gpu")]
pub mod gpu;

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
    /// Source line (1-based; 0 when the reader doesn't know it).
    pub line: u32,
    /// Programmed rate in mm/min (the rapid rate for rapids).
    pub rate: f32,
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
    /// The box to frame when there are no moves (no program loaded): the
    /// table and the travel above it, so the view still shows the machine.
    pub frame: Option<(P4, P4)>,
    /// The offsets the interpreter applied, each from `segs[n]` on (the
    /// first entry from 0). Canon positions are work positions; these turn
    /// them into machine positions (see `preflight`).
    pub offsets: Vec<(usize, Offsets)>,
    /// Source lines (sorted) whose moves are in machine coordinates (G53).
    pub machine_lines: Vec<u32>,
    /// Source lines (sorted) where a job's probing step starts that finds
    /// a work offset (`(MYMILL-PROBE FIND ...)`): what follows runs with
    /// an offset nobody knows yet.
    pub find_lines: Vec<u32>,
    /// The first source line with an X, Y and Z word (0: none), when the
    /// source was read (`axis_lines`). Until a program moves an axis, its
    /// position is wherever the machine happens to be, not the preview's
    /// arbitrary start (see `preflight`).
    pub axis_lines: Option<[u32; 3]>,
    /// How A is drawn (see `wrap_a`): 0 ignores it; 1 or -1 wraps the path
    /// around the rotary axis (X, through work Y0 Z0) in that sense.
    pub a_wrap: f64,
    /// `drawn_bounds` when wrapped, worked out once by `set_rotary`.
    pub wrap_bounds: Option<([f64; 3], [f64; 3])>,
}

/// A point of the part on the rotary (about X, through work Y0 Z0), from
/// where the tool is (work X Y Z) and the angle A. `sign` is the rotary's
/// sense ([DISPLAY]GEOMETRY "A" or "-A"); 0 leaves the point as it is.
/// Drawing the toolpath through this shows it wrapped around the part,
/// as it is cut, instead of every pass flat on top of the last.
pub fn wrap_a(p: P4, sign: f64) -> [f64; 3] {
    if sign == 0.0 || p[3] == 0.0 {
        return [p[0], p[1], p[2]];
    }
    let (s, c) = (sign * p[3]).to_radians().sin_cos();
    [p[0], p[1] * c - p[2] * s, p[1] * s + p[2] * c]
}

/// The rotary's sense from [DISPLAY]GEOMETRY ("XYZA", "-A", ...): 1, -1,
/// or 0 when A is not part of the geometry (as LinuxCNC's AXIS reads it).
pub fn geometry_a_sign(geometry: &str) -> f64 {
    let g = geometry.to_ascii_uppercase();
    match g.find('A') {
        Some(i) if g[..i].ends_with('-') => -1.0,
        Some(_) => 1.0,
        None => 0.0,
    }
}

/// The 1-based numbers of the lines that start a FIND probing step.
pub fn find_lines(text: &str) -> Vec<u32> {
    text.lines().enumerate().filter(|(_, l)| l.trim_start().starts_with("(MYMILL-PROBE FIND")).map(|(i, _)| i as u32 + 1).collect()
}

/// The first 1-based line with an X, Y and Z word (comments and `o<name>`
/// aside); 0 when there is none.
pub fn axis_lines(text: &str) -> [u32; 3] {
    let mut first = [0u32; 3];
    for (i, l) in text.lines().enumerate() {
        let mut code = String::new();
        let (mut depth, mut name) = (0, false);
        for c in l.chars() {
            match c {
                ';' if depth == 0 => break,
                '(' => depth += 1,
                ')' => depth = (depth - 1).max(0),
                '<' if depth == 0 => name = true,
                '>' if depth == 0 => name = false,
                c if depth == 0 && !name && !c.is_whitespace() => code.push(c.to_ascii_uppercase()),
                _ => {}
            }
        }
        let b = code.as_bytes();
        for (a, letter) in [b'X', b'Y', b'Z'].iter().enumerate() {
            let has = (0..b.len()).any(|k| b[k] == *letter && b.get(k + 1).is_some_and(|n| n.is_ascii_digit() || b"+-.#[".contains(n)));
            if has && first[a] == 0 {
                first[a] = i as u32 + 1;
            }
        }
    }
    first
}

/// The 1-based numbers of the lines in `text` that carry a G53 word
/// (comments aside).
pub fn machine_lines(text: &str) -> Vec<u32> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let mut code = String::new();
            let mut depth = 0;
            for c in l.chars() {
                match c {
                    ';' if depth == 0 => break,
                    '(' => depth += 1,
                    ')' => depth = (depth - 1).max(0),
                    c if depth == 0 && !c.is_whitespace() => code.push(c.to_ascii_uppercase()),
                    _ => {}
                }
            }
            // "G53" not followed by another digit or a decimal (G53.1 is not a word, but be strict).
            code.match_indices("G53").any(|(i, _)| !code[i + 3..].starts_with(|c: char| c.is_ascii_digit() || c == '.'))
        })
        .map(|(i, _)| i as u32 + 1)
        .collect()
}

/// The offsets in effect for a stretch of moves, as the interpreter
/// reported them (from its parameter file and the program's own G10/G92).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Offsets {
    /// Work system, 1 (G54) to 9 (G59.3); 0 before the interpreter named one.
    pub wcs: u8,
    pub g5x: [f64; 3],
    pub g92: [f64; 3],
    /// Tool length offset (G43): X Y Z.
    pub tool: [f64; 3],
    /// XY rotation of the work system, degrees.
    pub rotation: f64,
}

impl Toolpath {
    /// Wrap the drawing around the rotary (`sign` from `geometry_a_sign`)
    /// when the program turns A or works at an angle; a program that
    /// leaves A at 0 draws as it always has.
    pub fn set_rotary(&mut self, sign: f64) {
        let uses_a = self.segs.iter().any(|s| s.from[3] != 0.0 || s.to[3] != 0.0);
        self.a_wrap = if uses_a { sign } else { 0.0 };
        self.wrap_bounds = None;
        if self.a_wrap != 0.0 {
            self.wrap_bounds = self.wrapped_bounds();
        }
    }

    /// A point as drawn (wrapped when `a_wrap` says so).
    pub fn drawn(&self, p: P4) -> [f64; 3] {
        wrap_a(p, self.a_wrap)
    }

    /// (min, max) of the drawing: as `bounds`, after wrapping.
    pub fn drawn_bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        if self.a_wrap == 0.0 || self.segs.is_empty() {
            return self.bounds().map(|(a, b)| ([a[0], a[1], a[2]], [b[0], b[1], b[2]]));
        }
        self.wrap_bounds.or_else(|| self.wrapped_bounds())
    }

    fn wrapped_bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let feeds = self.segs.iter().any(|s| s.kind == Kind::Feed);
        let mut it = self.segs.iter().filter(|s| !feeds || s.kind == Kind::Feed).flat_map(|s| [self.drawn(s.from), self.drawn(s.to)]);
        let first = it.next()?;
        Some(it.fold((first, first), |(mut lo, mut hi), p| {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
            (lo, hi)
        }))
    }

    pub fn seconds(&self) -> f64 {
        self.ops.iter().map(|o| o.seconds).sum()
    }

    /// (min, max) over feed moves, or over all moves when there are none,
    /// or `frame` when there are no moves at all.
    pub fn bounds(&self) -> Option<(P4, P4)> {
        if self.segs.is_empty() {
            return self.frame;
        }
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
    /// The current op has cut (fed) already. Before that, a tool change
    /// belongs to the op: CAM posts name the op, then retract (G53 G0 Z0)
    /// and stop the spindle, and only then call T.. M6.
    op_cut: bool,
    /// The JOB page's `(MYMILL-OP n/N ID=... name)` markers were seen: ops
    /// follow them exactly (manual steps included) and other comments,
    /// such as `(MSG, ...)`, don't name or split ops.
    marked: bool,
    line: u32,
    offsets: Offsets,
    /// Rapid rate of A in degrees/min, for moves that only turn A.
    rapid_a: f64,
}

/// Longest turn of A drawn as one straight piece: wrapped, a move that
/// turns A is a helix around the part, so it is cut into short chords.
const A_STEP_DEG: f64 = 2.0;

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
            op_cut: false,
            marked: false,
            line: 0,
            offsets: Offsets::default(),
            rapid_a: 180.0 * 60.0,
        }
    }

    /// [AXIS_A]MAX_VELOCITY (degrees/s), for time estimates.
    pub fn set_rotary_rapid(&mut self, deg_s: f64) {
        self.rapid_a = deg_s.max(1.0) * 60.0;
    }

    /// The offsets now in effect (each change is recorded once).
    pub fn offsets(&self) -> Offsets {
        self.offsets
    }

    pub fn set_offsets(&mut self, o: Offsets) {
        if o == self.offsets && !self.tp.offsets.is_empty() {
            return;
        }
        self.offsets = o;
        let at = self.tp.segs.len();
        match self.tp.offsets.last_mut() {
            Some(last) if last.0 == at => last.1 = o,
            _ => self.tp.offsets.push((at, o)),
        }
    }

    /// The source line the following moves come from.
    pub fn set_line(&mut self, n: u32) {
        self.line = n;
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
            match self.tp.ops.last_mut() {
                Some(op) if !self.op_cut => op.tool = t,
                _ => self.op_has_motion = false,
            }
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
            self.op_cut = false;
        }
        self.tp.ops.len() - 1
    }

    pub fn line(&mut self, kind: Kind, to: P4) {
        if to == self.pos {
            return;
        }
        let op = self.current_op();
        if kind != Kind::Rapid {
            self.op_cut = true;
        }
        // The feed is along X Y Z; a move that only turns A is fed in
        // degrees/min (as LinuxCNC does, also for G93's inverse time). A
        // turns no faster than its rapid rate either way.
        let d = dist(self.pos, to);
        let turn = (to[3] - self.pos[3]).abs();
        let (d, rapid) = if d > 1e-9 { (d, self.rapid) } else { (turn, self.rapid_a) };
        let rate = if kind == Kind::Rapid || self.feed <= 0.0 { rapid } else { self.feed };
        self.tp.ops[op].seconds += (d / rate).max(turn / self.rapid_a) * 60.0;
        let from = self.pos;
        let n = ((turn / A_STEP_DEG).ceil() as usize).clamp(1, 3600);
        for i in 1..=n {
            let p = if i == n { to } else { std::array::from_fn(|k| from[k] + (to[k] - from[k]) * i as f64 / n as f64) };
            self.tp.segs.push(Seg { kind, from: self.pos, to: p, op, line: self.line, rate: rate as f32 });
            self.pos = p;
        }
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
        // CONTOUR1 has not cut when T2 is loaded, so T2 is its tool.
        assert_eq!(names, [("FACE1", 1), ("CONTOUR1", 2)]);
        assert!((tp.ops[0].seconds - 6.0).abs() < 1e-9);
    }

    #[test]
    fn op_named_before_its_retract_and_tool_change_keeps_the_new_tool() {
        // Fusion: (DRILL1) M9 M5 G53 G0 Z0 T5 M6 ... after a T1 op.
        let mut b = Builder::new([0.0; 4], 100.0);
        b.comment("FACE1");
        b.tool(1);
        b.feed_rate(600.0);
        b.line(Kind::Feed, [60.0, 0.0, 0.0, 0.0]);
        b.comment("DRILL1");
        b.line(Kind::Rapid, [60.0, 0.0, 50.0, 0.0]);
        b.tool(5);
        b.line(Kind::Rapid, [10.0, 10.0, 50.0, 0.0]);
        b.line(Kind::Feed, [10.0, 10.0, -3.0, 0.0]);
        let tp = b.finish();
        let names: Vec<_> = tp.ops.iter().map(|o| (o.name.as_str(), o.tool)).collect();
        assert_eq!(names, [("FACE1", 1), ("DRILL1", 5)]);
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

#[cfg(test)]
mod rotary_tests {
    use super::*;

    #[test]
    fn geometry_sign() {
        assert_eq!(geometry_a_sign("xyza"), 1.0);
        assert_eq!(geometry_a_sign("XYZ-A"), -1.0);
        assert_eq!(geometry_a_sign("XYZ"), 0.0);
    }

    #[test]
    fn wrap_turns_the_top_of_the_part_toward_minus_y_as_a_grows() {
        // Tool over the axis at Z 10: at A 90 it is on the part's -Y side.
        let q = wrap_a([5.0, 0.0, 10.0, 90.0], 1.0);
        assert!((q[0] - 5.0).abs() < 1e-9 && (q[1] + 10.0).abs() < 1e-9 && q[2].abs() < 1e-9);
        assert_eq!(wrap_a([5.0, 1.0, 10.0, 90.0], 0.0), [5.0, 1.0, 10.0]);
    }

    #[test]
    fn a_turn_is_cut_into_chords_and_timed_in_degrees() {
        let mut b = Builder::new([0.0, 0.0, 10.0, 0.0], 100.0);
        b.feed_rate(360.0);
        // A full turn of A alone at F360 (degrees/min): one minute.
        b.line(Kind::Feed, [0.0, 0.0, 10.0, 360.0]);
        let mut tp = b.finish();
        assert_eq!(tp.segs.len(), 180);
        assert!((tp.seconds() - 60.0).abs() < 1e-6);
        assert!(tp.segs.iter().all(|s| s.line == 0 && s.op == 0));
        // Ten turns along 60 mm at F900: X takes 4 s, but A at 180 deg/s takes 20 s.
        let mut b = Builder::new([0.0; 4], 100.0);
        b.feed_rate(900.0);
        b.line(Kind::Feed, [60.0, 0.0, 0.0, 3600.0]);
        assert!((b.finish().seconds() - 20.0).abs() < 1e-6);
        tp.set_rotary(1.0);
        let (lo, hi) = tp.drawn_bounds().unwrap();
        assert!((lo[1] + 10.0).abs() < 0.01 && (hi[1] - 10.0).abs() < 0.01 && (lo[2] + 10.0).abs() < 0.01);
    }

    #[test]
    fn a_program_that_leaves_a_alone_does_not_wrap() {
        let mut b = Builder::new([0.0; 4], 100.0);
        b.feed_rate(100.0);
        b.line(Kind::Feed, [10.0, 0.0, 0.0, 0.0]);
        let mut tp = b.finish();
        tp.set_rotary(1.0);
        assert_eq!(tp.a_wrap, 0.0);
    }
}
