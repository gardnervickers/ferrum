# millgui test harness

End-to-end tests for millgui against a real LinuxCNC 2.9.10 simulator.
For each **machine config** x **scenario** it boots LinuxCNC with millgui as
the display (on a private X server), drives millgui through the same
websocket the browser screen uses, and checks LinuxCNC's own status
(`linuxcnc.stat()`), HAL pins, millgui's on-screen properties and
screenshots. It never talks to a real machine.

```sh
millgui/harness/setup-linuxcnc          # once per box: builds LinuxCNC v2.9.10 in ~/lcnc
millgui/harness/run                     # every config x every scenario
millgui/harness/run -c sim-inch         # one config
millgui/harness/run basics/jog probe    # some scenarios (names or folders)
millgui/harness/run --list              # what exists
```

The report is `millgui/harness-out/index.html` (a matrix, then each run with
its failure, LinuxCNC log and screenshots), plus `report.md` (the CI summary)
and `results.json`. The exit status is non-zero when any run fails.

As root (cloud containers) `run` runs the simulator as user `cncsim`,
because LinuxCNC refuses root. `LCNC_DIR` points at another LinuxCNC build.
`HARNESS_NO_BUILD=1` skips the cargo build.

## Machine configs (`configs/`)

Each folder has a `prepare OUT_DIR` script that writes a LinuxCNC config with
a `millgui.ini` (it must have a `[MILLGUI]` section; the harness adds
`REMOTE_PORT`), and a `vars` file: `tags` plus values scenarios use as
`${name}`.

| config | what | tags |
|---|---|---|
| `mymill` | Gardner's PM-728VT: `mymill.ini`, `mymill.hal`, subroutines, Probe Basic macros, M6 remap (via `millgui/sim/make-config`) | mm probe-basic mymill-hal xyza drawbar pendant |
| `sim-mm` | a stock 3-axis metric machine, LinuxCNC sim style, no Probe Basic (`configs/stock.py`) | mm stock xyz tc-loop |
| `sim-inch` | the same in inches (`LINEAR_UNITS = inch`); nothing in HAL answers M6, so millgui's tool change dialog does | inch stock xyz tc-dialog |
| `sim-xyza` | stock metric XYZ + rotary A | mm stock xyza tc-loop |
| `sim-gantry` | stock metric gantry: two Y joints (`XYYZ`) | mm stock xyz gantry tc-loop |

The `mymill` config needs Probe Basic's macros in `vendor/probe_basic`. Its
submodule is pinned to a commit that is not on the fork's remote, so a fresh
clone cannot fetch it: check out the branch head instead
(`git -C vendor/probe_basic fetch --depth 1 origin codex/mymill-2026-09-30 &&
git -C vendor/probe_basic checkout FETCH_HEAD`). Without it the probe
scenarios fail with "File not open". CI does this with the
`PROBE_BASIC_TOKEN` secret, and skips `mymill` without it.

The stock configs use LinuxCNC's own `INCREMENTS` lines (`1 mm, .01 in, ...`),
loop the joints back, acknowledge tool changes at once (`tc-loop`; sim-inch
leaves M6 to millgui's dialog, `tc-dialog`) and put the probe
a stock block (`BLOCK` in stock.py) that trips the probe input, which `hal sim-probe 1`
also trips by hand. To add a machine,
add a folder with `prepare` and `vars`.

Programs in `nc_files/` are copied into each config's `nc_files` with the
config's `${vars}` filled in, so one program works in mm and inches.

## Scenarios (`scenarios/**/*.scn`)

One step per line; `#` after a space starts a comment. Files under
`scenarios/common/` are pieces to `include`, not scenarios.

| line | does |
|---|---|
| `title TEXT` | name in the report |
| `needs TAG...` | skip on configs without every tag |
| `xfail TAG-or-CONFIG...: why` | expected to fail there (reported, does not fail the run; a pass is flagged XPASS so the line can go) |
| `allow-error TEXT` | an error message containing TEXT is fine |
| `include FILE` | another file's steps |
| `seed PATH LINE` | append LINE to a file under the run's fresh HOME before LinuxCNC starts (millgui state, settings) |
| `do ACTION ARGS...` | an `Actions` callback from `app/ui/state.slint` (`do jog-start 0 1`); args are JSON or bare words |
| `set PROP VALUE` | a two-way property the remote may set (`mdi-text`, `gcode-selected`) |
| `mdi TEXT` | send an MDI line, wait for the machine to settle |
| `idle [SECS]` | wait until the interpreter is idle, the queue empty and motion in position |
| `screen ID` | navigate millgui to a screen and wait for it |
| `tap LIST FIELD VALUE ACTION` | call ACTION with the index of the LIST row whose FIELD is VALUE (`tap files name h_square.ngc files-tap`; FIELD `-` for a list of strings: `tap pick-items - h_square.ngc pick`) |
| `wait EXPR [within SECS]` | poll until true (default 30 s) |
| `expect EXPR` | must be true now |
| `hal PIN-or-SIGNAL VALUE` | `halcmd setp`/`sets` |
| `sleep SECS` | |
| `shot NAME` | screenshot into the report |
| `golden NAME [MAX%]` | screenshot compared with `goldens/<config>/<scenario>--NAME.png` (default 0.5% of pixels may change; record with `run --update-goldens`) |
| `log TEXT` | print a note |

Expressions are `SOURCE.path OP VALUE`:

- `stat.` any `linuxcnc.stat()` field: `stat.position[0]`, `stat.homed[2]`,
  `stat.task_state`, `stat.interp_state`, `stat.tool_in_spindle`,
  `stat.g5x_offset[0]`, `stat.probed_position[2]`, `stat.file`, `stat.linear_units`.
- `prop.` millgui's `Live` properties (`app/ui/state.slint`): `prop.homed`,
  `prop.dro-pos[0].work`, `prop.tool`, `prop.screen`, `prop.messages`.
- `hal.` a pin or signal: `hal.halui.mode.is-auto == TRUE`.
- `errors` / `ui-errors`: LinuxCNC's error messages / millgui's error rows so far.
- `.len` gives a length: `prop.files.len > 0`.
- OPs: `==` `!=` `<` `<=` `>` `>=`, `~=` for numbers (`~= 10 +- 0.01`;
  default tolerance 0.002), `contains` / `!contains`.

At the end of every scenario, any LinuxCNC error-channel error or millgui
error message not covered by `allow-error` fails it.

## How it drives millgui

`MILLGUI_HARNESS=1` (set only by the harness) lets the remote websocket
(`app/src/remote.rs`) also navigate, quit, continuous-jog and mirror the
per-screen properties. Everything else goes through the same admission path
as a browser tap. millgui runs with the software renderer, so screenshots
match across machines.

## CI

`.github/workflows/millgui-harness.yml` builds LinuxCNC (cached), runs every
scenario and uploads `harness-out` as an artifact; the matrix lands in the job
summary.
