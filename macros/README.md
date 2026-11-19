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
