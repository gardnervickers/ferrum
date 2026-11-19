//! Parse `rs274 -g` output (one canonical call per line) into a toolpath.
//!
//! ```text
//!    21 N..... STRAIGHT_TRAVERSE(-137.4588, 36.7976, 319.6559, 15.4098, 0.0000, 0.0000)
//!    40 N..... ARC_FEED(10.0000, 0.0000, 0.0000, 0.0000, 1, -1.0000, 0.0000, 0.0000, 0.0000)
//! ```
//! Positions are in the active work coordinate system, without the tool
//! length offset, matching the DRO's work position.
//!
//! The interpreter writes lengths in the program's units of the moment
//! (`USE_LENGTH_UNITS`, from G20/G21); they are converted to the machine's
//! units, which the DRO and the soft limits use.

use crate::{Builder, Kind, Toolpath, P4};

#[derive(Clone, Copy)]
pub(crate) enum Plane {
    Xy,
    Yz,
    Xz,
}

fn args(s: &str) -> Vec<f64> {
    s.split(|c| c == ',' || c == ' ').filter(|w| !w.is_empty()).filter_map(|w| w.parse().ok()).collect()
}

/// `rapid_per_s` (machine units per second) is used for time estimates only.
/// `machine_inch`: the machine's linear units are inches.
pub fn parse(text: &str, rapid_per_s: f64, machine_inch: bool) -> Toolpath {
    let mut b = Builder::new([0.0; 4], rapid_per_s);
    // Program length unit -> machine unit: multiply by 25.4, divide by
    // 25.4 (exact for inch values written in mm), or keep. The stand-alone
    // interpreter starts in millimetres.
    let to_machine = |prog_inch: bool| match (prog_inch, machine_inch) {
        (true, false) => 1i8,
        (false, true) => -1,
        _ => 0,
    };
    let mut way = to_machine(false);
    let k = |w: i8, v: f64| match w {
        1 => v * 25.4,
        -1 => v / 25.4,
        _ => v,
    };
    let mut plane = Plane::Xy;
    let mut started = false;
    let mut selected: Option<i32> = None;
    for line in text.lines() {
        // The sequence number, then the N-word column: "N....." or the
        // block's N number (rs274::run numbers each line with its own line
        // number, so this is the source line). A long number runs into the
        // call name.
        let rest = line.trim_start().trim_start_matches(|c: char| c.is_ascii_digit()).trim_start();
        let Some(rest) = rest.strip_prefix('N') else { continue };
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if let Ok(n) = rest[..digits].parse::<u32>() {
            b.set_line(n);
        }
        let call = rest[digits..].trim_start_matches('.').trim();
        let Some((name, rest)) = call.split_once('(') else { continue };
        let inner = rest.strip_suffix(')').unwrap_or(rest);
        match name {
            "USE_LENGTH_UNITS" => way = to_machine(inner.contains("INCHES")),
            "COMMENT" => b.comment(inner.trim_matches('"')),
            // SELECT_TOOL carries the T number. CHANGE_TOOL's argument is
            // the tool's place in the stand-alone interpreter's table, which
            // is a different number once the table skips one (T23 came out
            // as 22), so the selected number is the one to show.
            "FLOOD_ON" | "FLOOD_OFF" | "MIST_ON" | "MIST_OFF" => b.io_wait(),
            "SELECT_TOOL" => {
                b.io_wait();
                selected = args(inner).first().map(|t| *t as i32)
            }
            "CHANGE_TOOL" => {
                b.io_wait();
                if let Some(t) = selected.or_else(|| args(inner).first().map(|t| *t as i32)) {
                    b.tool(t);
                }
            }
            "SET_G5X_OFFSET" => {
                let a = args(inner);
                if a.len() >= 4 {
                    let mut o = b.offsets();
                    o.wcs = a[0].clamp(0.0, 9.0) as u8;
                    o.g5x = [k(way, a[1]), k(way, a[2]), k(way, a[3])];
                    b.set_offsets(o);
                }
            }
            "SET_G92_OFFSET" => {
                let a = args(inner);
                if a.len() >= 3 {
                    let mut o = b.offsets();
                    o.g92 = [k(way, a[0]), k(way, a[1]), k(way, a[2])];
                    b.set_offsets(o);
                }
            }
            "USE_TOOL_LENGTH_OFFSET" => {
                let a = args(inner);
                if a.len() >= 3 {
                    let mut o = b.offsets();
                    o.tool = [k(way, a[0]), k(way, a[1]), k(way, a[2])];
                    b.set_offsets(o);
                }
            }
            "SET_XY_ROTATION" => {
                if let Some(r) = args(inner).first() {
                    let mut o = b.offsets();
                    o.rotation = *r;
                    b.set_offsets(o);
                }
            }
            "DWELL" => b.dwell(args(inner).first().copied().unwrap_or(0.0)),
            // G64 [P..] blends (0: no tolerance); G61 and G61.1 stop at corners.
            "SET_MOTION_CONTROL_MODE" => {
                b.set_blend(if inner.contains("CONTINUOUS") {
                    args(inner).first().map_or(0.0, |t| k(way, *t))
                } else if inner.contains("EXACT_STOP") {
                    -2.0
                } else {
                    -1.0
                })
            }
            // Calls that change no motion and leave the moves before them
            // running (everything else waits for motion to finish).
            "SET_FEED_REFERENCE" | "ON_RESET" | "SET_NAIVECAM_TOLERANCE" | "SET_FEED_MODE" | "SET_TRAVERSE_RATE" | "ENABLE_FEED_OVERRIDE"
            | "ENABLE_SPEED_OVERRIDE" | "ENABLE_ADAPTIVE_FEED" | "ENABLE_FEED_HOLD" | "DISABLE_FEED_OVERRIDE" | "DISABLE_SPEED_OVERRIDE"
            | "DISABLE_ADAPTIVE_FEED" | "DISABLE_FEED_HOLD" | "SET_SPINDLE_MODE" | "START_CHANGE" | "SET_CUTTER_RADIUS_COMPENSATION"
            | "START_CUTTER_RADIUS_COMPENSATION" | "STOP_CUTTER_RADIUS_COMPENSATION" => {}
            "SET_FEED_RATE" => {
                if let Some(f) = args(inner).first() {
                    b.feed_rate(k(way, *f));
                }
            }
            "SELECT_PLANE" => {
                plane = if inner.contains("YZ") {
                    Plane::Yz
                } else if inner.contains("XZ") {
                    Plane::Xz
                } else {
                    Plane::Xy
                }
            }
            "STRAIGHT_TRAVERSE" | "STRAIGHT_FEED" | "STRAIGHT_PROBE" | "RIGID_TAP" => {
                let a = args(inner);
                if a.len() < 3 {
                    continue;
                }
                let p: P4 = [k(way, a[0]), k(way, a[1]), k(way, a[2]), a.get(3).copied().unwrap_or(0.0)];
                // The interpreter starts at an arbitrary point; begin at the first move.
                if !started {
                    started = true;
                    b.set_position(p);
                    continue;
                }
                let kind = if name == "STRAIGHT_TRAVERSE" { Kind::Rapid } else { Kind::Feed };
                // Probing and tapping start and end at rest.
                let alone = name == "STRAIGHT_PROBE" || name == "RIGID_TAP";
                if alone {
                    b.stop();
                }
                b.line(kind, p);
                if alone {
                    b.stop();
                }
                if name == "RIGID_TAP" {
                    // The tap returns to its start.
                }
            }
            "ARC_FEED" => {
                // first_end, second_end, first_axis, second_axis, rotation, axis_end, a, ...
                let a = args(inner);
                if a.len() < 6 || !started {
                    continue;
                }
                let (fe, se, fc, sc, rot, ae) = (k(way, a[0]), k(way, a[1]), k(way, a[2]), k(way, a[3]), a[4] as i32, k(way, a[5]));
                let aa = a.get(6).copied().unwrap_or(b.position()[3]);
                arc(&mut b, plane, fe, se, fc, sc, rot, ae, aa);
            }
            _ => b.stop(),
        }
    }
    b.finish()
}

/// Map a plane's (first, second, axis) to X Y Z, run the arc in XY space,
/// and map back. XZ in canon order is (Z, X), YZ is (Y, Z).
#[allow(clippy::too_many_arguments)]
pub(crate) fn arc(b: &mut Builder, plane: Plane, fe: f64, se: f64, fc: f64, sc: f64, rot: i32, ae: f64, a: f64) {
    let to_plane = |p: P4| -> P4 {
        match plane {
            Plane::Xy => p,
            Plane::Xz => [p[2], p[0], p[1], p[3]],
            Plane::Yz => [p[1], p[2], p[0], p[3]],
        }
    };
    let from_plane = |q: P4| -> P4 {
        match plane {
            Plane::Xy => q,
            Plane::Xz => [q[1], q[2], q[0], q[3]],
            Plane::Yz => [q[2], q[0], q[1], q[3]],
        }
    };
    let start = b.position();
    let q = to_plane(start);
    let mut sub = Builder::new(q, 1.0);
    sub.arc_xy([fe, se, ae, a], (fc, sc), rot);
    b.begin_arc(((q[0] - fc).powi(2) + (q[1] - sc).powi(2)).sqrt());
    for s in sub.finish().segs {
        b.line(Kind::Feed, from_plane(s.to));
    }
    b.end_arc();
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = r#"    1 N..... USE_LENGTH_UNITS(CANON_UNITS_MM)
    7 N..... COMMENT("FACE1")
    8 N..... CHANGE_TOOL(14)
    9 N..... STRAIGHT_TRAVERSE(-137.4588, 36.7976, 319.6559, 0.0000, 0.0000, 0.0000)
   10 N..... STRAIGHT_TRAVERSE(0.0000, 0.0000, 5.0000, 0.0000, 0.0000, 0.0000)
   11 N..... SET_FEED_RATE(600.0000)
   12 N..... STRAIGHT_FEED(100.0000, 0.0000, 5.0000, 0.0000, 0.0000, 0.0000)
   13 N..... ARC_FEED(100.0000, 20.0000, 100.0000, 10.0000, 1, 5.0000, 0.0000, 0.0000, 0.0000)
   14 N..... SELECT_PLANE(CANON_PLANE_XZ)
   15 N..... ARC_FEED(5.0000, 80.0000, 5.0000, 90.0000, -1, 20.0000, 0.0000, 0.0000, 0.0000)
"#;

    #[test]
    fn source_lines_come_from_the_n_column() {
        let out = "    8 N3     STRAIGHT_TRAVERSE(1.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000)
    9 N123456SET_FEED_RATE(100.0000)
   10 N123456STRAIGHT_FEED(5.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000)
   11 N..... STRAIGHT_FEED(5.0000, 3.0000, 0.0000, 0.0000, 0.0000, 0.0000)
";
        let tp = parse(out, 100.0, false);
        // A subroutine's own lines carry no number: they stay on the call.
        assert_eq!(tp.segs.iter().map(|s| s.line).collect::<Vec<_>>(), [123456, 123456]);
    }

    #[test]
    fn lengths_come_out_in_machine_units() {
        // rs274 on an inch INI: the parameter file's G54 (inches) is
        // reported in mm at the start, then the program's G20 switches.
        let out = "    1 N..... USE_LENGTH_UNITS(CANON_UNITS_MM)
    2 N..... SET_G5X_OFFSET(1, 25.4000, 50.8000, 12.7000, 0.0000, 0.0000, 0.0000)
    7 N..... USE_LENGTH_UNITS(CANON_UNITS_INCHES)
    8 N..... STRAIGHT_TRAVERSE(1.0000, 1.0000, 0.0000, 0.0000, 0.0000, 0.0000)
    9 N..... SET_FEED_RATE(10.0000)
   10 N..... STRAIGHT_FEED(1.0000, 2.0000, -0.1000, 90.0000, 0.0000, 0.0000)
";
        let tp = parse(out, 4.0, true);
        assert_eq!(tp.offsets[0].1.g5x, [1.0, 2.0, 0.5]);
        // A turns are cut into chords: the last one ends at the target.
        assert_eq!(tp.segs.last().unwrap().to, [1.0, 2.0, -0.1, 90.0]);
        // The same G20 program on a metric machine is scaled up.
        let tp = parse(out, 4.0, false);
        assert_eq!(tp.offsets[0].1.g5x, [25.4, 50.8, 12.7]);
        let end = tp.segs.last().unwrap().to;
        assert!((end[1] - 50.8).abs() < 1e-9 && end[3] == 90.0);
    }

    #[test]
    fn records_the_offsets_in_effect() {
        let out = "    1 N..... SET_G5X_OFFSET(1, 215.0000, -90.0000, -200.0000, 0.0000, 0.0000, 0.0000)
    2 N..... SET_G92_OFFSET(0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000)
    3 N..... SET_XY_ROTATION(0.0000)
    4 N..... STRAIGHT_TRAVERSE(0.0000, 0.0000, 10.0000, 0.0000, 0.0000, 0.0000)
    5 N..... STRAIGHT_FEED(10.0000, 0.0000, 10.0000, 0.0000, 0.0000, 0.0000)
    6 N..... USE_TOOL_LENGTH_OFFSET(0.0000 0.0000 98.7941, 0.0000 0.0000 0.0000, 0.0000 0.0000 0.0000)
    7 N..... STRAIGHT_FEED(10.0000, 5.0000, 10.0000, 0.0000, 0.0000, 0.0000)
";
        let tp = parse(out, 100.0, false);
        assert_eq!(tp.offsets.len(), 2);
        assert_eq!(tp.offsets[0].0, 0);
        assert_eq!(tp.offsets[0].1.wcs, 1);
        assert_eq!(tp.offsets[0].1.g5x, [215.0, -90.0, -200.0]);
        assert_eq!(tp.offsets[1].0, 1);
        assert_eq!(tp.offsets[1].1.tool, [0.0, 0.0, 98.7941]);
    }

    #[test]
    fn parses_moves_arcs_and_ops() {
        let tp = parse(OUT, 100.0, false);
        assert_eq!(tp.ops.len(), 1);
        assert_eq!((tp.ops[0].name.as_str(), tp.ops[0].tool), ("FACE1", 14));
        assert_eq!(tp.segs[0].kind, Kind::Rapid);
        assert_eq!(tp.segs[0].to, [0.0, 0.0, 5.0, 0.0]);
        // The XY half circle ends at (100, 20) and bulges to X 110.
        let (_, hi) = tp.bounds().unwrap();
        assert!((hi[0] - 110.0).abs() < 0.01, "{hi:?}");
        // The XZ arc ends at X 80, Z 5.
        let last = tp.segs.last().unwrap().to;
        assert!((last[0] - 80.0).abs() < 1e-6 && (last[2] - 5.0).abs() < 1e-6, "{last:?}");
    }

    #[test]
    fn the_tool_is_the_selected_t_number_not_its_table_index() {
        // T23 sits 22nd in a table that has no T15: rs274 prints both.
        let out = "    1 N..... SELECT_TOOL(23)\n    2 N..... CHANGE_TOOL(22)\n    3 N..... STRAIGHT_TRAVERSE(0.0000, 0.0000, 5.0000, 0.0000, 0.0000, 0.0000)\n    4 N..... SET_FEED_RATE(600.0000)\n    5 N..... STRAIGHT_FEED(10.0000, 0.0000, 5.0000, 0.0000, 0.0000, 0.0000)\n";
        assert_eq!(parse(out, 100.0, false).ops[0].tool, 23);
    }
}
