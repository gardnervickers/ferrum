# Ferrum's probe macros

The G-code subroutines Ferrum's PROBE screen, tool touch-off and jobs call
(`o<probe_x_plus_wco> call`, `o<tool_touch_off> call`, ...). MIT licence,
like the rest of Ferrum (see `../LICENSE`).

They were written for Ferrum from a behaviour description
([SPEC.md](SPEC.md)) of the macros Ferrum first used (Probe Basic's,
GPL-3), without their code, and they keep the same call names, arguments,
`#3004`–`#3037` inputs and result lines, so either set works with ferrum.
The harness checks every routine against simulated parts with known
dimensions (`harness/scenarios/probe-routines`, on the `mymill` config, whose INI
searches this folder first).

## Using them

Put this folder first on `[RS274NGC]SUBROUTINE_PATH`:

```ini
[RS274NGC]
SUBROUTINE_PATH = /path/to/ferrum/macros:subroutines
```

`ferrum --check` finds the folder and prints that line, and
`ferrum --check --write-ini` adds it (LinuxCNC stopped). The values the
macros read (feeds, distances, the probe's tool number) are Ferrum's:
see "Probe values" in `../docs/ferrum-toml.md`.

## What they need

- No HAL pins and no Probe Basic. They run on any LinuxCNC 2.9 machine with
  a probe on `motion.probe-input`.
- Machine units (mm or inch); none force G20/G21. Each refuses to run when
  the program units differ from the machine's.
- The probe loaded with G43 (its tool length applied), G40, no G52/G92.
- Modal state is saved and restored with M73, so a routine leaves modes as
  it found them.

Files starting `ferrum_` are helpers the routines share; the others are
the routines Ferrum calls. Two `ferrum_` files are called by Ferrum
itself: `ferrum_measure_tool` (TOOLS › MEASURE: load a tool and measure
its length on the setter) and `ferrum_tool_check` (TOOLS › CHECK and a
job's TOOL CHECK op: measure the loaded tool and stop when it is off its
table length by more than `#3037`, the breakage tolerance).

## Routines with no Probe Basic counterpart

These are Ferrum's own (not in [SPEC.md](SPEC.md)); each file's header
says exactly what it does.

- `probe_bore_3_point [a1] [a2] [a3]` and `probe_boss_3_point [a1] [a2]
  [a3]`: a bore or boss (or an arc of one) from three touches at chosen
  angles (degrees counter-clockwise from +X), fitted with
  `ferrum_circle3`; same results as the bore and boss routines. The PROBE
  screen's 3 POINTS lets the operator drag the angles.
- `probe_boss_scan [step] [max]` (with `ferrum_span_scan`): BOSS (round
  or block) with a size hint of 0: no size needed. From anywhere over
  the top it touches down every `[step]` outward until the top ends, then
  touches each face from the side, and measures across again from the
  centre. Same results as `probe_round_boss`, plus every touch as
  `ferrum_scan.receive_point{kind, x, y, z}` for the PROBE screen's 3D view.
  Aborts if a touch-down finds something lower than the top but above the
  probing depth beside the part (a vise jaw or a clamp), or if anything
  stands within `#3018` past an edge at the probing depth (a slot or step
  in the top would otherwise read as the edge).
- `probe_cal_boss`: CAL › PROBE TIP on a gauge pin of diameter `#3034`.
- `probe_cal_stylus [gauge]`: after either calibration, with the spindle
  turned half a turn by hand, measures the gauge's centre again; Ferrum
  shows how far the stylus sits off the spindle's axis.
- `probe_edge_skew [axis] [side] [d1] [d2]`: two touches at chosen places
  along an edge; reports its angle and (`#3030` = 0) sets the active work
  system's rotation to it, leaving X0 Y0 as they are.

`ferrum_touch_xy [angle] [max]` is `ferrum_touch` along any XY direction.

## Crash testing

`harness/probe-bench` runs every routine here headless on LinuxCNC 2.9.10
against a world with fixtures and a probe with a body, and fails on any
move that would hurt a real probe: a non-probing move into anything, the
body or spindle nose hitting anything, or a probing move more than 1 mm
past the surface. It also checks refusals (bad values, G20/G92/G52,
rotation, wrong tool or TLO), modal state left behind, abort and E-stop
part way, and random fuzz. See `harness/probe-bench/README.md`; CI runs it
when this folder changes.
