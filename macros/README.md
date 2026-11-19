# millgui's probe macros

The G-code subroutines millgui's PROBE screen, tool touch-off and jobs call
(`o<probe_x_plus_wco> call`, `o<tool_touch_off> call`, ...). MIT licence,
like the rest of millgui (see `../LICENSE`).

They were written for millgui from a behaviour description
([SPEC.md](SPEC.md)) of the macros millgui first used (Probe Basic's,
GPL-3), without their code, and they keep the same call names, arguments,
`#3004`–`#3037` inputs and result lines, so either set works with millgui.
The harness checks every routine against simulated parts with known
dimensions (`harness/scenarios/probe-routines`, on the `mymill` config, whose INI
searches this folder first).

## Using them

Put this folder first on `[RS274NGC]SUBROUTINE_PATH`:

```ini
[RS274NGC]
SUBROUTINE_PATH = /path/to/millgui/macros:subroutines
```

`millgui --check` finds the folder and prints that line, and
`millgui --check --write-ini` adds it (LinuxCNC stopped). The values the
macros read (feeds, distances, the probe's tool number) are millgui's:
see "Probe values" in `../docs/millgui-toml.md`.

## What they need

- No HAL pins and no Probe Basic. They run on any LinuxCNC 2.9 machine with
  a probe on `motion.probe-input`.
- Machine units (mm or inch); none force G20/G21. Each refuses to run when
  the program units differ from the machine's.
- The probe loaded with G43 (its tool length applied), G40, no G52/G92.
- Modal state is saved and restored with M73, so a routine leaves modes as
  it found them.

Files starting `millgui_` are helpers the routines share; the others are
the routines millgui calls. Two `millgui_` files are called by millgui
itself: `millgui_measure_tool` (TOOLS › MEASURE: load a tool and measure
its length on the setter) and `millgui_tool_check` (TOOLS › CHECK and a
job's TOOL CHECK op: measure the loaded tool and stop when it is off its
table length by more than `#3037`, the breakage tolerance).
