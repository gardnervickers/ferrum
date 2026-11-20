//! The preview on the probe picture: the tip plays the selected routine in
//! a loop (the mockup's 1× preview), so the operator sees where the probe
//! must start and what it will do before holding the button.
//!
//! A `Script` is built from the same approach moves the picture's arrows
//! show, following what the macros do: which routines touch the top first,
//! which stay at depth, where the operator jogs between runs. Times are
//! for reading, not the machine's feeds.

use crate::{Live, MainWindow};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// The tip's radius at depth, in picture units; arrows end on the surface,
/// so the tip's center stops this far short of it.
pub const TIP_R: f64 = 6.0;
/// How far the tip backs off a surface between the fast and slow touches.
const BACK: f64 = 12.0;
/// The pause on the last frame before the loop starts again.
const HOLD_MS: f64 = 1200.0;
/// How far a flat on the rotary is drawn off level before it is found.
pub const FLAT_TILT: f64 = 6.0;
/// The rotary's axis in the end view, and the part's radius.
pub const ROT_C: (f64, f64) = (200.0, 165.0);
pub const ROT_R: f64 = 70.0;

type Pt = (f64, f64);

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    from: Pt,
    to: Pt,
    /// Height above the probing depth: 1 clear of the part, 0 at depth.
    z0: f64,
    z1: f64,
    /// Rotary angle in degrees.
    a0: f64,
    a1: f64,
    ms: f64,
    label: String,
    /// Ends on the surface.
    touch: bool,
    /// Leaves a line on the picture.
    draws: bool,
}

/// What the selected routine does, as timed steps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    steps: Vec<Step>,
    /// The picture is a side or end view: every move is in its plane, so
    /// the tip keeps one size and only the touches leave a line.
    flat_view: bool,
    /// What turns with A in the end view: "" (nothing), "round" or "flat".
    part: &'static str,
}

/// The tip at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub a: f64,
    pub label: String,
    /// On the surface now.
    pub contact: bool,
    /// SVG path of the lines drawn so far.
    pub trail: String,
}

/// What `script` needs to know about the selection.
pub struct Routine<'a> {
    pub kind: &'a str,
    /// The approach moves on the picture: start and contact of each touch.
    pub segs: &'a [[f64; 4]],
    /// The selected spot (a corner, a center).
    pub at: Pt,
    /// EDGE: also measure the edge's angle.
    pub angle: bool,
    /// RIDGE / SLOT: a ridge (probed from outside, over the top).
    pub ridge: bool,
    /// BLOCK center: the side the walk starts on.
    pub side: &'a str,
}

struct Builder {
    steps: Vec<Step>,
    cur: Pt,
    z: f64,
    a: f64,
    flat_view: bool,
}

fn dist(a: Pt, b: Pt) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Unit vector from `a` to `b`.
fn unit(a: Pt, b: Pt) -> Pt {
    let l = dist(a, b).max(1e-9);
    ((b.0 - a.0) / l, (b.1 - a.1) / l)
}

impl Builder {
    fn new(start: Pt, z: f64, a: f64, flat_view: bool) -> Self {
        Self { steps: Vec::new(), cur: start, z, a, flat_view }
    }

    fn push(&mut self, to: Pt, z: f64, a: f64, ms: f64, label: &str, touch: bool) {
        let draws = to != self.cur && if self.flat_view { touch } else { self.z < 0.5 && z < 0.5 };
        self.steps.push(Step { from: self.cur, to, z0: self.z, z1: z, a0: self.a, a1: a, ms, label: label.into(), touch, draws });
        self.cur = to;
        self.z = z;
        self.a = a;
    }

    /// Move to `to` at height `z`.
    fn go(&mut self, to: Pt, z: f64, ms: f64, label: &str) {
        self.push(to, z, self.a, ms, label, false);
    }

    /// Stay put.
    fn wait(&mut self, ms: f64, label: &str) {
        self.push(self.cur, self.z, self.a, ms, label, false);
    }

    /// Turn the rotary to `a` degrees.
    fn turn(&mut self, a: f64, label: &str) {
        let ms = (a - self.a).abs() * 10.0;
        self.push(self.cur, self.z, a, ms.max(600.0), label, false);
    }

    /// The fast touch toward the surface point `to`, a short back-off and
    /// the slow touch that is measured. `what` names the touch ("1/2").
    fn touch(&mut self, to: Pt, what: &str) {
        let u = unit(self.cur, to);
        let hit = (to.0 - u.0 * TIP_R, to.1 - u.1 * TIP_R);
        let tag = if what.is_empty() { String::new() } else { format!(" · {what}") };
        let ms = (dist(self.cur, hit) * 28.0).clamp(500.0, 2400.0);
        self.push(hit, self.z, self.a, ms, &format!("FAST TOUCH{tag}"), true);
        self.go((hit.0 - u.0 * BACK, hit.1 - u.1 * BACK), self.z, 300.0, "BACK OFF");
        self.push(hit, self.z, self.a, 1200.0, &format!("SLOW TOUCH{tag}"), true);
    }

    /// The Z touch on the top face that the corner, boss, ridge and edge
    /// angle routines make first, seen from above: the tip comes down.
    fn top(&mut self, at: Pt, start: &str) {
        self.go(at, 1.0, 700.0, start);
        self.push(at, 0.4, self.a, 1300.0, "TOUCH THE TOP · Z", true);
        self.go(at, 1.0, 500.0, "RETRACT Z");
    }

    /// One side touch reached over the top: off the side, down, touch, up.
    fn over(&mut self, seg: &[f64; 4], what: &str) {
        let (a, c) = ((seg[0], seg[1]), (seg[2], seg[3]));
        self.go(a, 1.0, 700.0, "MOVE OFF THE SIDE");
        self.go(a, 0.0, 600.0, "LOWER TO DEPTH");
        self.touch(c, what);
        let u = unit(a, c);
        let back = (self.cur.0 - u.0 * BACK, self.cur.1 - u.1 * BACK);
        self.go(back, 0.0, 300.0, "BACK OFF");
        self.go(back, 1.0, 500.0, "RETRACT Z");
    }
}

fn of(i: usize, n: usize) -> String {
    if n > 1 { format!("{}/{}", i + 1, n) } else { String::new() }
}

/// Top touch at A0, then the two sides with the part turned a quarter each
/// way: the rotary centerline, diameter and (per station) deflection moves.
fn rotary_station(b: &mut Builder, tag: &str, with_back: bool) {
    let (cx, cy) = ROT_C;
    let (up, left, right) = (40.0, cx - 120.0, cx + 120.0);
    b.go((cx, up), 0.0, 600.0, &format!("{tag}OVER THE CENTERLINE"));
    b.touch((cx, cy - ROT_R), "TOP AT A0");
    b.go((cx, up), 0.0, 500.0, "RETRACT Z");
    b.turn(90.0, "TURN TO A+90");
    b.go((left, up), 0.0, 700.0, "MOVE TO THE Y− SIDE");
    b.go((left, cy), 0.0, 700.0, "LOWER TO THE CENTERLINE");
    b.touch((cx - ROT_R, cy), "SIDE AT A+90");
    b.go((left, cy), 0.0, 300.0, "BACK OFF");
    b.go((left, up), 0.0, 600.0, "RETRACT Z");
    b.turn(-90.0, "TURN TO A−90");
    b.go((right, up), 0.0, 1100.0, "MOVE TO THE Y+ SIDE");
    b.go((right, cy), 0.0, 700.0, "LOWER TO THE CENTERLINE");
    b.touch((cx + ROT_R, cy), "SIDE AT A−90");
    b.go((right, cy), 0.0, 300.0, "BACK OFF");
    b.go((right, up), 0.0, 600.0, "RETRACT Z");
    if with_back {
        b.turn(180.0, "TURN TO A180");
        b.go((cx, up), 0.0, 700.0, "OVER THE CENTERLINE");
        b.touch((cx, cy - ROT_R), "TOP AT A180");
        b.go((cx, up), 0.0, 500.0, "RETRACT Z");
    }
    b.turn(0.0, "BACK TO A0");
    b.go((cx, up), 0.0, 600.0, "OVER THE CENTERLINE");
}

/// Where the tilted flat is under `x` in the end view.
pub fn flat_y(x: f64) -> f64 {
    let r = FLAT_TILT.to_radians();
    let p0 = (ROT_C.0 - 50.0 * r.sin(), ROT_C.1 - 50.0 * r.cos());
    p0.1 - (x - p0.0) / r.cos() * r.sin()
}

pub fn script(r: &Routine) -> Script {
    let segs = r.segs;
    let n = segs.len();
    let start = |i: usize| (segs[i][0], segs[i][1]);
    let end = |i: usize| (segs[i][2], segs[i][3]);
    let rotary = matches!(r.kind, "cyl" | "dia" | "defl" | "runout" | "flat");
    let flat_view = rotary || matches!(r.kind, "top" | "setter" | "nose");
    let part = match r.kind {
        "flat" => "flat",
        _ if rotary => "round",
        _ => "",
    };
    if n == 0 {
        return Script { steps: Vec::new(), flat_view, part };
    }
    let mut b = Builder::new(start(0), if flat_view { 0.0 } else { 1.0 }, if r.kind == "flat" { FLAT_TILT } else { 0.0 }, flat_view);
    match r.kind {
        // Top Z just inside the corner, then each side from outside it.
        "corner" => {
            let (dx, dy) = (if r.at.0 < 200.0 { -1.0 } else { 1.0 }, if r.at.1 < 150.0 { -1.0 } else { 1.0 });
            b.cur = (r.at.0 - dx * 30.0, r.at.1 - dy * 30.0);
            b.top(b.cur, "START OVER THE TOP, JUST INSIDE THE CORNER");
            for (i, s) in segs.iter().enumerate() {
                b.over(s, &of(i, n));
            }
            b.go(r.at, 1.0, 800.0, "MOVE OVER THE CORNER");
        }
        // Top Z on the rim, then both walls from one spot inside the pocket.
        "icorner" => {
            let (dx, dy) = (if r.at.0 < 200.0 { -1.0 } else { 1.0 }, if r.at.1 < 150.0 { -1.0 } else { 1.0 });
            b.cur = (r.at.0 + dx * 22.0, r.at.1 + dy * 22.0);
            b.top(b.cur, "START OVER THE TOP, BESIDE THE CORNER");
            let inside = start(0);
            b.go(inside, 1.0, 700.0, "MOVE OVER THE POCKET");
            b.go(inside, 0.0, 600.0, "LOWER TO DEPTH");
            for i in 0..n {
                b.touch(end(i), &of(i, n));
                b.go(inside, 0.0, 450.0, "BACK TO THE START");
            }
            b.go(inside, 1.0, 500.0, "RETRACT Z");
            b.go(r.at, 1.0, 800.0, "MOVE OVER THE CORNER");
        }
        // Top Z inside the edge, then two touches along it.
        "edge" if r.angle => {
            let u = unit(start(0), end(0));
            let over = (end(0).0 + u.0 * 30.0, end(0).1 + u.1 * 30.0);
            b.cur = over;
            b.top(over, "START OVER THE TOP, NEAR THE EDGE");
            b.go(start(0), 1.0, 700.0, "MOVE OFF THE EDGE");
            b.go(start(0), 0.0, 600.0, "LOWER TO DEPTH");
            for i in 0..n {
                if i > 0 {
                    // Along the edge, as far off it as the last back-off.
                    let d = dist(b.cur, end(i - 1));
                    b.go((end(i).0 - u.0 * d, end(i).1 - u.1 * d), 0.0, 900.0, "MOVE ALONG THE EDGE");
                }
                b.touch(end(i), &of(i, n));
                b.go((b.cur.0 - u.0 * BACK, b.cur.1 - u.1 * BACK), 0.0, 300.0, "BACK OFF");
            }
            b.go(b.cur, 1.0, 500.0, "RETRACT Z");
            b.go(over, 1.0, 900.0, "BACK OVER THE START");
        }
        // One touch from where the probe is; no Z move.
        "edge" => {
            b.z = 0.0;
            b.wait(900.0, "START BESIDE THE EDGE, AT DEPTH");
            b.touch(end(0), "");
            b.go(start(0), 0.0, 500.0, "BACK OFF");
        }
        // Around the outside at depth, a touch on each side; no Z move.
        "stock" => {
            b.z = 0.0;
            b.wait(900.0, &format!("START BESIDE THE {} SIDE, AT DEPTH", r.side));
            for i in 0..n {
                b.touch(end(i), &of(i, n));
                b.go(start(i), 0.0, 450.0, "BACK OFF");
                if i + 1 < n {
                    // Round the block's corner: the one further from its middle.
                    let (p, q) = (start(i), start(i + 1));
                    let (c1, c2) = ((p.0, q.1), (q.0, p.1));
                    let corner = if dist(c1, (200.0, 150.0)) > dist(c2, (200.0, 150.0)) { c1 } else { c2 };
                    b.go(corner, 0.0, dist(p, corner) * 7.0, "WALK AROUND THE CORNER");
                    b.go(q, 0.0, dist(corner, q) * 7.0, "WALK AROUND THE CORNER");
                }
            }
        }
        // One face per run; the operator jogs across in between.
        "two" => {
            b.z = 0.0;
            b.wait(900.0, &format!("START BESIDE THE {} SIDE, AT DEPTH", r.side));
            b.touch(end(0), "1/2");
            b.go(start(0), 0.0, 450.0, "BACK OFF");
            if n > 1 {
                b.go(start(0), 1.0, 700.0, "YOU JOG: UP");
                b.go(start(1), 1.0, 1500.0, "YOU JOG: ACROSS TO THE OPPOSITE SIDE");
                b.go(start(1), 0.0, 700.0, "YOU JOG: DOWN TO DEPTH");
                b.touch(end(1), "2/2");
                b.go(start(1), 0.0, 450.0, "BACK OFF");
            }
        }
        // Top Z in the middle, then each side from outside, over the top.
        "boss" | "slot" if r.kind == "boss" || r.ridge => {
            b.cur = r.at;
            b.top(r.at, "START OVER THE MIDDLE");
            for (i, s) in segs.iter().enumerate() {
                b.over(s, &of(i, n));
            }
            b.go(r.at, 1.0, 800.0, "MOVE OVER THE CENTER");
        }
        // From the middle at depth, out to each wall and back.
        "pocket" | "bore" | "slot" => {
            b.z = 0.0;
            b.wait(900.0, "START NEAR THE MIDDLE, AT DEPTH");
            for i in 0..n {
                b.touch(end(i), &of(i, n));
                b.go(start(i), 0.0, 500.0, "BACK TO THE MIDDLE");
            }
        }
        // Top Z on the ring's wall, then the bore from its middle.
        "ring" => {
            let wall = (r.at.0 - 83.0, r.at.1);
            b.cur = wall;
            b.top(wall, "START OVER THE RING'S TOP");
            b.go(start(0), 1.0, 700.0, "MOVE OVER THE BORE");
            b.go(start(0), 0.0, 600.0, "LOWER TO DEPTH");
            for i in 0..n {
                b.touch(end(i), &of(i, n));
                b.go(start(i), 0.0, 500.0, "BACK TO THE MIDDLE");
            }
            b.go(start(0), 1.0, 500.0, "RETRACT Z");
        }
        "cyl" | "dia" => rotary_station(&mut b, "", false),
        "defl" => {
            rotary_station(&mut b, "STATION 1 · ", true);
            b.wait(1500.0, "MOVE ALONG X TO STATION 2");
            rotary_station(&mut b, "STATION 2 · ", true);
        }
        "runout" => {
            let (cx, cy) = ROT_C;
            b.go((cx, 40.0), 0.0, 600.0, "OVER THE CENTERLINE");
            for (i, a) in [0.0, 90.0, 180.0, 270.0].into_iter().enumerate() {
                if i > 0 {
                    b.turn(a, &format!("TURN TO A{a:.0}"));
                }
                b.touch((cx, cy - ROT_R), &format!("TOP AT A{a:.0}"));
                b.go((cx, 40.0), 0.0, 500.0, "RETRACT Z");
            }
            b.turn(360.0, "ON TO A0");
        }
        // Two touches on the flat, mirrored across the centerline; the
        // difference is the angle that A0 is set from.
        "flat" => {
            for i in 0..n {
                b.go(start(i), 0.0, 700.0, if i == 0 { "START OVER THE FLAT, OFF CENTER" } else { "MIRROR ACROSS THE CENTERLINE" });
                b.touch(end(i), &of(i, n));
                b.go(start(i), 0.0, 500.0, "RETRACT Z");
            }
            b.turn(0.0, "A0 IS WHERE THE FLAT IS LEVEL");
        }
        // Side views: straight down onto the face, setter or nose reference.
        _ => {
            b.wait(900.0, match r.kind {
                "setter" => "OVER THE TOOL SETTER",
                "nose" => "SPINDLE NOSE OVER THE SETTER",
                _ => "START ABOVE THE FACE",
            });
            b.touch(end(0), "");
            b.go(start(0), 0.0, 700.0, "RETRACT Z");
        }
    }
    Script { steps: b.steps, flat_view, part }
}

impl Script {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    pub fn total_ms(&self) -> f64 {
        self.steps.iter().map(|s| s.ms).sum()
    }

    /// The length of one loop, with the pause at its end.
    pub fn loop_ms(&self) -> f64 {
        self.total_ms() + HOLD_MS
    }

    /// The angle the part is drawn at before the preview plays.
    pub fn rest_angle(&self) -> f64 {
        self.steps.first().map_or(0.0, |s| s.a0)
    }

    /// The tip `ms` into the loop.
    pub fn at(&self, ms: f64) -> Option<Frame> {
        let last = self.steps.last()?;
        let mut t = ms.rem_euclid(self.loop_ms());
        let mut trail = String::new();
        let mut pen: Option<Pt> = None;
        let mut draw = |trail: &mut String, from: Pt, to: Pt| {
            if pen != Some(from) {
                *trail += &format!("M {:.1} {:.1} ", from.0, from.1);
            }
            *trail += &format!("L {:.1} {:.1} ", to.0, to.1);
            pen = Some(to);
        };
        for s in &self.steps {
            if t >= s.ms {
                t -= s.ms;
                if s.draws {
                    draw(&mut trail, s.from, s.to);
                }
                continue;
            }
            let k = t / s.ms;
            // Ease the travel; touches run at a steady feed.
            let e = if s.touch { k } else { k * k * (3.0 - 2.0 * k) };
            let (x, y) = (s.from.0 + (s.to.0 - s.from.0) * e, s.from.1 + (s.to.1 - s.from.1) * e);
            if s.draws {
                draw(&mut trail, s.from, (x, y));
            }
            return Some(Frame { x, y, z: s.z0 + (s.z1 - s.z0) * e, a: s.a0 + (s.a1 - s.a0) * e, label: s.label.clone(), contact: s.touch && k > 0.9, trail });
        }
        // The pause at the end: the whole routine stays drawn.
        Some(Frame { x: last.to.0, y: last.to.1, z: last.z1, a: last.a1, label: "DONE".into(), contact: false, trail })
    }
}

/// The piece of the end view that turns with A, as SVG path data: a round
/// part's index mark, or the whole outline of a part with a flat.
pub fn part_path(part: &str, a: f64) -> String {
    let (cx, cy) = ROT_C;
    // +A turns the picture counter-clockwise (y points down on screen).
    let (s, c) = (-a).to_radians().sin_cos();
    let rot = |dx: f64, dy: f64| (cx + dx * c - dy * s, cy + dx * s + dy * c);
    match part {
        "round" => {
            let (p, q) = (rot(0.0, 0.0), rot(0.0, 46.0));
            let (d0, d1) = (rot(-5.0, 51.0), rot(5.0, 51.0));
            format!("M {:.1} {:.1} L {:.1} {:.1} M {:.1} {:.1} A 5 5 0 1 0 {:.1} {:.1} A 5 5 0 1 0 {:.1} {:.1} Z", p.0, p.1, q.0, q.1, d0.0, d0.1, d1.0, d1.1, d0.0, d0.1)
        }
        "flat" => {
            let half = (ROT_R * ROT_R - 50.0 * 50.0).sqrt();
            let (l, r) = (rot(-half, -50.0), rot(half, -50.0));
            let (m, n) = (rot(0.0, 0.0), rot(0.0, 46.0));
            format!("M {:.1} {:.1} A {ROT_R} {ROT_R} 0 1 0 {:.1} {:.1} Z M {:.1} {:.1} L {:.1} {:.1}", l.0, l.1, r.0, r.1, m.0, m.1, n.0, n.1)
        }
        _ => String::new(),
    }
}

struct Player {
    script: Script,
    started: Instant,
    timer: slint::Timer,
    /// The tip was showing at the last tick.
    showing: bool,
}

thread_local! {
    static PLAYER: RefCell<Player> = RefCell::new(Player { script: Script::default(), started: Instant::now(), timer: slint::Timer::default(), showing: false });
    /// `--show probe-at:<ms>`: a fixed moment, for headless screenshots.
    static FIXED_MS: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

const FRAME: Duration = Duration::from_millis(33);
const IDLE: Duration = Duration::from_millis(250);
/// Loops played before the preview rests on its last frame. A preview that
/// loops forever redraws the whole screen 30 times a second for as long as
/// PROBE is open; a new shape, target or option (or coming back to PROBE)
/// plays it again.
const PLAYS: f64 = 2.0;

/// Hold the preview at `ms` into its loop (headless screenshots).
pub fn freeze(ui: &MainWindow, ms: f64) {
    FIXED_MS.with(|f| f.set(Some(ms)));
    tick(ui);
}

/// One frame: where the tip is now. Plays only while the PROBE screen shows
/// the picture and no probe is running; otherwise the tip stays where it
/// was (a run hides it on the screen) and the clock idles.
fn tick(ui: &MainWindow) {
    let live = ui.global::<Live>();
    PLAYER.with(|p| {
        let mut p = p.borrow_mut();
        let active = live.get_screen() == "probe" && !live.get_pr_running() && !p.script.is_empty();
        if !active {
            if p.showing {
                p.showing = false;
                p.timer.set_interval(IDLE);
            }
            return;
        }
        if !p.showing {
            // Back on the screen: play from the start.
            p.showing = true;
            p.started = Instant::now();
            p.timer.set_interval(FRAME);
        }
        let ms = FIXED_MS.with(|f| f.get()).unwrap_or_else(|| {
            let ms = p.started.elapsed().as_secs_f64() * 1e3;
            let end = PLAYS * p.script.loop_ms();
            if ms < end {
                return ms;
            }
            // Done: rest on the finished routine and tick slowly.
            p.timer.set_interval(IDLE);
            end - 1.0
        });
        draw(&live, &p.script, ms);
    });
}

/// Show the tip, its trail and the caption at `ms` into `script`.
fn draw(live: &Live, script: &Script, ms: f64) {
    let Some(f) = script.at(ms) else {
        live.set_pr_tip_on(false);
        live.set_pr_trail("".into());
        live.set_pr_caption("".into());
        return;
    };
    live.set_pr_tip_on(true);
    live.set_pr_tip_x(f.x as f32);
    live.set_pr_tip_y(f.y as f32);
    live.set_pr_tip_z(f.z as f32);
    live.set_pr_tip_contact(f.contact);
    if live.get_pr_trail() != f.trail.as_str() {
        live.set_pr_trail(f.trail.into());
    }
    let caption = if script.part.is_empty() { format!("PREVIEW · {}", f.label) } else { format!("PREVIEW · {} · A {:.0}°", f.label, if f.a >= 360.0 { f.a - 360.0 } else { f.a }) };
    if live.get_pr_caption() != caption.as_str() {
        live.set_pr_caption(caption.into());
    }
    if !script.part.is_empty() {
        let part = part_path(script.part, f.a);
        if live.get_pr_part() != part.as_str() {
            live.set_pr_part(part.into());
        }
    }
}

/// Play `script` on the picture, from the start when it differs from the
/// one playing (a new shape, target or option).
///
/// The tip is never hidden to restart: the picture shows the approach
/// arrows instead of the preview when there is none (`pr-preview`), and a
/// hidden tip for even one frame flashed the arrows of the new routine.
/// Until PROBE plays it (now if it is showing, else on coming back), the new
/// routine rests on its finished frame, which is also what a screen in the
/// browser sees while the touchscreen is elsewhere.
pub fn play(ui: &MainWindow, script: Script) {
    let live = ui.global::<Live>();
    live.set_pr_preview(!script.is_empty());
    let same = PLAYER.with(|p| p.borrow().script == script);
    if !same {
        live.set_pr_part(part_path(script.part, script.rest_angle()).into());
        draw(&live, &script, PLAYS * script.loop_ms() - 1.0);
        PLAYER.with(|p| {
            let mut p = p.borrow_mut();
            p.script = script;
            p.started = Instant::now();
            // Played again from the first step (below).
            p.showing = false;
            if !p.timer.running() {
                let weak = ui.as_weak();
                p.timer.start(slint::TimerMode::Repeated, FRAME, move || {
                    if let Some(ui) = weak.upgrade() {
                        tick(&ui);
                    }
                });
            } else {
                p.timer.set_interval(FRAME);
            }
        });
    }
    wake(ui);
}

/// Draw the preview's first frame now rather than on the timer's next tick
/// (up to 250 ms away when idle): a new routine, coming back to PROBE or the
/// end of a run shows the tip at once, in the same frame as the rest of the
/// picture.
pub fn wake(ui: &MainWindow) {
    let idle = PLAYER.with(|p| !p.borrow().showing);
    if idle {
        tick(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routine<'a>(kind: &'a str, segs: &'a [[f64; 4]]) -> Routine<'a> {
        Routine { kind, segs, at: (200.0, 150.0), angle: false, ridge: false, side: "LEFT" }
    }

    #[test]
    fn an_edge_stays_at_depth_and_stops_a_tip_radius_short() {
        let segs = [[14.0, 150.0, 100.0, 150.0]];
        let s = script(&routine("edge", &segs));
        assert!(s.steps.iter().all(|st| st.z0 == 0.0 && st.z1 == 0.0));
        let touch = s.steps.iter().find(|st| st.touch).unwrap();
        assert_eq!(touch.to, (100.0 - TIP_R, 150.0));
        // Fast touch, back off, slow touch.
        assert_eq!(s.steps.iter().filter(|st| st.touch).count(), 2);
    }

    #[test]
    fn a_corner_touches_the_top_first_and_ends_over_the_corner() {
        let segs = [[40.0, 200.0, 100.0, 200.0], [130.0, 290.0, 130.0, 230.0]];
        let s = script(&Routine { at: (100.0, 230.0), ..routine("corner", &segs) });
        let first_touch = s.steps.iter().find(|st| st.touch).unwrap();
        assert!(first_touch.label.contains("TOP"));
        assert_eq!(first_touch.to, (130.0, 200.0));
        // Sideways moves between the touches are made clear of the part.
        assert!(s.steps.iter().filter(|st| st.label == "MOVE OFF THE SIDE").all(|st| st.z0 == 1.0 && st.z1 == 1.0));
        let last = s.steps.last().unwrap();
        assert_eq!((last.to, last.z1), ((100.0, 230.0), 1.0));
    }

    #[test]
    fn the_loop_plays_every_step_then_rests_and_starts_again() {
        let segs = [[14.0, 150.0, 100.0, 150.0]];
        let s = script(&routine("edge", &segs));
        let total = s.total_ms();
        let f0 = s.at(0.0).unwrap();
        assert_eq!((f0.x, f0.y, f0.trail.as_str()), (14.0, 150.0, ""));
        // At the end of the fast touch the tip is on the surface, with its line drawn.
        let before: f64 = s.steps.iter().take_while(|st| !st.touch).map(|st| st.ms).sum();
        let f = s.at(before + s.steps.iter().find(|st| st.touch).unwrap().ms - 1.0).unwrap();
        assert!(f.contact && (f.x - (100.0 - TIP_R)).abs() < 0.5, "{f:?}");
        assert!(f.trail.starts_with("M 14.0 150.0 L "), "{}", f.trail);
        let rest = s.at(total + 10.0).unwrap();
        assert_eq!(rest.label, "DONE");
        assert_eq!(s.at(s.loop_ms() + 5.0).unwrap().label, s.at(5.0).unwrap().label);
    }

    #[test]
    fn the_rotary_turns_between_touches_and_comes_back() {
        let segs = [[200.0, 40.0, 200.0, 95.0]];
        for kind in ["cyl", "dia", "defl", "runout"] {
            let s = script(&routine(kind, &segs));
            assert_eq!(s.part, "round");
            assert!(s.steps.iter().any(|st| st.a0 != st.a1), "{kind}");
            // The tip never moves while the part turns, and A ends where it began.
            assert!(s.steps.iter().filter(|st| st.a0 != st.a1).all(|st| st.from == st.to), "{kind}");
            assert_eq!(s.steps.last().unwrap().a1.rem_euclid(360.0), 0.0, "{kind}");
        }
        let flat = [[170.0, 40.0, 170.0, flat_y(170.0)], [230.0, 40.0, 230.0, flat_y(230.0)]];
        let s = script(&routine("flat", &flat));
        assert_eq!((s.part, s.rest_angle()), ("flat", FLAT_TILT));
        assert_eq!(s.steps.last().unwrap().a1, 0.0);
        // The tilted flat is higher on the Y+ side.
        assert!(flat_y(230.0) < flat_y(170.0));
        assert!(part_path("flat", 0.0).starts_with("M 151.0 115.0 A 70 70 0 1 0 249.0 115.0 Z"), "{}", part_path("flat", 0.0));
    }

    #[test]
    fn nothing_plays_without_moves() {
        let s = script(&routine("edge", &[]));
        assert!(s.is_empty() && s.at(100.0).is_none());
    }
}
