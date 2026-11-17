//! Travel check before a program runs: would any move take an axis past
//! its soft limit, or the tool tip into the table?
//!
//! The toolpath holds work positions. The machine position is the one
//! LinuxCNC's canon computes (emccanon.cc `rotate_and_offset_pos`):
//! `rotate(work + G92) + G5x + tool length offset`, which is where the
//! spindle nose (the controlled point) goes. Soft limits apply to that.
//! The tip is the same without the tool offset.
//!
//! Work offsets are taken from the machine as it is now, not from the
//! interpreter's parameter file (LinuxCNC saves that file only at exit, and
//! the operator may probe after loading). A program that sets an offset
//! itself (G10, a probing op) is still checked with today's value, and the
//! report says where it does that. After a job's probing step that finds
//! the offset, moves are not checked at all (the offset is not known until
//! the step has run); the report names that line too. The tool length is the one the
//! interpreter applied from the tool table when the program was read.

use crate::Toolpath;

/// [AXIS_X/Y/Z] MIN_LIMIT and MAX_LIMIT, machine coordinates (mm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

/// The offsets in effect on the machine now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Live {
    /// G54..G59.3: X Y Z.
    pub wcs: [[f64; 3]; 9],
    /// XY rotation per work system, degrees.
    pub rotation: [f64; 9],
    pub g92: [f64; 3],
    /// The active work system (1..9), for moves made before the program
    /// names one.
    pub active: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub enum What {
    /// Axis 0..2 would pass its limit: `high` the MAX side.
    Travel { axis: usize, high: bool },
    /// The tool tip would go below the table top.
    Table,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub what: What,
    /// The furthest past, mm.
    pub by: f64,
    /// The first program line that goes past (1-based; 0 unknown).
    pub line: u32,
    /// The op of that move (index into `Toolpath::ops`).
    pub op: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Worst first: travel before the table, then by distance.
    pub problems: Vec<Problem>,
    /// Lines where the program sets a work offset or G92 itself; checked
    /// with today's value there.
    pub sets_offsets: Vec<u32>,
    /// Moves checked.
    pub moves: usize,
}

impl Report {
    pub fn travel(&self) -> impl Iterator<Item = &Problem> {
        self.problems.iter().filter(|p| matches!(p.what, What::Travel { .. }))
    }
}

/// Small enough to ignore: rounding in the canon output (4 places).
const SLACK: f64 = 0.0005;

/// Check the moves from program line `from_line` on (0 or 1: all of them).
/// `table_top`: machine Z the tool tip would touch the table at (the
/// spindle nose height above the table at Z 0, negated); None skips it.
pub fn check(tp: &Toolpath, limits: &Limits, live: &Live, table_top: Option<f64>, from_line: u32) -> Report {
    let mut report = Report::default();
    // (worst, first line, op) per axis side, then the table.
    let mut worst: [Option<(f64, u32, usize)>; 7] = [None; 7];
    let mut seen_g5x: [Option<[f64; 3]>; 10] = [None; 10];
    let mut seen_g92: Option<[f64; 3]> = None;
    let mut next = 0;
    let mut cur = crate::Offsets { wcs: live.active, ..Default::default() };
    // The first FIND probing step this run reaches.
    let found = tp.find_lines.iter().copied().find(|l| *l >= from_line);
    // Whether the move before this one was checked, and was a G53 move.
    let (mut prev_counted, mut prev_machine) = (false, false);
    for (i, seg) in tp.segs.iter().enumerate() {
        let mut shifted = false;
        while next < tp.offsets.len() && tp.offsets[next].0 <= i {
            let o = tp.offsets[next].1;
            let w = o.wcs as usize;
            let changed_g5x = seen_g5x[w].is_some_and(|v| differs(v, o.g5x));
            let changed_g92 = seen_g92.is_some_and(|v| differs(v, o.g92));
            if changed_g5x || changed_g92 {
                if report.sets_offsets.last() != Some(&seg.line) {
                    report.sets_offsets.push(seg.line);
                }
            }
            seen_g5x[w].get_or_insert(o.g5x);
            seen_g92.get_or_insert(o.g92);
            cur = o;
            shifted = true;
            next += 1;
        }
        let machine = seg.line != 0 && tp.machine_lines.binary_search(&seg.line).is_ok();
        if seg.line != 0 && seg.line < from_line {
            (prev_counted, prev_machine) = (false, machine);
            continue;
        }
        // After a probing step that finds the work offset, where the moves
        // land is not known until it has run. Its own G53 moves are.
        if let Some(find) = found.filter(|f| seg.line >= *f && !machine) {
            if !report.sets_offsets.contains(&find) {
                report.sets_offsets.push(find);
            }
            (prev_counted, prev_machine) = (false, machine);
            continue;
        }
        report.moves += 1;
        let w = if (1..=9).contains(&cur.wcs) { cur.wcs } else { live.active.clamp(1, 9) } as usize - 1;
        // A G53 move goes to a machine position whatever the offsets are:
        // the interpreter's own offsets turn its work position back into
        // that. Every other move lands where today's offsets put it.
        let (g5x, rot, g92) = if machine { (cur.g5x, cur.rotation, cur.g92) } else { (live.wcs[w], live.rotation[w], live.g92) };
        let (sin, cos) = rot.to_radians().sin_cos();
        // A straight move is checked at its end; arcs arrive as short
        // lines. Its start is the end of the move before, so it is only
        // checked for the first move of a run from a line. Not when the
        // start is unknown or was shifted without a move: the first move
        // of the program (the interpreter starts at an arbitrary point),
        // one after an offset change (G43 moves the work position, not
        // the machine) or after a G53 move.
        let start = !prev_counted && i > 0 && !shifted && !prev_machine && !machine;
        (prev_counted, prev_machine) = (true, machine);
        for p in start.then_some(seg.from).into_iter().chain([seg.to]) {
            let (x, y) = (p[0] + g92[0], p[1] + g92[1]);
            let tip = [x * cos - y * sin + g5x[0], x * sin + y * cos + g5x[1], p[2] + g92[2] + g5x[2]];
            let nose = [tip[0] + cur.tool[0], tip[1] + cur.tool[1], tip[2] + cur.tool[2]];
            for a in 0..3 {
                for (k, by) in [(a * 2, limits.min[a] - nose[a]), (a * 2 + 1, nose[a] - limits.max[a])] {
                    note(&mut worst[k], by, seg.line, seg.op);
                }
            }
            if let Some(top) = table_top {
                note(&mut worst[6], top - tip[2], seg.line, seg.op);
            }
        }
    }
    for (k, w) in worst.iter().enumerate() {
        let Some((by, line, op)) = *w else { continue };
        let what = if k == 6 { What::Table } else { What::Travel { axis: k / 2, high: k % 2 == 1 } };
        report.problems.push(Problem { what, by, line, op });
    }
    report.problems.sort_by(|a, b| {
        let rank = |p: &Problem| matches!(p.what, What::Table) as u8;
        rank(a).cmp(&rank(b)).then(b.by.total_cmp(&a.by))
    });
    report
}

fn note(slot: &mut Option<(f64, u32, usize)>, by: f64, line: u32, op: usize) {
    if by <= SLACK {
        return;
    }
    match slot {
        Some((worst, _, _)) => *worst = worst.max(by),
        None => *slot = Some((by, line, op)),
    }
}

fn differs(a: [f64; 3], b: [f64; 3]) -> bool {
    (0..3).any(|i| (a[i] - b[i]).abs() > 1e-4)
}

/// One line for an operator: "X goes 12.300 mm past its + limit at line 230".
pub fn describe(p: &Problem) -> String {
    match p.what {
        What::Travel { axis, high } => {
            let name = ["X", "Y", "Z"][axis];
            format!("{name} goes {:.3} mm past its {} limit{}", p.by, if high { "+" } else { "−" }, at(p.line))
        }
        What::Table => format!("The tool tip goes {:.3} mm below the table top{}", p.by, at(p.line)),
    }
}

fn at(line: u32) -> String {
    if line > 0 { format!(" at line {line}") } else { String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Builder, Kind, Offsets};

    const LIMITS: Limits = Limits { min: [0.0, -180.0, -350.0], max: [430.0, 0.0, 0.0] };

    fn live() -> Live {
        let mut l = Live { active: 1, ..Default::default() };
        l.wcs[0] = [200.0, -90.0, -250.0];
        l
    }

    fn path(moves: &[(u32, [f64; 4])], offsets: &[(usize, Offsets)]) -> Toolpath {
        let mut b = Builder::new([0.0, 0.0, 50.0, 0.0], 100.0);
        b.feed_rate(500.0);
        let mut at = 0;
        for (i, (line, p)) in moves.iter().enumerate() {
            while at < offsets.len() && offsets[at].0 <= i {
                b.set_offsets(offsets[at].1);
                at += 1;
            }
            b.set_line(*line);
            b.line(Kind::Feed, *p);
        }
        b.finish()
    }

    fn g54(tool: f64) -> Offsets {
        // The parameter file's G54, which the check replaces by today's.
        Offsets { wcs: 1, g5x: [1.0, 2.0, 3.0], tool: [0.0, 0.0, tool], ..Default::default() }
    }

    #[test]
    fn inside_travel_is_clean() {
        let tp = path(&[(10, [100.0, -50.0, 50.0, 0.0]), (11, [-100.0, 50.0, -5.0, 0.0])], &[(0, g54(100.0))]);
        let r = check(&tp, &LIMITS, &live(), Some(-380.0), 0);
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        assert_eq!(r.moves, 2);
    }

    #[test]
    fn x_past_max_names_the_first_line_and_the_worst_distance() {
        // G54 X 200: work X 240 is machine 440, 10 past 430.
        let tp = path(&[(10, [240.0, 0.0, 10.0, 0.0]), (11, [245.0, 0.0, 10.0, 0.0]), (12, [0.0, 0.0, 10.0, 0.0])], &[(0, g54(0.0))]);
        let r = check(&tp, &LIMITS, &live(), None, 0);
        assert_eq!(r.problems.len(), 1);
        let p = &r.problems[0];
        assert_eq!(p.what, What::Travel { axis: 0, high: true });
        assert_eq!(p.line, 10);
        assert!((p.by - 15.0).abs() < 1e-9);
        assert_eq!(describe(p), "X goes 15.000 mm past its + limit at line 10");
    }

    #[test]
    fn z_top_uses_the_tool_length_and_the_table_uses_the_tip() {
        // Nose = tip + 100. Work Z 160 -> tip machine -90 -> nose +10.
        let tp = path(&[(5, [0.0, 0.0, 160.0, 0.0]), (6, [0.0, 0.0, -140.0, 0.0])], &[(0, g54(100.0))]);
        let r = check(&tp, &LIMITS, &live(), Some(-380.0), 0);
        assert_eq!(r.problems[0].what, What::Travel { axis: 2, high: true });
        assert!((r.problems[0].by - 10.0).abs() < 1e-9);
        // Tip at work -140 is machine -390, 10 below the table top.
        assert_eq!(r.problems[1].what, What::Table);
        assert!((r.problems[1].by - 10.0).abs() < 1e-9);
        assert_eq!(r.problems[1].line, 6);
    }

    #[test]
    fn todays_offsets_win_over_the_parameter_file() {
        let mut l = live();
        l.wcs[0] = [0.0, -90.0, -250.0];
        // With the file's G54 (X 1) this would be fine; today's X 0 is not.
        let tp = path(&[(3, [-1.0, 0.0, 10.0, 0.0])], &[(0, g54(0.0))]);
        let r = check(&tp, &LIMITS, &l, None, 0);
        assert_eq!(r.problems[0].what, What::Travel { axis: 0, high: false });
    }

    #[test]
    fn programs_that_set_offsets_are_named() {
        let mut moved = g54(0.0);
        moved.g5x = [5.0, 2.0, 3.0];
        let tp = path(&[(3, [10.0, 0.0, 10.0, 0.0]), (40, [20.0, 0.0, 10.0, 0.0])], &[(0, g54(0.0)), (1, moved)]);
        let r = check(&tp, &LIMITS, &live(), None, 0);
        assert_eq!(r.sets_offsets, vec![40]);
        assert!(r.problems.is_empty());
    }

    #[test]
    fn from_line_skips_earlier_moves() {
        let tp = path(&[(10, [240.0, 0.0, 10.0, 0.0]), (20, [0.0, 0.0, 10.0, 0.0]), (30, [10.0, 0.0, 10.0, 0.0])], &[(0, g54(0.0))]);
        assert_eq!(check(&tp, &LIMITS, &live(), None, 21).problems, vec![]);
        // The move into line 20 starts past the limit.
        assert_eq!(check(&tp, &LIMITS, &live(), None, 20).problems.len(), 1);
    }

    #[test]
    fn a_tool_offset_change_does_not_move_the_machine() {
        // G53 G0 Z0 with no offset (work Z 250 = machine 0), then G43 with
        // a 100 mm tool and a move down: the nose never passes Z 0.
        let tp = path(&[(18, [0.0, 0.0, 250.0, 0.0]), (48, [10.0, 0.0, 150.0, 0.0]), (50, [10.0, 0.0, 20.0, 0.0])], &[(0, g54(0.0)), (1, g54(100.0))]);
        let r = check(&tp, &LIMITS, &live(), None, 0);
        assert!(r.problems.is_empty(), "{:?}", r.problems);
    }

    #[test]
    fn g53_moves_use_the_offsets_they_were_read_with() {
        // The parameter file's G54 Z is -300; today's is -250. "G53 G0 Z0"
        // arrives as work Z 300 and is machine Z 0 either way.
        let mut file = g54(0.0);
        file.g5x = [200.0, -90.0, -300.0];
        let mut tp = path(&[(7, [0.0, 0.0, 300.0, 0.0]), (9, [10.0, 0.0, 10.0, 0.0])], &[(0, file)]);
        assert_eq!(check(&tp, &LIMITS, &live(), None, 0).problems.len(), 1, "without the mark it reads as past Z+");
        tp.machine_lines = vec![7];
        let r = check(&tp, &LIMITS, &live(), None, 0);
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        // A G53 move past a limit is still caught.
        let mut tp = path(&[(7, [0.0, 0.0, 302.0, 0.0])], &[(0, file)]);
        tp.machine_lines = vec![7];
        assert_eq!(check(&tp, &LIMITS, &live(), None, 0).problems[0].what, What::Travel { axis: 2, high: true });
    }

    #[test]
    fn moves_after_a_find_step_are_not_judged_with_todays_offset() {
        // Today's G54 puts line 30 past X+; the FIND at line 20 will move it.
        let mut tp = path(&[(10, [10.0, 0.0, 10.0, 0.0]), (30, [240.0, 0.0, 10.0, 0.0])], &[(0, g54(0.0))]);
        assert_eq!(check(&tp, &LIMITS, &live(), None, 0).problems.len(), 1);
        tp.find_lines = vec![20];
        let r = check(&tp, &LIMITS, &live(), None, 0);
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        assert_eq!(r.sets_offsets, vec![20]);
        assert_eq!(r.moves, 1);
        // Started after the find (the offset is set by then): checked again.
        assert_eq!(check(&tp, &LIMITS, &live(), None, 25).problems.len(), 1);
    }

    #[test]
    fn rotation_turns_about_the_work_origin() {
        let mut l = live();
        l.rotation[0] = 90.0;
        // Work X 100 turned 90° is work Y 100: machine Y +10, past 0.
        let tp = path(&[(7, [100.0, 0.0, 10.0, 0.0])], &[(0, g54(0.0))]);
        let r = check(&tp, &LIMITS, &l, None, 0);
        assert_eq!(r.problems[0].what, What::Travel { axis: 1, high: true });
    }
}
