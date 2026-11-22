# Ferrum test harness

End-to-end tests for Ferrum against a real LinuxCNC 2.9.10 simulator.
For each **machine config** x **scenario** it boots LinuxCNC with Ferrum as
the display (on a private X server), drives Ferrum through the same
websocket the browser screen uses, and checks LinuxCNC's own status
(`linuxcnc.stat()`), HAL pins, Ferrum's on-screen properties and
screenshots. It never talks to a real machine.

```sh
ferrum/harness/setup-linuxcnc          # once per box: builds LinuxCNC v2.9.10 in ~/lcnc
ferrum/harness/run                     # every config x every scenario
ferrum/harness/run -c sim-inch         # one config
ferrum/harness/run basics/jog probe    # some scenarios (names or folders)
ferrum/harness/run --list              # what exists
```

The report is `ferrum/harness-out/index.html` (a matrix, then each run with
its failure, LinuxCNC log and screenshots), plus `report.md` (the CI summary)
and `results.json`. The exit status is non-zero when any run fails.

As root (cloud containers) `run` runs the simulator as user `cncsim`,
because LinuxCNC refuses root. `LCNC_DIR` points at another LinuxCNC build.
`HARNESS_NO_BUILD=1` skips the cargo build.

## On a Mac (Docker)

LinuxCNC only runs on Linux, so on macOS the harness runs in a container.
With Docker Desktop (or OrbStack, Colima) running:

```sh
ferrum/harness/docker-run                   # every config x every scenario
ferrum/harness/docker-run -c mymill probe   # same arguments as run
open ferrum/harness-out/index.html
```

The first run builds the `ferrum-harness` image (Ubuntu 24.04, LinuxCNC
2.9.10 built from source, Rust, Xvfb), which takes 15-25 minutes; later runs
start in seconds (the first Ferrum build in the container adds ~10 minutes;
give Docker at least 6 GB of memory) and keep the cargo build in the Docker volumes
`ferrum-harness-target` and `ferrum-harness-cargo`. The repo is mounted
into the container, so it tests your working tree and writes the report
back into `ferrum/harness-out/`. On Apple Silicon the image is native arm64
(the same architecture as the Pi). Screenshots are the same 1920x1080 frames
as in the cloud. `docker volume rm ferrum-harness-target` forces a clean
build; `docker rmi ferrum-harness` rebuilds the image. If Docker Hub
rate-limits the base image, `HARNESS_BASE=mirror.gcr.io/library/ubuntu:24.04`
uses Google's mirror. Behind a TLS-intercepting proxy, `HARNESS_EXTRA_CA=<ca.crt>` adds
its CA to the image and `HARNESS_DOCKER_ARGS="--network host"` lets the
container reach it; neither is needed on a normal Mac.

## Machine configs (`configs/`)

Each folder has a `prepare OUT_DIR` script that writes a LinuxCNC config with
a `ferrum.ini` (it must have a `[FERRUM]` section; the harness adds
`REMOTE_PORT`), and a `vars` file: `tags` plus values scenarios use as
`${name}`.

| config | what | tags |
|---|---|---|
| `mymill` | Gardner's PM-728VT: `mymill.ini`, `mymill.hal`, subroutines, Probe Basic macros, M6 remap (via `ferrum/sim/make-config`) | mm probe-basic mymill-hal xyza drawbar pendant |
| `sim-mm` | a stock 3-axis metric machine, LinuxCNC sim style, no Probe Basic (`configs/stock.py`) | mm stock xyz tc-loop |
| `sim-inch` | the same in inches (`LINEAR_UNITS = inch`); nothing in HAL answers M6, so Ferrum's tool change dialog does | inch stock xyz tc-dialog |
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
turn on `HAL_PIN_VARS` (a built JOB checks its stamp through a HAL pin), loop
the joints back, acknowledge tool changes at once (`tc-loop`; sim-inch
leaves M6 to Ferrum's dialog, `tc-dialog`) and put the probe
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
| `allow-error TEXT` | an error message containing TEXT (any case) is fine |
| `errors-to-ferrum` | the harness stops reading LinuxCNC's error queue (one reader only), so Ferrum sees every error, as on the machine; errors are then checked via Ferrum |
| `include FILE` | another file's steps |
| `cfg-edit FILE OLD => NEW` | replace OLD in a file of the prepared config before LinuxCNC starts (`cfg-edit ferrum.ini HAL_PIN_VARS = 1 => HAL_PIN_VARS = 0`) |
| `seed PATH LINE` | append LINE to a file under the run's fresh HOME before LinuxCNC starts (Ferrum state, settings) |
| `do ACTION ARGS...` | an `Actions` callback from `app/ui/state.slint` (`do jog-start 0 1`); args are JSON or bare words |
| `set PROP VALUE` | a two-way property the remote may set (`mdi-text`, `gcode-selected`) |
| `mdi TEXT` | send an MDI line, wait for the machine to settle |
| `idle [SECS]` | wait until the interpreter is idle, the queue empty and motion in position |
| `screen ID` | navigate Ferrum to a screen and wait for it |
| `tap LIST FIELD VALUE ACTION` | call ACTION with the index of the LIST row whose FIELD is VALUE (`tap files name h_square.ngc files-tap`; FIELD `-` for a list of strings: `tap pick-items - h_square.ngc pick`) |
| `wait EXPR [within SECS]` | poll until true (default 30 s) |
| `expect EXPR` | must be true now |
| `hal PIN-or-SIGNAL VALUE` | `halcmd setp`/`sets` |
| `sleep SECS` | |
| `reconnect [SECS]` | Ferrum restarted LinuxCNC and itself (SETUP's RESTART LINUXCNC): wait for its websocket to close, connect again (default 120 s) |
| `shot NAME` | screenshot into the report |
| `golden NAME [MAX%]` | screenshot compared with `goldens/<config>/<scenario>--NAME.png` (default 0.5% of pixels may change; record with `run --update-goldens`) |
| `log TEXT` | print a note |
| `signal STOP\|CONT\|KILL` | signal Ferrum's GUI process (STOP freezes it: no frames, no input) |
| `click X Y` | a real X pointer click at screen pixels (xdotool), through the same path as a tap |
| `press X Y` / `release` | a real X pointer pressed at screen pixels and held, then let go (a held jog button) |
| `overwrite DST SRC` | copy config file SRC over DST in place (truncate and rewrite, as `scp` does) while LinuxCNC and Ferrum run |
| `drag X0 Y0 X1 Y1` | a real X pointer drag at screen pixels (xdotool), pressed from the first point to the second |
| `screen-may-stall` | the scenario stalls Ferrum on purpose: skip the end-of-run screen watchdog checks |

Expressions are `SOURCE.path OP VALUE`:

- `stat.` any `linuxcnc.stat()` field: `stat.position[0]`, `stat.homed[2]`,
  `stat.task_state`, `stat.interp_state`, `stat.tool_in_spindle`,
  `stat.g5x_offset[0]`, `stat.probed_position[2]`, `stat.file`, `stat.linear_units`.
- `prop.` Ferrum's `Live` properties (`app/ui/state.slint`): `prop.homed`,
  `prop.dro-pos[0].work`, `prop.tool`, `prop.screen`, `prop.messages`.
- `hal.` a pin or signal: `hal.halui.mode.is-auto == TRUE`.
- `errors` / `ui-errors`: LinuxCNC's error messages / Ferrum's error rows so far.
- `.len` gives a length: `prop.files.len > 0`.
- A path may end in `{TEXT}`: the number after the last TEXT in a text
  value, or in the last list item containing it:
  `prop.pr-result{X width} ~= 40 +- 0.01`, `prop.messages{P3032=} ~= 0.059`.
- OPs: `==` `!=` `<` `<=` `>` `>=`, `~=` for numbers (`~= 10 +- 0.01`;
  default tolerance 0.002), `contains` / `!contains`.

At the end of every scenario, any LinuxCNC error-channel error or Ferrum
error message not covered by `allow-error` fails it, and so does a screen
that stopped drawing: HAL's screen watchdog (`ferrum.watchdog.ok`) must be
happy and no two frames may have been more than 700 ms apart since Ferrum
answered (it E-stops at 1 s; see `app/src/failsafe.rs`).

Ferrum starts the way the Pi starts it, through `ferrum --boot` (the
splash launcher, which runs LinuxCNC itself). `HARNESS_BOOT=0` starts
`linuxcnc ferrum.ini` with Ferrum as its display instead.
`HARNESS_RENDERER=gpu` draws with femtovg (Mesa's llvmpipe under Xvfb)
instead of the software renderer; screenshots then differ from the goldens.

## PROBE routines against known geometry (`scenarios/probe-routines`)

On `mymill` the simulated card (`ferrum/sim/sim_card.py`) has workpieces at
known machine coordinates: the STOCK block (edges, corners, top, auto stock,
two faces) with an X and a Y slot, a PLATE with a round and a rect boss, an
X and a Y ridge, a rect pocket and a bore, a block turned +2 deg (edge
angles) and a D20 ring gauge (CAL). The probe is a ball of T99's diameter
less `probe_pretravel` (= `#3032`), so the expected results are the true
geometry; the numbers are in `configs/mymill/vars`. One scenario per
routine family runs it from the PROBE screen and checks the work offset
written and the sizes reported (`needs probe-basic probe-geometry`).

mymill searches Ferrum's own macros (`ferrum/macros`) first, so these
check them; to compare with another set, `ferrum/sim/make-config --macros
DIR` puts DIR ahead of them in a config of your own.

## How it drives Ferrum

`FERRUM_HARNESS=1` (set only by the harness) lets the remote websocket
(`app/src/remote.rs`) also navigate, quit, continuous-jog and mirror the
per-screen properties. Everything else goes through the same admission path
as a browser tap. Ferrum runs with the software renderer, so screenshots
match across machines.

## CI

`.github/workflows/ferrum-harness.yml` builds LinuxCNC (cached), runs every
scenario and uploads `harness-out` as an artifact; the matrix lands in the job
summary.
