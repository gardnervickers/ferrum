//! Measure what a status poll costs: `cargo run --release -p lcnc --example poll_cost`.
use lcnc::Connection;
use std::time::{Duration, Instant};

fn main() -> Result<(), lcnc::Error> {
    let mut lc = Connection::open(None)?;
    lc.poll()?;
    let n = 20_000;
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(lc.poll()?);
    }
    let back_to_back = t.elapsed() / n;
    // At a GUI's rate, where most polls find a new status.
    let mut worst = Duration::ZERO;
    let mut sum = Duration::ZERO;
    for _ in 0..200 {
        std::thread::sleep(Duration::from_millis(5));
        let t = Instant::now();
        std::hint::black_box(lc.poll()?);
        let d = t.elapsed();
        sum += d;
        worst = worst.max(d);
    }
    println!("poll: {back_to_back:?} back to back, {:?} mean / {worst:?} worst after 5 ms", sum / 200);
    Ok(())
}
