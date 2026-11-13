//! A small G-code reader for development without LinuxCNC (the Mac and
//! the simulated machine): G0/G1/G2/G3 in XY with I/J, G90/G91, F, T/M6 and
//! comments. No subroutines, offsets or canned cycles; the Pi uses rs274.

use crate::{Builder, Kind, Toolpath, P4};

pub fn parse(text: &str, rapid_mm_s: f64) -> Toolpath {
    let mut b = Builder::new([0.0; 4], rapid_mm_s);
    let mut absolute = true;
    let mut motion = 0;
    let mut started = false;
    for raw in text.lines() {
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
            match (l, v as i32) {
                ('G', 0..=3) if v.fract() == 0.0 => motion = v as i32,
                ('G', 90) => absolute = true,
                ('G', 91) => absolute = false,
                ('G', 53) => machine = true,
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
        match motion {
            0 => b.line(Kind::Rapid, p),
            1 => b.line(Kind::Feed, p),
            2 | 3 => {
                let c = (cur[0] + get('I').unwrap_or(0.0), cur[1] + get('J').unwrap_or(0.0));
                b.arc_xy(p, c, if motion == 3 { 1 } else { -1 });
            }
            _ => {}
        }
    }
    b.finish()
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_a_simple_program() {
        let tp = super::parse("(FACE1)\nT14 M6\nG0 X0 Y0 Z5\nG1 Z0 F600\nX100\nG3 X100 Y20 I0 J10\nG0 Z15\n", 100.0);
        assert_eq!(tp.ops[0].name, "FACE1");
        assert_eq!(tp.ops[0].tool, 14);
        assert!(tp.segs.len() > 10);
    }
}
