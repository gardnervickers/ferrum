//! Software renderer timings on a synthetic big program.
//! `cargo run --release -p preview --example bench -- [segments]`
use preview::{software::{Software, View}, Kind, Op, Seg, Toolpath};
use std::time::Instant;

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let mut segs = Vec::with_capacity(n);
    let mut p = [0.0, 0.0, 5.0, 0.0];
    for i in 0..n {
        let t = i as f64 * 0.05;
        let r = 5.0 + 45.0 * ((i % 4000) as f64 / 4000.0);
        let z = -((i / 4000) as f64) * 0.5;
        let q = [r * t.cos() + 60.0, r * t.sin() + 40.0, z, 0.0];
        segs.push(Seg { kind: if i % 4000 == 0 { Kind::Rapid } else { Kind::Feed }, from: p, to: q, op: (i * 4) / n, line: 0, rate: 1000.0 });
        p = q;
    }
    let ops = (0..4).map(|i| Op { name: format!("op{i}"), tool: i, ..Default::default() }).collect();
    let tp = Toolpath { segs, ops, error: None, frame: None, offsets: Vec::new(), machine_lines: Vec::new(), find_lines: Vec::new(), ..Default::default() };
    let trail: Vec<_> = tp.segs.iter().take(2000).map(|s| s.to).collect();
    let (w, h) = (1300u32, 760u32);
    let mut r = Software::new();
    for v in [View::Iso, View::Top, View::Front] {
        let t = Instant::now();
        r.render(&tp, 1, v, w, h, None, &trail, Some(p));
        let base = t.elapsed();
        let t = Instant::now();
        for _ in 0..20 { r.render(&tp, 1, v, w, h, None, &trail, Some(p)); }
        println!("{n} segs {v:?}: base {:.0} ms, frame {:.1} ms", base.as_secs_f64() * 1e3, t.elapsed().as_secs_f64() * 50.0);
    }
}
