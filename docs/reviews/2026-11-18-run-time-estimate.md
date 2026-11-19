# Run-time estimate and time left (2026-11-18)

millgui now estimates how long a program takes the way LinuxCNC 2.9's
trajectory planner runs it, and counts the time down while it runs.

## What shows

- RUN: beside `LINE n / N` the program card shows `TIME 1:18` when a
  program is loaded and `LEFT 1:14` while it runs. The bar under it follows
  time, not lines, while a run is timed. Over the backplot:
  `OP 2/5 · 2D ADAPTIVE1 · 3:10 LEFT IN OP`.
- JOB: each op's estimated time leads its detail line; the title shows the
  job's total (ops turned off left out), and `… LEFT` while it runs. The
  running op's bar is by time with `40% · 3:10 LEFT`.
- FILES: the op times and the total use the same estimate.

The time left follows the feed and rapid overrides. It counts machine time
only: waits for a person (manual tool changes, M0, probing a FIND step,
which the preview skips) are not in it.

## How (crates/preview/src/timing.rs)

Each programmed move is one planner segment, as in `src/emc/tp`: top speed
and acceleration from the axes along its direction ([AXIS_n]MAX_VELOCITY,
MAX_ACCELERATION, [TRAJ]MAX_LINEAR_VELOCITY), arcs held back by sideways
acceleration (0.87 a) and given half of it along the path; corners get the
blend-arc speed from `blendComputeParameters` (G64 P or a quarter of each
move); nearly straight joins pass by the kink rule; rapid/feed changes,
G61 corners, G61.1, reversals and non-motion commands stop. Every segment
and blend arc lasts at least 1.02 servo periods (what limits dense 3D
finishing). Then a backward and forward pass with the 50-move lookahead,
trapezoids, and per-stop waits: 9 ms per stop, 1.2 x [EMCIO]CYCLE_TIME per
coolant or tool command (both measured).

`timing::Tracker` finds the tool on the toolpath (the motion line, then
the nearest piece ahead within 2 mm, only moving forward) and gives the
time left in the op and the program.

## Checked against the simulator

LinuxCNC 2.9.10 uspace sim, axes 100 mm/s and 900 mm/s² (as mymill.ini),
1 ms servo period; wall time from start to idle.

| program | real | estimate | old estimate |
|---|---|---|---|
| squares (long feeds and rapids) | 177.3 s | 177.0 s | 169.5 s |
| spiral, 3000 short moves at F3000 | 252.6 s | 252.1 s | 252.0 s |
| zigzag facing, 100 corners | 194.6 s | 193.4 s | 192.8 s |
| 3D finishing, 4000 moves of 0.05 mm | 9.39 s | 9.40 s | 8.12 s |
| small arcs at F4000 | 18.1 s | 17.4 s | 9.3 s |
| 50 holes G81 | 148.4 s | 146.9 s | 113.6 s |
| random corners, G64 P0.02, dwells, M8/M9 | 471.5 s | 469.5 s | 462.9 s |
| 200 reversals | 31.62 s | 31.61 s | 4.35 s |
| 50 x M8 between moves | 11.7 s | 12.1 s | 3.0 s |
| 100 straight moves in G61 | 3.56 s | 3.06 s | 3.00 s |

The real times include starting the program (a few tenths of a second),
which the estimate leaves out.

`cargo run -p preview --example estimate -- machine.ini prog.ngc` prints
the estimate (rs274 on PATH). Harness: `basics/time-left`, and
`job/probe-then-program` checks the JOB page's times.
