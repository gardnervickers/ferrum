//! Run-time estimate that follows LinuxCNC 2.9's trajectory planner, and
//! where a running program is along it.
//!
//! The planner (src/emc/tp) treats each programmed move as one segment with
//! a top speed and an acceleration, both limited per axis along its
//! direction ([AXIS_n]MAX_VELOCITY, MAX_ACCELERATION), and blends corners
//! with a small arc whose radius comes from the G64 tolerance and the
//! lengths of the two moves (`blendComputeParameters`). A command that is
//! not a move (tool change, spindle, coolant, M-code, message, dwell) waits
//! for motion to stop. This module rebuilds that: a speed limit for every
//! corner, a forward and backward pass so each move speeds up and slows
//! down within its acceleration, then the time of each trapezoid.
//!
//! Not modelled: the blend arcs' shortcut of the path (a little shorter),
//! kink handling of nearly tangent corners, waits for a person (tool
//! changes, M0) and spindle spin-up. Overrides are applied by the caller
//! (`Tracker::left`).

use crate::{Kind, Seg, Toolpath, P4};

/// The machine's limits, in machine units (A in degrees) per second.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// [AXIS_X/Y/Z/A]MAX_VELOCITY; 0 for an axis the machine lacks.
    pub vel: [f64; 4],
    /// [AXIS_X/Y/Z/A]MAX_ACCELERATION.
    pub acc: [f64; 4],
    /// [TRAJ]MAX_LINEAR_VELOCITY: also the rapid rate (0: none).
    pub max_linear_vel: f64,
    /// [TRAJ]MAX_LINEAR_ACCELERATION (0: none).
    pub max_linear_acc: f64,
    /// [EMCMOT]SERVO_PERIOD in seconds: a move lasts at least one period.
    pub servo_period: f64,
    /// [TRAJ]ARC_BLEND_OPTIMIZATION_DEPTH: how many moves ahead the planner
    /// looks to slow down in time.
    pub depth: usize,
    /// Seconds a command to the I/O controller holds the program up (about
    /// [EMCIO]CYCLE_TIME; 120 ms measured in the sim with 100 ms).
    pub io_wait: f64,
    /// Seconds lost each time motion comes to a stop between moves (9 ms
    /// measured in the sim).
    pub stop_overhead: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { vel: [100.0, 100.0, 100.0, 180.0], acc: [1000.0, 1000.0, 1000.0, 1000.0], max_linear_vel: 0.0, max_linear_acc: 0.0, servo_period: 0.001, depth: 50, io_wait: 0.12, stop_overhead: 0.009 }
    }
}

/// The share of acceleration a blend arc may use sideways
/// (`BLEND_ACC_RATIO_NORMAL`), and an arc along its path
/// (`BLEND_ACC_RATIO_TANGENTIAL`).
const ACC_NORMAL: f64 = 0.866_025_403_784_438_6;
const ACC_TANGENTIAL: f64 = 0.5;
/// A join with an arc this close to straight is taken as tangent (the
/// chords this crate cuts arcs into are off the true tangent by half a
/// chord's angle).
const ARC_TANGENT_DEG: f64 = 1.5;
/// [TRAJ]ARC_BLEND_TANGENT_KINK_RATIO's default: a join between lines is
/// run through without a blend arc when the jerk of turning it in one
/// servo period stays under this share of the acceleration.
const KINK_RATIO: f64 = 0.1;

/// One planner segment.
#[derive(Clone, Copy, Debug, Default)]
struct Plan {
    first: usize,
    end: usize,
    /// Path length (degrees for a move that only turns A).
    len: f64,
    /// Highest speed along the path, and acceleration along it.
    vmax: f64,
    acc: f64,
    /// The acceleration of the axes it moves (for blend arcs).
    acc_plane: f64,
    /// Speed it may still have at its end (the corner into the next move).
    v_end: f64,
    rotary_only: bool,
    arc: bool,
    /// Ends of the move that get a blend arc (0 to 2).
    blended: u8,
    blend: f64,
    stop_before: bool,
    dwell_before: f64,
    io_waits: u16,
}

fn sub(a: P4, b: P4) -> P4 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]]
}

fn xyz_len(d: P4) -> f64 {
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// Unit direction of a piece in XYZ (or A alone), None when it has no length.
fn dir(s: &Seg) -> Option<[f64; 4]> {
    let d = sub(s.to, s.from);
    let l = xyz_len(d);
    if l > 1e-9 {
        Some([d[0] / l, d[1] / l, d[2] / l, 0.0])
    } else if d[3].abs() > 1e-9 {
        Some([0.0, 0.0, 0.0, d[3].signum()])
    } else {
        None
    }
}

/// Limit along `per_unit` (axis travel per unit of path): the smallest
/// axis limit over its share of the motion.
fn along(per_unit: [f64; 4], lim: [f64; 4]) -> f64 {
    let mut v = f64::INFINITY;
    for i in 0..4 {
        if per_unit[i].abs() > 1e-9 && lim[i] > 0.0 {
            v = v.min(lim[i] / per_unit[i].abs());
        }
    }
    v
}

/// Fill `tp.t_end`, `tp.t_rapid_end` and every op's `seconds`.
pub fn estimate(tp: &mut Toolpath, lim: &Limits) {
    let n = tp.segs.len();
    tp.t_end = vec![0.0; n];
    tp.t_rapid_end = vec![0.0; n];
    if n == 0 || tp.moves.is_empty() {
        return;
    }
    let mut plans = plan(tp, lim);
    corners(tp, lim, &mut plans);
    // Every planner segment lasts at least a servo period
    // (TP_MIN_SEGMENT_CYCLES): a move, and each blend arc (shared with
    // the neighbour) at its ends. Short moves are held back by this.
    if lim.servo_period > 0.0 {
        for p in plans.iter_mut() {
            let cycles = 1.02 * (1.0 + 0.5 * p.blended as f64);
            p.vmax = p.vmax.min(p.len / (lim.servo_period * cycles));
            p.v_end = p.v_end.min(p.vmax);
        }
        for k in 1..plans.len() {
            let v = plans[k].vmax;
            plans[k - 1].v_end = plans[k - 1].v_end.min(v);
        }
    }
    passes(lim, &mut plans);

    // Time each piece along its move's speed profile.
    let (mut t, mut t_rapid) = (0.0f64, 0.0f64);
    let mut op_secs = vec![0.0f64; tp.ops.len()];
    let mut op_rapid = vec![0.0f64; tp.ops.len()];
    let mut v_in = 0.0;
    for p in &plans {
        if p.stop_before {
            v_in = 0.0;
        }
        let rapid = tp.segs[p.first].kind == Kind::Rapid;
        // Waits before the move: dwell, I/O, and the time a stop costs.
        let mut wait = p.dwell_before + p.io_waits as f64 * lim.io_wait;
        if v_in == 0.0 && t > 0.0 {
            wait += lim.stop_overhead;
        }
        if wait > 0.0 {
            t += wait;
            if let Some(o) = op_secs.get_mut(tp.segs[p.first].op) {
                *o += wait;
            }
        }
        let prof = Profile::new(p.len, v_in, p.v_end, p.vmax, p.acc);
        let (t0, tr0) = (t, t_rapid);
        let mut s = 0.0;
        for i in p.first..p.end {
            let seg = &tp.segs[i];
            let d = sub(seg.to, seg.from);
            s += if p.rotary_only { d[3].abs() } else { xyz_len(d) };
            let at = prof.time_at(s.min(p.len));
            let te = t0 + at;
            let dt = te - t;
            if let Some(o) = op_secs.get_mut(seg.op) {
                *o += dt;
                if rapid {
                    op_rapid[seg.op] += dt;
                }
            }
            t = te;
            if rapid {
                t_rapid = tr0 + at;
            }
            tp.t_end[i] = t as f32;
            tp.t_rapid_end[i] = t_rapid as f32;
        }
        v_in = p.v_end;
    }
    for ((o, s), r) in tp.ops.iter_mut().zip(op_secs).zip(op_rapid) {
        o.seconds = s;
        o.rapid_seconds = r;
    }
}

/// Each move's length and limits.
fn plan(tp: &Toolpath, lim: &Limits) -> Vec<Plan> {
    let n = tp.segs.len();
    let mut plans = Vec::with_capacity(tp.moves.len());
    for (k, m) in tp.moves.iter().enumerate() {
        let end = tp.moves.get(k + 1).map_or(n, |m| m.start).min(n);
        if m.start >= end {
            continue;
        }
        let segs = &tp.segs[m.start..end];
        let (first, last) = (segs[0].from, segs[segs.len() - 1].to);
        let len_xyz: f64 = segs.iter().map(|s| xyz_len(sub(s.to, s.from))).sum();
        let turn: f64 = segs.iter().map(|s| (s.to[3] - s.from[3]).abs()).sum();
        let rotary_only = len_xyz < 1e-9;
        let len = if rotary_only { turn } else { len_xyz };
        if len < 1e-12 {
            continue;
        }
        // Axis travel per unit of path: straight from the end points for a
        // line; for an arc, any axis it moves counts fully (the direction
        // turns through the plane).
        let per_unit: [f64; 4] = if m.radius > 0.0 {
            let mut lo = segs[0].from;
            let mut hi = lo;
            for s in segs {
                for i in 0..4 {
                    lo[i] = lo[i].min(s.to[i]);
                    hi[i] = hi[i].max(s.to[i]);
                }
            }
            std::array::from_fn(|i| if i == 3 { turn / len } else if hi[i] - lo[i] > 1e-9 { 1.0 } else { 0.0 })
        } else {
            let d = sub(last, first);
            std::array::from_fn(|i| if rotary_only { if i == 3 { 1.0 } else { 0.0 } } else { d[i] / len })
        };
        let mut vmax = along(per_unit, lim.vel);
        let mut acc = along(per_unit, lim.acc);
        if !rotary_only {
            if lim.max_linear_vel > 0.0 {
                vmax = vmax.min(lim.max_linear_vel);
            }
            if lim.max_linear_acc > 0.0 {
                acc = acc.min(lim.max_linear_acc);
            }
        }
        let acc_plane = acc;
        let rate = segs[0].rate as f64 / 60.0;
        let mut v = if segs[0].kind == Kind::Rapid || rate <= 0.0 { vmax } else { rate.min(vmax) };
        if m.radius > 0.0 {
            // Sideways acceleration on the arc, and the share left along it.
            v = v.min((acc * ACC_NORMAL * m.radius).sqrt());
            acc *= ACC_TANGENTIAL;
        }
        plans.push(Plan {
            first: m.start,
            end,
            len,
            vmax: v.max(1e-6),
            acc: if acc.is_finite() { acc.max(1e-6) } else { 1e6 },
            acc_plane: if acc_plane.is_finite() { acc_plane } else { 1e6 },
            v_end: 0.0,
            rotary_only,
            arc: m.radius > 0.0,
            blended: 0,
            blend: m.blend,
            stop_before: m.stop_before || m.dwell_before > 0.0 || m.io_waits > 0,
            dwell_before: m.dwell_before,
            io_waits: m.io_waits,
        });
    }
    // Moves dropped for having no length keep their stops for the next.
    plans
}

/// The speed each move may keep into the next: 0 at a stop, the slower
/// top speed through a tangent join, the blend arc's speed at a corner.
fn corners(tp: &Toolpath, lim: &Limits, plans: &mut [Plan]) {
    for k in 0..plans.len() {
        let Some(next) = plans.get(k + 1).copied() else {
            plans[k].v_end = 0.0;
            break;
        };
        let p = plans[k];
        let (s1, s2) = (&tp.segs[p.end - 1], &tp.segs[next.first]);
        // No blending across a stop, in exact path or stop mode, or
        // between a rapid and a feed (tp.c handleModeChange).
        if next.stop_before || p.blend < -1.5 || p.rotary_only != next.rotary_only || s1.kind != s2.kind {
            plans[k].v_end = 0.0;
            continue;
        }
        let (Some(u1), Some(u2)) = (dir(s1), dir(s2)) else {
            plans[k].v_end = 0.0;
            continue;
        };
        let cos = (0..4).map(|i| u1[i] * u2[i]).sum::<f64>().clamp(-1.0, 1.0);
        let deflection = cos.acos();
        let v_through = p.vmax.min(next.vmax);
        let arc = p.arc || next.arc;
        // tpSetupTangent: nearly tangent joins run straight through.
        let tangent = if arc {
            deflection.to_degrees() < ARC_TANGENT_DEG
        } else if lim.servo_period > 0.0 {
            let a_inst = v_through / lim.servo_period + next.acc;
            (0..3).filter(|&i| lim.acc[i] > 0.0).map(|i| (a_inst * (u2[i] - u1[i])).abs() / lim.acc[i]).fold(0.0, f64::max) < KINK_RATIO
        } else {
            deflection < 1e-6
        };
        if tangent {
            plans[k].v_end = v_through;
            continue;
        }
        // Exact path: no blend arcs, so a corner is a stop.
        if p.blend < 0.0 {
            plans[k].v_end = 0.0;
            continue;
        }
        // blendComputeParameters: theta is half the angle between the
        // moves; the arc may use a quarter of either move (or the G64
        // tolerance) and at most half of it.
        let theta = (std::f64::consts::PI - deflection) / 2.0;
        if theta < 1e-3 {
            plans[k].v_end = 0.0;
            continue;
        }
        let tol = |q: &Plan| if q.blend > 0.0 { q.blend } else { q.len * 0.25 };
        let t_blend = tol(&p).min(tol(&next)).min(p.len * 0.25).min(next.len * 0.25);
        let h_tol = t_blend / (1.0 - theta.sin());
        let d_tol = theta.cos() * h_tol;
        let d_geom = (p.len / 2.0).min(next.len / 2.0).min(d_tol);
        let r = theta.tan() * d_geom;
        let a_n = p.acc_plane.min(next.acc_plane) * ACC_NORMAL;
        plans[k].v_end = (a_n * r).sqrt().min(v_through);
        plans[k].blended += 1;
        plans[k + 1].blended += 1;
    }
}

/// Backward then forward: no move may need to slow down (or speed up)
/// faster than its acceleration allows, and the planner looks only
/// `depth` moves ahead.
fn passes(lim: &Limits, plans: &mut [Plan]) {
    let n = plans.len();
    if n == 0 {
        return;
    }
    // Backward: the end speed of move k must let move k+1 slow down to its own end speed.
    for k in (0..n.saturating_sub(1)).rev() {
        let nx = plans[k + 1];
        let reach = (nx.v_end * nx.v_end + 2.0 * nx.acc * nx.len).sqrt();
        plans[k].v_end = plans[k].v_end.min(reach);
    }
    // Lookahead depth: what lies past `depth` moves is not planned yet, so
    // the planner must be able to stop within the moves it has.
    if lim.depth > 0 && n > lim.depth {
        let mut window: f64 = 0.0;
        let mut acc_min = f64::INFINITY;
        // Sum of lengths of moves k+1..=k+depth, from the back.
        let lens: Vec<f64> = plans.iter().map(|p| p.len).collect();
        let mut prefix = vec![0.0; n + 1];
        for i in 0..n {
            prefix[i + 1] = prefix[i] + lens[i];
        }
        for k in 0..n {
            let hi = (k + 1 + lim.depth).min(n);
            window = prefix[hi] - prefix[(k + 1).min(n)];
            acc_min = plans[(k + 1).min(n - 1)].acc;
            if hi < n {
                plans[k].v_end = plans[k].v_end.min((2.0 * acc_min * window).sqrt());
            }
        }
        let _ = (window, acc_min);
    }
    // Forward: move k cannot end faster than it can speed up from its start.
    let mut v_in = 0.0;
    for p in plans.iter_mut() {
        if p.stop_before {
            v_in = 0.0;
        }
        let reach = (v_in * v_in + 2.0 * p.acc * p.len).sqrt();
        p.v_end = p.v_end.min(reach).min(p.vmax);
        v_in = p.v_end;
    }
}

/// A move's speed over its length: speed up from `v0`, cruise, slow down to `v1`.
struct Profile {
    v0: f64,
    vp: f64,
    acc: f64,
    /// Lengths of the speed-up and cruise parts.
    d1: f64,
    d2: f64,
    /// Times of the speed-up and cruise parts.
    t1: f64,
    t2: f64,
}

impl Profile {
    fn new(len: f64, v0: f64, v1: f64, vmax: f64, acc: f64) -> Self {
        let v0 = v0.min(vmax);
        let v1 = v1.min(vmax);
        let vp = vmax.min((acc * len + (v0 * v0 + v1 * v1) / 2.0).sqrt()).max(v0.max(v1));
        let d1 = ((vp * vp - v0 * v0) / (2.0 * acc)).max(0.0);
        let d3 = ((vp * vp - v1 * v1) / (2.0 * acc)).max(0.0);
        let d2 = (len - d1 - d3).max(0.0);
        Profile { v0, vp, acc, d1, d2, t1: (vp - v0) / acc, t2: if vp > 0.0 { d2 / vp } else { 0.0 } }
    }

    /// Seconds from the start of the move to `s` along it.
    fn time_at(&self, s: f64) -> f64 {
        let Profile { v0, vp, acc, d1, d2, t1, t2 } = *self;
        if s <= d1 {
            ((v0 * v0 + 2.0 * acc * s).sqrt() - v0) / acc
        } else if s <= d1 + d2 {
            t1 + (s - d1) / vp.max(1e-9)
        } else {
            let s3 = s - d1 - d2;
            t1 + t2 + (vp - (vp * vp - 2.0 * acc * s3).max(0.0).sqrt()) / acc
        }
    }
}

/// Where a running program is along its toolpath, from the line LinuxCNC is
/// executing and the tool's position (work coordinates, as the toolpath).
#[derive(Clone, Debug, Default)]
pub struct Tracker {
    /// The piece the tool was last found on.
    pub cursor: usize,
    /// Seconds from the start to the tool, and of those in rapids.
    pub at: f64,
    pub at_rapid: f64,
    /// A line looked for through the whole toolpath and not found (a
    /// subroutine's own line): not looked for again until it changes.
    missed: u32,
}

/// How far ahead (in pieces) to look for the tool.
const WINDOW: usize = 4000;

impl Tracker {
    pub fn reset(&mut self) {
        *self = Tracker::default();
    }

    /// Follow the tool: `line` is the source line motion is on (0 if
    /// unknown), `pos` where the tool is, `near` how close counts as on the
    /// path (blends cut corners). Only moves forward, so a path that comes
    /// back over itself is not mistaken for an earlier pass.
    pub fn update(&mut self, tp: &Toolpath, line: u32, pos: P4, near: f64) {
        let n = tp.segs.len();
        if n == 0 || tp.t_end.len() != n {
            return;
        }
        let from = self.cursor.min(n - 1);
        let to = (from + WINDOW).min(n);
        let on = |i: usize| -> Option<f64> {
            let s = &tp.segs[i];
            let (d, f) = closest(s, pos);
            (d <= near).then_some(f)
        };
        // The first piece ahead on the right line and under the tool; then
        // any piece under the tool close ahead (a subroutine's lines are
        // not the program's); then the first piece of the line, wherever
        // it is (run from a line, a jump the tracker missed).
        let found = (from..to)
            .find_map(|i| (line > 0 && tp.segs[i].line == line).then(|| on(i).map(|f| (i, f))).flatten())
            .or_else(|| (from..(from + 200).min(n)).find_map(|i| on(i).map(|f| (i, f))))
            .or_else(|| {
                if line == 0 || tp.segs[from].line == line || self.missed == line {
                    return None;
                }
                let first = |r: std::ops::Range<usize>| r.into_iter().find(|&i| tp.segs[i].line == line);
                let hit = first(from..n).or_else(|| first(0..from)).map(|i| (i, 0.0));
                if hit.is_none() {
                    self.missed = line;
                }
                hit
            });
        let Some((i, f)) = found else { return };
        self.cursor = i;
        let (t0, t1) = (tp.t_start(i), tp.t_end[i] as f64);
        self.at = t0 + (t1 - t0) * f;
        let r0 = if i == 0 { 0.0 } else { tp.t_rapid_end[i - 1] as f64 };
        let r1 = tp.t_rapid_end[i] as f64;
        self.at_rapid = r0 + (r1 - r0) * f;
    }

    /// Seconds left in the op the tool is in and in the whole program,
    /// with the feed and rapid overrides (1.0 = 100 %); ops marked in
    /// `skip` are not run.
    pub fn left(&self, tp: &Toolpath, feed_scale: f64, rapid_scale: f64, skip: &[bool]) -> Left {
        let n = tp.t_end.len();
        if n == 0 || tp.segs.len() != n {
            return Left::default();
        }
        let i = self.cursor.min(n - 1);
        let op = tp.segs[i].op;
        // The op ends where the next op's first piece starts.
        let end = tp.segs[i..].iter().position(|s| s.op != op).map_or(n, |k| i + k);
        let rapid_at = |k: usize| if k == 0 { 0.0 } else { tp.t_rapid_end[k - 1] as f64 };
        let op_left = scaled(tp.t_start(end) - self.at, rapid_at(end) - self.at_rapid, feed_scale, rapid_scale);
        let later: f64 = tp.ops.iter().enumerate().skip(op + 1).filter(|(j, _)| !skip.get(*j).copied().unwrap_or(false)).map(|(_, o)| scaled(o.seconds, o.rapid_seconds, feed_scale, rapid_scale)).sum();
        Left { op, op_left, total: op_left + later }
    }
}

/// Time left in a run (`Tracker::left`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Left {
    /// Index into `Toolpath::ops` of the op the tool is in.
    pub op: usize,
    pub op_left: f64,
    pub total: f64,
}

/// Seconds `t` (of which `rapid` in rapids) at the given overrides. A
/// held or zeroed override counts as 100 %: the time is still what is left.
pub fn scaled(t: f64, rapid: f64, feed_scale: f64, rapid_scale: f64) -> f64 {
    let fs = if feed_scale > 0.05 { feed_scale } else { 1.0 };
    let rs = if rapid_scale > 0.05 { rapid_scale } else { 1.0 };
    let rapid = rapid.clamp(0.0, t.max(0.0));
    (t - rapid).max(0.0) / fs + rapid / rs
}

/// The whole program's time at the given overrides, without skipped ops.
pub fn total(tp: &Toolpath, feed_scale: f64, rapid_scale: f64, skip: &[bool]) -> f64 {
    tp.ops.iter().enumerate().filter(|(j, _)| !skip.get(*j).copied().unwrap_or(false)).map(|(_, o)| scaled(o.seconds, o.rapid_seconds, feed_scale, rapid_scale)).sum()
}

/// Distance from `p` to a piece (XYZ, or A for a turn), and how far along it the closest point is.
fn closest(s: &Seg, p: P4) -> (f64, f64) {
    let d = sub(s.to, s.from);
    let l2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    if l2 < 1e-18 {
        let span = d[3];
        let f = if span.abs() > 1e-9 { ((p[3] - s.from[3]) / span).clamp(0.0, 1.0) } else { 0.0 };
        return (xyz_len(sub(p, s.from)), f);
    }
    let w = sub(p, s.from);
    let f = ((w[0] * d[0] + w[1] * d[1] + w[2] * d[2]) / l2).clamp(0.0, 1.0);
    let q = [s.from[0] + d[0] * f, s.from[1] + d[1] * f, s.from[2] + d[2] * f, 0.0];
    (xyz_len(sub(p, q)), f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Builder;

    fn lim(v: f64, a: f64) -> Limits {
        Limits { vel: [v, v, v, 360.0], acc: [a, a, a, 720.0], max_linear_vel: 0.0, max_linear_acc: 0.0, servo_period: 0.0, depth: 0, io_wait: 0.0, stop_overhead: 0.0 }
    }

    #[test]
    fn canon_stops_dwells_and_modes() {
        let out = "    1 N..... STRAIGHT_TRAVERSE(0.0000, 0.0000, 5.0000, 0.0000, 0.0000, 0.0000)
    2 N1     STRAIGHT_TRAVERSE(10.0000, 0.0000, 5.0000, 0.0000, 0.0000, 0.0000)
    3 N2     SET_FEED_RATE(600.0000)
    4 N2     STRAIGHT_FEED(10.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000)
    5 N3     FLOOD_ON()
    6 N3     STRAIGHT_FEED(20.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000)
    7 N4     DWELL(0.5000)
    8 N5     SET_MOTION_CONTROL_MODE(CANON_CONTINUOUS, 0.050000)
    9 N5     ARC_FEED(30.0000, 0.0000, 25.0000, 0.0000, -1, 0.0000, 0.0000, 0.0000, 0.0000)
   10 N6     SET_MOTION_CONTROL_MODE(CANON_EXACT_PATH)
   11 N6     STRAIGHT_FEED(40.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000)
";
        let tp = crate::canon::parse(out, 100.0, false);
        let m: Vec<_> = tp.moves.iter().map(|m| (m.stop_before, m.dwell_before, m.blend, m.radius)).collect();
        assert_eq!(m, [(false, 0.0, 0.0, 0.0), (false, 0.0, 0.0, 0.0), (true, 0.0, 0.0, 0.0), (true, 0.5, 0.05, 5.0), (false, 0.0, -1.0, 0.0)]);
        assert_eq!(tp.moves.iter().map(|m| m.io_waits).collect::<Vec<_>>(), [0, 0, 1, 0, 0]);
    }

    #[test]
    fn job_markers_carry_the_op_id() {
        let tp = crate::mini::parse("(MYMILL-OP 1/1 ID=A62CA28A FACE 100 X 60)\nG0 X0 Y0 Z5\nG1 X10 F600\n", 100.0);
        assert_eq!((tp.ops[0].id.as_str(), tp.ops[0].name.as_str()), ("A62CA28A", "FACE 100 X 60"));
    }

    #[test]
    fn a_long_line_is_a_trapezoid() {
        // 100 mm at 10 mm/s with 100 mm/s²: 0.1 s up, 0.1 s down (1 mm
        // between them), 98 mm cruising.
        let mut b = Builder::new([0.0; 4], 100.0);
        b.feed_rate(600.0);
        b.line(Kind::Feed, [100.0, 0.0, 0.0, 0.0]);
        let mut tp = b.finish();
        estimate(&mut tp, &lim(100.0, 100.0));
        assert!((tp.seconds() - 10.1).abs() < 1e-6, "{}", tp.seconds());
    }

    #[test]
    fn a_short_line_never_reaches_the_feed() {
        // 1 mm with 100 mm/s²: up for 0.1 s to 10 mm/s and down again.
        let mut b = Builder::new([0.0; 4], 100.0);
        b.feed_rate(6000.0);
        b.line(Kind::Feed, [1.0, 0.0, 0.0, 0.0]);
        let mut tp = b.finish();
        estimate(&mut tp, &lim(1000.0, 100.0));
        assert!((tp.seconds() - 0.2).abs() < 1e-6, "{}", tp.seconds());
    }

    #[test]
    fn straight_on_keeps_the_speed_and_a_reversal_stops() {
        let run = |second: P4, f: f64| {
            let mut b = Builder::new([0.0; 4], 100.0);
            b.feed_rate(f);
            b.line(Kind::Feed, [50.0, 0.0, 0.0, 0.0]);
            b.line(Kind::Feed, second);
            let mut tp = b.finish();
            estimate(&mut tp, &lim(100.0, 100.0));
            tp.seconds()
        };
        assert!((run([100.0, 0.0, 0.0, 0.0], 600.0) - 10.1).abs() < 1e-6);
        assert!((run([0.0, 0.0, 0.0, 0.0], 600.0) - 10.2).abs() < 1e-6);
        // A right angle between long moves blends on a 25 mm radius: at
        // 10 mm/s that needs no slowing down, at 60 mm/s it does (to 46.5),
        // but only part way.
        assert!((run([50.0, 50.0, 0.0, 0.0], 600.0) - 10.1).abs() < 1e-6);
        let (straight, corner) = (run([100.0, 0.0, 0.0, 0.0], 3600.0), run([50.0, 50.0, 0.0, 0.0], 3600.0));
        let stop = run([0.0, 0.0, 0.0, 0.0], 3600.0);
        assert!(corner > straight + 0.01 && corner < stop - 0.1, "{straight} {corner} {stop}");
    }

    #[test]
    fn a_stop_or_dwell_brings_motion_to_rest() {
        let mut b = Builder::new([0.0; 4], 100.0);
        b.feed_rate(600.0);
        b.line(Kind::Feed, [50.0, 0.0, 0.0, 0.0]);
        b.dwell(0.5);
        b.line(Kind::Feed, [100.0, 0.0, 0.0, 0.0]);
        let mut tp = b.finish();
        estimate(&mut tp, &lim(100.0, 100.0));
        assert!((tp.seconds() - 10.7).abs() < 1e-6, "{}", tp.seconds());
    }

    #[test]
    fn a_small_arc_is_held_back_by_sideways_acceleration() {
        // r = 1 mm, 100 mm/s²: sqrt(86.6) = 9.3 mm/s, not the 50 mm/s asked.
        let mut b = Builder::new([1.0, 0.0, 0.0, 0.0], 100.0);
        b.feed_rate(3000.0);
        b.begin_arc(1.0);
        let mut sub = Builder::new([1.0, 0.0, 0.0, 0.0], 1.0);
        sub.arc_xy([1.0, 0.0, 0.0, 0.0], (0.0, 0.0), 1);
        for s in sub.finish().segs {
            b.line(Kind::Feed, s.to);
        }
        b.end_arc();
        let mut tp = b.finish();
        assert_eq!(tp.moves.len(), 1);
        estimate(&mut tp, &lim(100.0, 100.0));
        let cruise = std::f64::consts::TAU / (100.0 * ACC_NORMAL).sqrt();
        assert!(tp.seconds() > cruise && tp.seconds() < cruise + 0.5, "{}", tp.seconds());
    }

    #[test]
    fn the_tracker_follows_the_tool_and_counts_down() {
        let mut b = Builder::new([0.0; 4], 100.0);
        b.feed_rate(600.0);
        b.comment("ONE");
        b.set_line(1);
        b.line(Kind::Feed, [100.0, 0.0, 0.0, 0.0]);
        b.comment("TWO");
        b.set_line(2);
        b.line(Kind::Feed, [0.0, 0.0, 0.0, 0.0]);
        let mut tp = b.finish();
        estimate(&mut tp, &lim(100.0, 100.0));
        let mut t = Tracker::default();
        t.update(&tp, 1, [50.0, 0.0, 0.0, 0.0], 0.5);
        let l = t.left(&tp, 1.0, 1.0, &[]);
        assert!((l.total - 15.15).abs() < 0.01 && (l.op_left - 5.05).abs() < 0.01 && l.op == 0, "{l:?}");
        // With the second op skipped, only the first's rest is left.
        assert!((t.left(&tp, 1.0, 1.0, &[false, true]).total - 5.05).abs() < 0.01);
        // Back over the same spot on the way home: the second op.
        t.update(&tp, 2, [50.0, 0.0, 0.0, 0.0], 0.5);
        let l = t.left(&tp, 1.0, 1.0, &[]);
        assert!((l.total - 5.05).abs() < 0.01 && (l.op_left - 5.05).abs() < 0.01 && l.op == 1, "{l:?}");
        // Half the feed: twice the time.
        assert!((t.left(&tp, 0.5, 1.0, &[]).total - 10.1).abs() < 0.01);
        // A line the toolpath doesn't have (a subroutine's) leaves it where it is.
        t.update(&tp, 77, [500.0, 0.0, 0.0, 0.0], 0.5);
        assert_eq!(t.left(&tp, 1.0, 1.0, &[]).op, 1);
        // Run from line 1 again: found by its line before the tool is there.
        let mut t = Tracker::default();
        t.cursor = 1;
        t.update(&tp, 1, [500.0, 0.0, 0.0, 0.0], 0.5);
        assert_eq!((t.cursor, t.at), (0, 0.0));
    }
}
