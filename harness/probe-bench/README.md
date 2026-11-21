# Probe bench: can a probe macro crash the probe?

A headless LinuxCNC 2.9.10 simulator that runs Ferrum's probe macros
(`ferrum/macros`) against a world of workpieces and fixtures, with a probe
that has a body, and records every move that would hurt a real probe. It
complements the GUI harness (`harness/run`, which checks Ferrum's screens):
here there is no Ferrum, only LinuxCNC and the macros, so it can run
hundreds of cases, including ones Ferrum would not send.

```sh
ferrum/harness/setup-linuxcnc                       # once (builds LinuxCNC in ~/lcnc)
ferrum/harness/probe-bench/run                      # mm and inch, every fixed case
ferrum/harness/probe-bench/run -u mm                # one unit system
BENCH_ONLY='boss|corner' ferrum/harness/probe-bench/run -u mm
BENCH_FUZZ=300 BENCH_SEED=7 ferrum/harness/probe-bench/run -u inch
```

Reports: `probe-bench-out/<units>/report.md` and `results.json`; the
crash log is `events.jsonl`. The exit status counts failing cases. A full
fixed run takes about 45 minutes per unit system (the routines run at
Gardner's real probe feeds); each fuzz case adds about 30 s.

## What counts as a crash (`world.py`)

The world is the mymill simulator's (`ferrum/sim/sim_card.py`: the STOCK
block with two slots, the PLATE with bosses, ridges, a pocket and a bore,
the block turned 2 deg, the ring gauge), at the same machine coordinates,
plus fixtures a case can switch on: a vise jaw 3 mm off the stock's right
face, a clamp bolt beside the round boss, a post inside the bore, a clamp
on the tilted block's face, a toe clamp at the stock's front, a tall block
beside the ring gauge. The probe is a ball (table diameter less the
pretravel, so a calibrated `#3032` reads true edges) on a stylus 30 mm long,
then a body of radius 10 mm up to the spindle nose (radius 35 mm).

- **contact**: the ball or stylus touches something, or presses further in,
  during a move that is not a probing move (`motion.motion-type` != 5).
  Backing away from a contact is fine.
- **overtravel**: a probing move pushes the ball more than 1 mm past the
  surface (deceleration at too high a probing or traverse feed).
- **body**: the probe body or spindle nose touches anything, in any move
  (nothing trips the probe for those).
- **tool**: a tool that is not the probe touches anything.

Positions between two samples are checked every 0.05 mm, so a rapid cannot
jump through a part. The `selftest:` cases make each kind of crash on
purpose and pass only when the bench sees it.

## The cases (`cases.py`)

- every routine once from a good start, results checked against the true
  geometry (work offset written, sizes, `#3032` for calibration);
- **refuse**: G20 on a mm machine (G21 on inch), G92, G52, rotated WCS,
  G49, another tool's G43, cutter comp, an end mill in the spindle, tip
  diameter 0, probe tool unset, each probe value zero or negative: the
  routine must fail with its message and not move at all;
- **obstacle**: a fixture in the routine's path: it must stop on contact
  (guarded move) and not crash; 3-point routines that leave the fixture out
  must still give the right answer;
- **modal**: started from G91, G93, G18, G91.1, another F, G55, M51 with
  200 % feed override: the routine must work and leave every mode as it
  found it;
- **stop**: abort and E-stop at several moments of a routine: no crash;
- **fuzz** (`BENCH_FUZZ=n`): a random routine with one to three of: a
  probe value from a list of bad ones, a start moved by up to 3 mm, a bad
  modal state, a wrong tip diameter in the table, a probe longer or shorter
  than its table length, a fixture, an abort or E-stop at a random moment.
  It may fail, but must not crash, and if it says it succeeded its numbers
  must be right (unless the case changed what "right" is).

`BENCH_SEED` makes a fuzz run repeatable; a failing case's name lists what
was changed.
