//! A small G-code reader for development without LinuxCNC (the Mac and
//! the simulated machine): G0/G1/G2/G3 in G17/G18/G19 with I/J/K or R and
//! P turns, G81-G89 drilling (as straight plunges), G90/G91, F, T/M6 and
//! comments. No subroutines, offsets or canned
//! cycles; the Pi uses rs274.

use crate::canon::{arc, Plane};
use crate::{Builder, Kind, Toolpath, P4};

/// Center of an R-format arc in the plane (first, second coordinates).
fn r_center(s: (f64, f64), e: (f64, f64), r: f64, ccw: bool) -> (f64, f64) {
    let (dx, dy) = (e.0 - s.0, e.1 - s.1);
    let d = (dx * dx + dy * dy).sqrt();
    if d < 1e-9 {
        return s;
    }
    let h = (r * r - d * d / 4.0).max(0.0).sqrt();
    let (mx, my) = (s.0 + dx / 2.0, s.1 + dy / 2.0);
    // Left of the chord for a short CCW arc; negative R takes the long way.
    let side = if ccw == (r > 0.0) { 1.0 } else { -1.0 };
    (mx - side * h * dy / d, my + side * h * dx / d)
}

pub fn parse(text: &str, rapid_mm_s: f64) -> Toolpath {
    let mut b = Builder::new([0.0; 4], rapid_mm_s);
    let mut absolute = true;
    let mut motion = 0;
    let mut plane = Plane::Xy;
    // Canned drilling (G81-G89): R plane and bottom Z, retract to R.
    let (mut cr, mut cz) = (0.0, 0.0);
    // G98 retracts to the Z the cycle started at (if above R), G99 to R.
    let (mut g98, mut start_z) = (true, None::<f64>);
    let mut started = false;
    for (n, raw) in text.lines().enumerate() {
        b.set_line(n as u32 + 1);
        let mut code = String::new();
        let mut rest = raw;
        while let Some(i) = rest.find('(') {
            code.push_str(&rest[..i]);
            let end = rest[i..].find(')').map_or(rest.len(), |j| i + j + 1);
            b.comment(rest[i + 1..end.saturating_sub(1).max(i + 1)].trim());
            rest = &rest[end..];
        }
        code.push_str(rest.split(';').next().unwrap_or(""));
        let code = code.to_ascii_uppercase();
        let mut words: Vec<(char, f64)> = Vec::new();
        let mut chars = code.chars().filter(|c| !c.is_whitespace()).peekable();
        while let Some(c) = chars.next() {
            if !c.is_ascii_alphabetic() {
                continue;
            }
            let mut num = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() || d == '.' || d == '-' || d == '+' {
                    num.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            if let Ok(v) = num.parse() {
                words.push((c, v));
            }
        }
        let get = |l: char| words.iter().find(|w| w.0 == l).map(|w| w.1);
        let mut machine = false;
        for &(l, v) in &words {
            // G91.1/G90.1 set the arc-center mode, not the distance mode.
            if l == 'G' && v.fract() != 0.0 {
                continue;
            }
            match (l, v as i32) {
                ('G', 0..=3) => motion = v as i32,
                ('G', 81..=89) => motion = v as i32,
                ('G', 80) => {
                    motion = -1;
                    start_z = None;
                }
                ('G', 98) => g98 = true,
                ('G', 99) => g98 = false,
                ('G', 90) => absolute = true,
                ('G', 91) => absolute = false,
                ('G', 53) => machine = true,
                ('G', 17) => plane = Plane::Xy,
                ('G', 18) => plane = Plane::Xz,
                ('G', 19) => plane = Plane::Yz,
                ('F', _) => b.feed_rate(v),
                ('T', _) => b.tool(v as i32),
                _ => {}
            }
        }
        if machine || !"XYZA".chars().any(|l| get(l).is_some()) {
            continue;
        }
        let cur = b.position();
        let mut p: P4 = cur;
        for (i, l) in "XYZA".chars().enumerate() {
            if let Some(v) = get(l) {
                p[i] = if absolute { v } else { cur[i] + v };
            }
        }
        if !started {
            started = true;
            b.set_position(p);
            continue;
        }
        if (81..=89).contains(&motion) {
            cr = get('R').unwrap_or(cr);
            cz = get('Z').unwrap_or(cz);
            // Holes at X Y: Z words are the bottom, not a move.
            let z0 = *start_z.get_or_insert(cur[2]);
            let at = [p[0], p[1], cur[2], p[3]];
            b.line(Kind::Rapid, at);
            b.line(Kind::Rapid, [at[0], at[1], cr, at[3]]);
            b.line(Kind::Feed, [at[0], at[1], cz, at[3]]);
            let up = if g98 { z0.max(cr) } else { cr };
            b.line(Kind::Rapid, [at[0], at[1], up, at[3]]);
            continue;
        }
        match motion {
            0 => b.line(Kind::Rapid, p),
            1 => b.line(Kind::Feed, p),
            2 | 3 => {
                let ccw = motion == 3;
                let turns = get('P').map_or(1, |v| (v as i32).max(1));
                let rot = if ccw { turns } else { -turns };
                // Canon order: XY is (X, Y) about Z, XZ is (Z, X) about Y,
                // YZ is (Y, Z) about X; offsets I J K go with X Y Z.
                let (f, s, ax) = match plane {
                    Plane::Xy => (0, 1, 2),
                    Plane::Xz => (2, 0, 1),
                    Plane::Yz => (1, 2, 0),
                };
                let off = |i: usize| get(['I', 'J', 'K'][i]).unwrap_or(0.0);
                let c = match get('R') {
                    Some(r) => r_center((cur[f], cur[s]), (p[f], p[s]), r, ccw),
                    None => (cur[f] + off(f), cur[s] + off(s)),
                };
                arc(&mut b, plane, p[f], p[s], c.0, c.1, rot, p[ax], p[3]);
            }
            _ => {}
        }
    }
    b.finish()
}

#[cfg(test)]
mod tests {
    #[test]
    fn arcs_in_other_planes_stay_in_them() {
        // G18 ramp: X-27.5 Z2 to X-25 Z-0.5 about X-25 Z2 (a quarter in XZ).
        let tp = super::parse("G0 X-27.5 Y-5 Z2\nG18 G2 X-25 Z-0.5 I2.5\nG17 G1 X0\n", 100.0);
        let ys: Vec<f64> = tp.segs.iter().map(|s| s.to[1]).collect();
        assert!(ys.iter().all(|y| (y + 5.0).abs() < 1e-9), "{ys:?}");
        let zmin = tp.segs.iter().map(|s| s.to[2]).fold(f64::MAX, f64::min);
        assert!((zmin + 0.5).abs() < 1e-6);
        // R format quarter circle.
        let tp = super::parse("G0 X10 Y0 Z0\nG3 X0 Y10 R10\n", 100.0);
        let mid = &tp.segs[tp.segs.len() / 2].to;
        assert!((mid[0].hypot(mid[1]) - 10.0).abs() < 0.05, "{mid:?}");
    }

    #[test]
    fn g98_drilling_moves_between_holes_at_the_start_height() {
        let tp = super::parse("G0 X0 Y0 Z10\nG98 G81 X5 Y0 Z-3 R2 F100\nX10\nG80\n", 100.0);
        let tops: Vec<f64> = tp.segs.iter().filter(|s| s.from[0] != s.to[0]).map(|s| s.to[2]).collect();
        assert!(tops.iter().all(|z| *z == 10.0), "{tops:?}");
        let tp = super::parse("G0 X0 Y0 Z10\nG99 G81 X5 Y0 Z-3 R2 F100\nX10\nG80\n", 100.0);
        let last_xy = tp.segs.iter().filter(|s| s.from[0] != s.to[0]).last().unwrap();
        assert_eq!(last_xy.to[2], 2.0);
    }

    #[test]
    fn g91_1_is_not_incremental() {
        let tp = super::parse("G90 G91.1\nG0 X0 Y0 Z5\nG1 X10 F100\nX20\nX20 Y10\n", 100.0);
        let last = tp.segs.last().unwrap().to;
        assert_eq!((last[0], last[1]), (20.0, 10.0));
    }

    #[test]
    fn job_markers_define_the_ops() {
        let src = "(MYMILL-OP 1/2 ID=A FACE 100 X 60 T14)\nT14 M6\nG0 X0 Y0 Z5\nG1 Z0 F600\nX10\n\
                   (MYMILL-OP 2/2 ID=B FLIP PART AND RE-CLAMP)\n(MSG, Stopped before op 2: FLIP)\nM0\n";
        let tp = super::parse(src, 100.0);
        let names: Vec<_> = tp.ops.iter().map(|o| (o.name.as_str(), o.tool)).collect();
        assert_eq!(names, [("FACE 100 X 60 T14", 14), ("FLIP PART AND RE-CLAMP", 0)]);
    }

    #[test]
    fn reads_a_simple_program() {
        let tp = super::parse("(FACE1)\nT14 M6\nG0 X0 Y0 Z5\nG1 Z0 F600\nX100\nG3 X100 Y20 I0 J10\nG0 Z15\n", 100.0);
        assert_eq!(tp.ops[0].name, "FACE1");
        assert_eq!(tp.ops[0].tool, 14);
        assert!(tp.segs.len() > 10);
    }
}
