# millgui configuration: what a new machine has to tell it

Status: plan, 2026-11-18, written against main `6c68340`. Nothing here is
built yet. The inventory comes from reading the code, `mymill.ini`,
`mymill.hal` and `subroutines/`; nothing was run, on the simulator or the
mill. Items marked **verify** are things the implementer should confirm in
the simulator before relying on them.

Audience: the thread that implements this. The goal is that someone with a
working LinuxCNC mill who has never used Probe Basic can install millgui,
start it, and be walked through everything it needs, and that millgui can
later ship as its own project.

## 1. Summary

millgui has about 25 real settings today, but most of them are not settings
yet. They are one of:

- INI keys Probe Basic users already have (`[DISPLAY]`, `#30xx` probe values
  in `probe_basic.var`);
- constants in the code that happen to match the PM-728VT (probe is T99, Z 0
  is the top of travel, millimetres everywhere, A is a rotary about X);
- HAL signal names from `mymill.hal` written into `millgui/hal/*.hal`;
- G-code subroutines that live in this repo or in the Probe Basic
  submodule, several of which need `mymill.hal`'s latches to run at all.

A stranger's machine fails in four different ways, and the plan treats them
differently:

| Class | Example | What it needs |
| --- | --- | --- |
| Silent and wrong | An inch machine: every number is read as mm | Fix in code first (phase 1), no setting can paper over it |
| Hangs or aborts | M6 with no `drawbar` module: nothing answers the tool change | Split modules, detect, say why |
| Feature dead with a HAL error line | `probe.hal` nets `wireless-probe-tool-active`, which only `mymill.hal` has | Signal map in config, nets generated |
| Works, shows Gardner's values | Table model, warm-up rpm, MDI library text | Plain settings with detected defaults |

The proposal in one paragraph: millgui reads machine facts from the INI and
from HAL and never asks for them; everything else lives in one file,
`millgui.toml`, beside the INI, which a SETUP screen writes and a person can
also edit; a checker (`millgui --check`, and the same code at startup)
turns every requirement below into a row that is OK, needs attention, or
blocks one named feature; a missing setting disables the one feature that
needs it and never stops the GUI from starting. Gardner's mill becomes a
profile (a `millgui.toml`, a signal map and its own subroutines) and stops
being the default.

## 2. Where settings live

Four layers, each with one writer.

| Layer | File | Writer | Holds |
| --- | --- | --- | --- |
| Machine facts | the LinuxCNC INI | the user, PNCconf | axes, units, limits, velocities, spindle max, paths. millgui only reads |
| millgui machine config | `millgui.toml` beside the INI (`[MILLGUI]CONFIG` to move it) | SETUP screen, or by hand | everything in section 4 marked "config" |
| Operator state | `~/.local/state/millgui/*` (today's `settings.rs`) | millgui | jog speed, power feed values, GOTO list, conv field memory, warm-up stages |
| Interpreter parameters | `[RS274NGC]PARAMETER_FILE` | LinuxCNC only | G30/G28, work offsets. millgui changes them through MDI and never writes the file |

Decisions for the implementer:

- `millgui.toml` replaces `machine.toml` (its `[buttons]` and `[model]`
  tables move in unchanged). Keep reading `machine.toml` when no
  `millgui.toml` exists, so the Pi keeps working until its profile lands.
- Keep `serde(deny_unknown_fields)` but stop falling back to all defaults on
  one bad key: report the key in SETUP and use defaults for that table only.
- Write with `toml_edit`, so comments and hand edits survive a save from the
  SETUP screen. Write to a temp file and rename.
- The INI's `[MILLGUI]` section shrinks to `CONFIG` (optional). `MODULES`,
  `POSTGUI_HALFILE`, `REMOTE_PORT`, `REMOTE_ROOT`, `RENDERER`, `MDI_LIBRARY`
  and `MACHINE_CONFIG` stay honoured as fallbacks, with `millgui.toml`
  winning. millgui must never rewrite the INI while LinuxCNC runs (LinuxCNC
  has it loaded, PNCconf regenerates it, `#INCLUDE` makes "which file" unclear).
- Stock users set `[DISPLAY]DISPLAY = millgui` in their own INI. The
  `millgui.ini` + `#INCLUDE mymill.ini` wrapper is a mymill arrangement for
  keeping Probe Basic alongside; it should not appear in the standalone docs.
- Probe defaults stop depending on the parameter file. See 4.6.

Sketch of the file (every key optional; the comments are the detected value
or default):

```toml
[machine]
units = "auto"            # from [TRAJ]LINEAR_UNITS
z_top = "auto"            # machine Z to retract to; default [AXIS_Z]MAX_LIMIT minus 0.5 mm
park = "machine-zero"     # or "g28", or [x, y] in machine coordinates
rotary = "auto"           # which of A/B/C exists and the linear axis it turns about ("x")

[modules]
enabled = "auto"          # or a list, in nav order

[spindle]
max_rpm = "auto"          # [SPINDLE_0]MAX_FORWARD_VELOCITY
reverse = true
manual_rpm = 300
warmup = [[1000, 3], [2000, 3], [3000, 3], [4000, 3]]   # scaled to max_rpm on first run
coolant = ["mist", "flood"]

[toolchange]
style = "auto"            # "dialog" | "external" | "drawbar"
scratch_tools = []        # mymill: [200, 249]

[probe]
enabled = false
tool = 99
# defaults sent before every probe run; see 4.6 for the list
[probe.defaults]
fast_feed = 300
# ...

[toolsetter]
enabled = false
position = "g30"
# ...

[hal]                     # logical name -> HAL signal or pin; see 4.8
probe_tripped = "auto"

[model]                   # backplot table, today's machine.toml
[buttons]
[display]
[remote]
```

## 3. Setup experience

### 3.1 Tiers

millgui must be useful before any setup. Three tiers, decided at startup
from the checks, shown on SETUP:

1. **Core** needs only a valid INI and a running LinuxCNC: RUN, MANUAL (jog,
   home, MDI, spindle, coolant, zero), FILES, OFFSETS, TOOLS, preview and the
   travel check. No millgui subroutine, no HAL signal, no `millgui.toml`.
2. **Tooling** adds the tool change dialog, the tool setter and probing.
   Needs the probe/setter questions answered and millgui's macros on
   `SUBROUTINE_PATH`.
3. **Extras**: JOB and CONV, pendant, drive and interlock pills, load
   colouring, remote screen, rotary probing. Each has its own row.

Today core is not clean: MANUAL's PARK calls `o<go_to_home>`, power feed
calls `o<mymill_powerfeed>`, TOOLS' UNLOAD calls `o<unload_spindle>`, and
the default module list includes `drives drawbar interlock`. Phase 1 fixes
that (section 6).

### 3.2 The checker

One module, `app/src/setup/checks.rs`, returns `Vec<Check>`:

```rust
struct Check {
    id: &'static str,          // "probe.input-wired"
    group: Group,              // Machine, Spindle, ToolChange, Probe, ToolSetter, Hal, Job, Display
    level: Level,              // Ok, Info, Attention, Blocks(&'static [FeatureId])
    title: String,             // "Probe input is not connected"
    detail: String,            // what was looked at and what was found
    fix: Fix,                  // Setting(key) | IniLines(Vec<String>) | HalHint(String) | None
}
```

It runs in three places from the same code:

- `millgui --check <ini>`: prints the rows and exits non-zero when anything
  blocks core. Works without LinuxCNC running (HAL checks report "LinuxCNC
  not running", not a failure). This is also what the docs tell a new user
  to run first, and what bug reports should paste.
- At startup, before the window: results gate features. A blocked feature
  keeps its nav entry and screen, with the buttons that need it disabled and
  the reason on them ("PROBE needs setup: probe tool number"). This replaces
  today's one-line `notes.join("; ")` in the message bar, which a touch user
  cannot act on.
- The SETUP screen: the rows, grouped, each with its fix.

### 3.3 SETUP screen and first run

A new always-on module `setup` (nav entry at the end, a badge with the count
of Attention rows). When no `millgui.toml` exists it opens first, with one
sentence: core works now; the rest is below.

Rows whose fix is `Setting` open the normal keypad or a picker in place.
Rows whose fix is `IniLines` show the exact lines to add and the section,
with the text "LinuxCNC must be restarted"; millgui does not edit the INI
from the GUI. `millgui --check <ini> --write-ini` may append the lines to
the INI with a `.bak` copy when LinuxCNC is not running (**decision for
Gardner**, default: print only).

Guided steps (each is a group of rows with a NEXT, not a separate wizard
framework; skippable, re-enterable):

1. **Machine**: shows what was read (axes, units, travel per axis, max
   velocity) for the user to confirm. Asks only: is machine Z maximum the
   safe retract height; where to park.
2. **Spindle and coolant**: max rpm read from the INI; reverse yes/no; which
   coolant buttons to show; warm-up stages.
3. **Tool change**: detected style, explained (4.5).
4. **Probe**: do you have one; its tool number; live input test ("touch the
   probe: the row turns green while `motion.probe-input` is high"); feeds
   and clearances with defaults; whether millgui's probe macros are found.
5. **Tool setter**: do you have one; jog over it and SET POSITION HERE
   (G30.1); live trip test; feeds; reference height.
6. **Extras**: pendant, drive status, load, remote.

The live input tests are the one place a setup screen is much better than a
file: they catch the wrong pin, the inverted pin and the unplugged probe
before a G38 move does.

Nothing in SETUP moves the machine except buttons that already exist
elsewhere (jog). Probe calibration stays on PROBE › CAL.

## 4. Inventory

Columns: **Today** is where the value comes from now. **Detect** is what
millgui can read without asking. **Set** is `config` (`millgui.toml`),
`setup` (config, with a SETUP row worth showing), `ini` (user edits the
INI), or `code` (not a setting: fix the assumption). **If missing or wrong**
is the behaviour today on a machine that is not Gardner's.

### 4.1 Start-up, files and paths

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| A1 | INI path | `-ini` arg or `INI_FILE_NAME` (`main.rs:61`) | LinuxCNC passes it | none | n/a | no config, simulated defaults |
| A2 | NML file | `[EMC]NML_FILE`, else the shim's default | INI | LinuxCNC default | ini | 15 s of retries, then exit 1 with one stderr line. Should show a window saying so |
| A3 | Program folder | `[DISPLAY]PROGRAM_PREFIX`, else `nc_files` under the INI folder (`files.rs:118`) | INI; create if absent | `~/linuxcnc/nc_files` | ini | empty FILES list, no message |
| A4 | Parameter file | `[RS274NGC]PARAMETER_FILE`, else `linuxcnc.var` (4 call sites) | INI | as LinuxCNC | ini | offsets table, G30 readout and probe defaults read as empty or zero |
| A5 | Tool table | `[EMCIO]TOOL_TABLE`, else `tool.tbl` (`tools.rs:424`) | INI | as LinuxCNC | ini | TOOLS empty. millgui edits the file and sends a reload; a tool database program (`[EMCIO]DB_PROGRAM`) is not supported and must be detected and TOOLS made read-only |
| A6 | `rs274` on PATH | `preview/rs274.rs:26` | PATH | n/a | check row | no preview, no travel check, silently |
| A7 | `halcmd` on PATH, `liblinuxcnchal.so.0` loadable | `main.rs:164`, `hal/lib.rs:11` | try | n/a | check row | "HAL unavailable" note, every HAL module dead |
| A8 | millgui's own files | `millgui/hal/<id>.hal`, `millgui/mdi.toml`, `millgui/machine.toml`, `millgui/web/dist`, all relative to the INI folder; binary at `millgui/bin/millgui` via `millgui/run` | n/a | n/a | code | A packaged millgui has no folder inside the user's config. Built-in copies of the MDI library and HAL templates; data in `/usr/share/millgui`; binary on PATH |
| A9 | State and cache | `$MILLGUI_STATE`, `~/.local/state/millgui`, `~/.cache/millgui` | env | XDG | n/a | fine. State is per user, not per machine: key the folder by the INI's `[EMC]MACHINE` name so two configs on one PC do not share GOTO lists and probe markers |
| A10 | `[DISPLAY]` keys under `#INCLUDE` | first `[DISPLAY]` wins, so `millgui.ini` repeats keys | n/a | n/a | docs | only matters for the wrapper layout; stock users edit their own `[DISPLAY]` |

### 4.2 Axes, kinematics and units

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| B1 | Axis list | `[TRAJ]COORDINATES` (`config.rs:85`) | INI, and `axis_mask` from status | XYZ | n/a | fine for XYZ, XYZA |
| B2 | Joint for each axis | identity: HOME beside an axis homes joint = position of the letter in COORDINATES (`main.rs:436`); jogs before homing use the axis index as the joint | `[KINS]KINEMATICS` and COORDINATES; a repeated letter (`XYYZ` gantry) or non-trivkins is detectable | identity | code | gantry: HOME Y homes one side only; unhomed jog moves the wrong joint. Phase 1: build the axis to joints map from COORDINATES, home all joints of an axis (or only offer HOME ALL when letters repeat), and with non-trivial kinematics hide per-axis home and unhomed axis jog |
| B3 | Linear units | never read. Everything is mm: DRO 3 decimals, jog speed "mm/s", power feed, probe values, conv defaults, table model, travel check slack, the "5 mm below Z 0" teach rule | `[TRAJ]LINEAR_UNITS` (`mm`, `metric`, `inch`, `imperial`, or a number) and `linear_units` already in the status struct (`nml.rs:52`) | mm | code, then `machine.units` to override display only | **inch machine: silently wrong.** Jog speed 50 is 50 in/s (LinuxCNC clamps it, so jogs run at max), limits compare right but every constant in mm is 25.4x off, probe defaults in mm would be sent as inches. Phase 1 must either make core unit-correct or refuse non-mm machines with a blocking row. Recommendation in 7 |
| B4 | Program units vs machine units | G20/G21 state is in status but unused for display | status | n/a | code | DRO shows machine units while an inch program runs on a mm machine. Acceptable if labelled; label the DRO |
| B5 | Z top | `MACHINE_TOP = "G53 G0 Z0"` in jobs (`mill_ops.rs:7`), restart preamble, MDI library, `go_to_home`, `go_to_g30`, job probe teach rule `actual[2] > -5.0` (`probe.rs:533`), backplot "Z 0 is the top of travel" | `[AXIS_Z]MAX_LIMIT` | `MAX_LIMIT - 0.5 mm` (0 when the limit is within 1 mm of 0, as on the PM-728VT) | setup: `machine.z_top` | a machine homed at the bottom or with Z max = 100: `G53 G0 Z0` is not a retract; it can be a plunge into the part. This is the most dangerous hardcode after units |
| B6 | Soft limits | `[AXIS_X/Y/Z]MIN_LIMIT/MAX_LIMIT` (`plot.rs:109`, `powerfeed.rs:69`) | INI | n/a | n/a | absent: no travel check (silently) and power feed "end of travel" is off. Add a row |
| B7 | Rotary axis | A only, index 3 hardcoded (`manual.rs:15`), assumed to turn about X, mounted left, wrapped; drive check skips 'A' by letter (`modules.rs:222`); rotary probe targets need axis 3 | COORDINATES has A/B/C; `[AXIS_A]WRAPPED_ROTARY`; `[JOINT_n]TYPE = ANGULAR` | none | setup: `machine.rotary = { axis = "A", about = "x" }` | B or C rotary: no jog row logic, no reference button; rotary probe macros are written for A about X. Jog and DRO should work for any rotary letter; rotary probing stays A-about-X only and says so |
| B8 | Rotary has no home switch ("A REF") | assumed: pill says REF, SET A REFERENCE homes joint 3 in place | `[JOINT_n]HOME_SEARCH_VEL = 0` | n/a | code (derive) | a rotary with a real home switch is told "REF" and gets a button that starts a homing move |
| B9 | Homing | HOME ALL sends home -1; "homed" means every joint | `HOME_SEQUENCE` on every joint; `NO_FORCE_HOMING`; `VOLATILE_HOME` | n/a | check row | no `HOME_SEQUENCE`: HOME ALL fails with LinuxCNC's message. Offer per-axis home only and say why |
| B10 | Velocity limits for the preview | `[TRAJ]MAX_LINEAR_VELOCITY` for rapid time (`plot.rs:120`) | INI | 100 | n/a | time estimate only |

### 4.3 Jogging and overrides

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| C1 | Jog increments | `[DISPLAY]INCREMENTS`, split on commas; a step that does not parse as a number becomes a **continuous** jog (`manual.rs:421`) | INI | `JOG, 0.01, 0.1, 1, 10` (mm) or `JOG, .0001, .001, .01, .1` (inch) | ini, or `display.jog_steps` | **stock INIs write `1mm`, `.001in`, `1/16 in`: selecting one jogs continuously.** Phase 1: parse LinuxCNC's increment syntax (suffix mm/in/inch/cm/um/mil, fractions), convert to machine units, drop what cannot be parsed and say so |
| C2 | Jog speed limits and defaults | `[DISPLAY]` DEFAULT/MIN/MAX_LINEAR_VELOCITY, DEFAULT/MIN/MAX_ANGULAR_VELOCITY, else 50/0.01/100 and 180 | INI; fall back to `[TRAJ]MAX_LINEAR_VELOCITY` then the smallest `[AXIS_*]MAX_VELOCITY` | derived | n/a | fallbacks are mm numbers |
| C3 | Override limits | `[DISPLAY]MAX_FEED_OVERRIDE`, MAX/MIN_SPINDLE_OVERRIDE, else 2.0/2.0/0.5; rapid fixed 0 to 100 % | INI | as LinuxCNC (1.2 / 1.0 / 0.5 are the stock defaults) | n/a | fine |
| C4 | Remembered jog speed | state file | n/a | C2 | n/a | fine |

### 4.4 Spindle and coolant

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| D1 | Max rpm | `[SPINDLE_0]MAX_FORWARD_VELOCITY` / `MAX_REVERSE_VELOCITY`, used by JOB and CONV only (`job.rs:503`); "max 4250" is text in `mdi.toml`; warm-up accepts up to 24000 | INI (also `[SPINDLE_9]`-era names **verify**) | none: no limit check | setup: `spindle.max_rpm` | manual spindle and warm-up accept any rpm; conv defaults ask for 4250 on a 3000 rpm spindle and the job refuses |
| D2 | Min rpm | none | `[SPINDLE_0]MIN_FORWARD_VELOCITY` | 0 | config | VFD spindles that stall below a floor |
| D3 | Reverse | M4 offered everywhere | `MAX_REVERSE_VELOCITY = 0` | true | setup | button that does nothing or faults a VFD |
| D4 | Manual spindle rpm | 300, remembered (`manual.rs:92`) | `[DISPLAY]DEFAULT_SPINDLE_SPEED` | that, else 10 % of max | n/a | fine |
| D5 | Speed feedback | none; pill says "SPINDLE CMD" | `spindle.0.speed-in` connected | commanded | `hal.spindle_speed` | fine; show actual when wired |
| D6 | Warm-up stages | 1000/2000/3000/4000 rpm x 3 min, remembered (`util.rs:21`) | D1 | 25/50/75/95 % of max | setup | a 24k spindle warms to 4000; a 2000 rpm spindle is commanded to 4000 |
| D7 | Coolant kinds | quick buttons `mist`, `flood` in `machine.toml`; conv offers MIST/FLOOD/OFF | `iocontrol.0.coolant-mist` / `-flood` connected | show what is wired | setup: `spindle.coolant` | buttons that do nothing |
| D8 | Spindle start blockers | messages from the probe and drawbar modules | n/a | n/a | via 4.8 | fine |

### 4.5 Tool change

Three styles. millgui today implements only the third and hides the first
inside it.

| Style | What the machine has | millgui does |
| --- | --- | --- |
| `external` | an ATC or its own M6 remap that answers `iocontrol.0.tool-changed` itself, or `hal_manualtoolchange` | nothing; shows the tool number |
| `dialog` | plain manual change: `iocontrol.0.tool-change` raised, someone must answer | shows the overlay, RESUME sets `tool-changed` |
| `drawbar` | `dialog` plus a power drawbar and the touch-off latch (mymill) | today's overlay in full |

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| E1 | Who answers M6 | the overlay exists only when the `drawbar` module is listed (`modules.rs` `toolchange()`), and `drawbar.hal` nets signals named `tool-change-request`, `tool-change-confirmed`, `tool-number`, `spindle-request` | is `iocontrol.0.tool-changed` already driven by something (halcmd: the pin's signal has a writer)? Then `external`. Otherwise `dialog` | detected | setup: `toolchange.style` | no `drawbar`: **M6 waits forever** unless the user's HAL loops it back. With `drawbar` on a stock machine: five halcmd errors and a dead overlay. Phase 2: a `toolchange` module that owns the overlay and nets itself to `iocontrol.0.*` directly; `drawbar` becomes an add-on that only adds the drawbar rows |
| E2 | Tool prepare loopback | `mymill.hal` loops `tool-prepare` to `tool-prepared` | pin connected | n/a | check row with the HAL line to add | T word hangs |
| E3 | M6 remap | mymill remaps M6 to `m6_toolchange_optional_touch_off` with Python prolog/epilog (machine side, not millgui). RUN FROM SELECTED and the preview know M6 may be remapped (preview strips `REMAP` lines) | `[RS274NGC]REMAP` | none | n/a | fine without a remap. RESUME + TOUCH OFF needs the remap and the latch: hide it unless `toolchange.touch_off_after_change` is set up |
| E4 | Probe tool rules | touch-off refused for the probe tool and T0; CYCLE START refused with the probe loaded (`main.rs:485`) | n/a | n/a | via F1 | wrong tool number makes these rules hit a cutter or miss the probe |
| E5 | Scratch tools | T200 to T249 warn "touch off likely required" (`toolchange.rs:27`) | n/a | none | config: `toolchange.scratch_tools` | harmless but odd |
| E6 | Unload | `o<unload_spindle>` (`M61 Q0 G49`, and Probe Basic's `#3991`) | n/a | send `M61 Q0` then `G49` as MDI | code | "unable to open file" error |
| E7 | Tool length source | tool table Z; "length applies at next G43" | n/a | n/a | n/a | fine |

### 4.6 Probe

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| F1 | Probe tool number | `[PROBE]TOOL_NUMBER`, else 99, read in 7 places. `mymill.ini` has no `[PROBE]` section, so it is 99 by default everywhere; `tool_touch_off.ngc` (`#5400 EQ 99`), `mymill.hal` (`probe-manager.wireless-tool-number 99`) and `#3014` each carry their own copy | none reliable | none: ask | setup: `probe.tool`, read once into one place | wrong number: "Load the probe (T99) first" forever, or the spindle-start and cycle-start guards protect the wrong tool. The macros must take the number from millgui (a parameter it sets), not a literal |
| F2 | Probe exists and is wired | assumed | `motion.probe-input` has a signal | false | setup: `probe.enabled`, with the live test | G38 moves that never trip |
| F3 | Probe status signals | `probe.hal` nets `wireless-probe-tool-active`, `wireless-probe-fault`, `probe-in` (all from `mymill.hal`'s `probe_manager`) | `probe_tripped` = the signal on `motion.probe-input`; the other two do not exist on stock machines | tripped only | `hal.probe_tripped`, `hal.probe_loaded`, `hal.probe_fault` | three halcmd errors; pill stuck on "PROBE NOT LOADED". With no `probe_loaded` signal, derive "loaded" from tool number = F1 |
| F4 | Spindle lockout with the probe in | HAL (`spindle.0.inhibit` from probe_manager) plus a GUI message | is `spindle.0.inhibit` connected | n/a | Attention row | without the HAL interlock the only guard is millgui refusing its own spindle buttons; a program's M3 still spins the probe. Say so plainly; offer the HAL snippet |
| F5 | Probe defaults `#3004` to `#3037` | read from the parameter file at start (`probe.rs:1375`); a run writes only the values that differ and restores after | n/a | below | setup: `[probe.defaults]`, `[toolsetter]` | **fresh machine: the file has no such lines, every value is 0.** The macros then abort ("feeds must be positive") at best. Also LinuxCNC only persists numbered parameters that are already listed in the file (**verify**), so values set by MDI vanish at restart. Change: millgui owns the defaults in `millgui.toml` and writes the full set before every probe run; nothing is read from the parameter file. Calibration results (`#3010`, `#3032`), which arrive as DEBUG lines, are saved to `millgui.toml` |
| F6 | Probe macros | 30-odd `o<probe_*>` and `o<mymill_probe_*>` subs, found through `SUBROUTINE_PATH = subroutines:subroutines/fusion_probing:vendor/probe_basic/...` | each needed file resolvable on `[RS274NGC]SUBROUTINE_PATH` | n/a | check row + ini | "Unable to open file <probe_...>" on every routine. See 5 |
| F7 | Results | macros print `(DEBUG, EVAL[vcp.getWidget{"name"}.setValue{..}])`, parsed in `probe.rs:1183`; job checks read `#1001` to `#1010` | n/a | n/a | code | tied to Probe Basic's macro text. Fine while those macros are used; document as the macro contract |
| F8 | Interpreter options | needs `[RS274NGC]` `HAL_PIN_VARS = 1` (mymill subs, job programs), `INI_VARS = 1`, and an `ON_ABORT_COMMAND` that leaves G90/G40 | INI | n/a | check row + ini | `#<_hal[...]>` not defined: every guarded macro and every job aborts at its first line |
| F9 | Preconditions | G40, no G92, unrotated WCS, `#3031` off (`probe.rs:471`) | status | n/a | n/a | fine, generic |
| F10 | Wired vs wireless, shared input | `probe_manager.comp` picks probe or setter by tool number | n/a | one input, no manager | docs + optional HAL snippet | fine: one `motion.probe-input`, however the user ORs it |

Defaults to seed (mm; inch machines get the converted, rounded set). These
are starting values for a 2 mm-tip touch probe and must be labelled as such
in SETUP; the user confirms them before the first run.

| Key | # | Default | Key | # | Default |
| --- | --- | --- | --- | --- | --- |
| slow_feed | 3015 | 25 mm/min | xy_clearance | 3019 | 5 |
| fast_feed | 3016 | 300 | max_z | 3020 | 15 |
| traverse_feed | 3017 | 1500 | z_clearance | 3021 | 5 |
| max_xy | 3018 | 15 | extra_depth | 3022 | 0 |
| step_off | 3023 | 10 | edge_width | 3024 | 20 |
| calibration_offset | 3032 | 0 (uncalibrated: Attention row) | ring_gauge | 3033 | 25 |

Setter: `fast_feed` 3004 = 300, `slow_feed` 3005 = 25, `traverse` 3006 =
1500, `max_z` 3007 = none (must be taught), `retract` 3009 = 2,
`reference_height` 3010 = none (blocks touch-off until set or calibrated).
The hints (3025 to 3029) stay per run. 3030 and 3031 are set by millgui per
run and are not user defaults.

### 4.7 Tool setter

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| G1 | Has a setter | assumed (UTIL card, TOUCH OFF buttons, PROBE › setter) | none | false | setup: `toolsetter.enabled` | buttons that start a G38 into nothing |
| G2 | Setter position | G30 (`#5181` to `#5183`), set with `o<set_g30_position>` (G30.1), read from the parameter file at start and from the macro's DEBUG lines | parameter file | unset | setup step "jog over the setter, SET HERE" | G30 at 0,0,0: touch-off drives to machine zero and probes there. Block touch-off until G30 has been set through millgui once (remember that in `millgui.toml`) |
| G3 | G30 is the setter | hardcoded convention; MDI library says "G30 = tool setter, G28 = park" | n/a | G30 | config: `toolsetter.position = "g30"` or `[x, y, z]` | users who already use G30 for something else |
| G4 | Reference height | `#3010` "spindle zero height", found by `o<probe_spindle_nose>`, which needs `[TOOL_SETTER]SPINDLE_NOSE_DIAMETER`, T0, and `mymill.hal`'s `spindle-nose-reference-valid` flipflop pulsed with M64 P7 | n/a | none | setup: either probe the nose (mymill profile) or type the value, or pick "lengths relative to a reference tool" | nose routine aborts on any other machine (`_hal[spindle-nose-reference-valid.out]` undefined). Generic path: a setter macro that measures a master tool or a typed height; the nose-reference validity latch stays mymill-only and its overlay line is hidden when `hal.nose_reference_valid` is unset |
| G5 | Setter trip signal | `toolsetter-triggered` | same as probe input unless separate | probe input | `hal.toolsetter_tripped` | status row blank |
| G6 | Air blast | `millgui.probe.blast-request` to `touchsetter-blast-manual-request-pin`; macros pulse M64 P5 | none | absent | `hal.setter_blast`, `toolsetter.blast_dout = 5` | UTIL button does nothing; hide when unset |
| G7 | Diameter modes | `#3011` to `#3013`; diameter probing "not implemented" in the macro | n/a | off | hidden | n/a |
| G8 | Breakage tolerance | `#3037` | n/a | 0.5 | config | n/a |

### 4.8 HAL: modules and signal names

Today each module has a hand-written `millgui/hal/<id>.hal` that nets
millgui's pins to signal names that only exist in `mymill.hal`. `halcmd -k`
keeps going past failures and the first error goes to the message bar.

| Module | Pins (`millgui.<id>.*`) | Nets to (mymill names) | Generic? |
| --- | --- | --- | --- |
| probe | wireless-selected, fault, tripped, toolsetter, blast-request, blast-active | `wireless-probe-tool-active`, `wireless-probe-fault`, `probe-in`, `toolsetter-triggered`, `touchsetter-blast-*` | tripped only |
| drives | ready-x.., fault-x.. per axis | `x-hlfb-monitor-arm`, `x-hlfb-fault` (ClearPath HLFB, `[HLFB]ARM_DELAY`) | no; generic equivalent is `joint.N.amp-fault-in` / a per-axis "drive ok" signal |
| drawbar | extended, spindle-rearm-required, tool-change, tool-number, tool-changed, spindle-request, manual-request, touch-off-*, nose-reference-valid | `drawbar-extended`, `tool-change-request`, `tool-touch-off-request-latch.*`, `spindle-nose-reference-valid.out`... | tool-change trio only (move to `toolchange`) |
| interlock | held, probe-fault, drives-not-ready | `wireless-probe-fault` for both held and probe-fault | no; `held` should be the signal on `motion.feed-inhibit` |
| pendant | connected, asleep, start, auto-request, ask-*/mode-* x4 | `whb.*` pins and `pdnt.*` nets of `xhc-whb04b-6.hal`; **unlinks** `halui.mode.*` and `halui.program.run` | WHB04B-6 with the stock example HAL only |
| load | value (float 0..1) | nothing | yes, opt-in |
| job | separate component `mymill-job`: stamp, skip-1..64, stop-after | read by compiled jobs as `#<_hal[mymill-job.*]>`; `M66 E0 L0` to sync | yes, but rename to `millgui-job` and needs F8 |
| (outputs) | `outputs.hal`: two `setp hm2_7i76e...` | Gardner's 7i76E | no: belongs to the mymill config, not millgui |

Plan:

- **Signal map.** `[hal]` in `millgui.toml` maps a logical name to a HAL
  signal or pin name. millgui creates its pins, then for each mapped name
  runs the equivalent of `net <signal> millgui.<pin>`; if the name is a pin
  with no signal, it creates `millgui-<name>` and nets both. An unmapped
  name creates no pin and its UI is hidden. A mapped name that does not
  exist is one Attention row naming it. The hand-written `.hal` files go
  away for stock users; mymill's become its profile's `[hal]` table (or
  stay as a `postgui` file listed in the profile).
- **Detection through halcmd.** `halcmd -s show pin <name>` and `show sig`
  give connection and writer state in a parseable form (**verify** the `-s`
  output on 2.9.10). Used for: `motion.probe-input`, `motion.feed-inhibit`,
  `spindle.0.inhibit`, `spindle.0.speed-in`, `iocontrol.0.tool-changed`,
  `iocontrol.0.tool-prepared`, coolant outputs, presence of `whb.*`.
  Detection only suggests; nothing is netted without a `[hal]` entry or an
  "auto" the user has seen on SETUP.
- **Default modules.** With no list: `run manual files offsets tools setup`
  always; `toolchange` when E1 says nobody answers M6; `probe` when
  `probe.enabled`; `job conv` when F8 holds; `util` always (warm-up is
  generic; setter rows hide per G1). `drives drawbar interlock pendant load`
  only when listed. Today's default list (`modules.rs:105`) includes
  `drives drawbar interlock`, which on a stock machine produces three
  failing HAL files.
- **Post-GUI HAL file.** LinuxCNC leaves `[HAL]POSTGUI_HALFILE` to the GUI.
  A stock user's file names their old GUI's pins (`axisui.*`,
  `gmoccapy.*`), so millgui must not run it blindly. Keep
  `[MILLGUI]POSTGUI_HALFILE` (also `postgui = [...]` in the toml) as the
  only files run, and add a row when `[HAL]POSTGUI_HALFILE` exists: "not
  run by millgui; these lines reference another GUI", listing them.
- **Pendant.** Rename the module `pendant-whb04b`; the takeover of
  `halui.mode.*` is specific to that driver's behaviour. Other pendants
  that jog through `axis.N.jog-*` in HAL need nothing from millgui. A
  detected `whb.*` component with the module off is an Attention row,
  because without the takeover a wheel click aborts a running probe macro.
- **E-stop and machine on.** millgui sends NML state commands; the chain is
  the user's HAL. No setting. A row when `iocontrol.0.emc-enable-in` is
  unconnected would be noise; skip.

### 4.9 Subroutines millgui calls

| Call site | Sub | Needs beyond plain G-code |
| --- | --- | --- |
| MANUAL › PARK, job probe ops | `go_to_home` | `G53 G0 Z0` then `G53 G0 X0 Y0` (B5, H1), `#<_ini[traj]axes>` |
| MANUAL power feed, GO TO, Z zero | `mymill_powerfeed` | `mymill_modes_begin` → `mymill.hal`'s mode-recovery latches, M64/M65 P8/P9, M66 E0 |
| TOOLS › UNLOAD | `unload_spindle` | nothing (replace with MDI, E6) |
| UTIL | `go_to_g30`, `set_g30_position` | `G53 G0 Z0` |
| PROBE (stock Probe Basic) | `probe_{x,y}_{plus,minus}_wco`, `probe_z_minus_wco`, `probe_*_top_corner`, `probe_*_inside_corner`, `probe_round_boss`, `probe_round_pocket_center_start`, `probe_rect_pocket_center_start`, `probe_ridge_{x,y}`, `probe_valley_{x,y}_center_start`, `probe_top_left_edge_angle` (and its siblings), `probe_cal_round_pocket` | `#3014` to `#3033`; several end in G91 (millgui restores modes) |
| PROBE (this repo) | `probe_auto_stock_center/apply`, `probe_two_face/_apply`, `probe_a_*` (5), `probe_spindle_nose`, `tool_touch_off` | `mymill_probe_safe_move_*`, `mymill_probe_coordinate_ready`, `mymill_modes_*` (HAL latches again), `M50`, G30 |
| JOB | `mymill_probe_assert_no_contact`, `go_to_home`, the FIND routines above; `o<probe>`/`f360_*` in Fusion-posted files are refused | `#<_hal[mymill-job.*]>`, `#<_task>` |
| machine side, not millgui | `on_abort`, `m6_toolchange_optional_touch_off`, `python/toplevel.py` | M6 remap uses Python; that is the machine's choice and millgui must keep working without it |

What a stranger lacks: all of them. See 5 for what to ship.

### 4.10 Park, safe moves and run behaviour

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| H1 | Park position | `go_to_home`: Z top, then machine X0 Y0, then A0 | n/a | Z top only, then ask | setup: `machine.park` (`"machine-zero"`, `"g28"`, `[x, y]`) | machine zero may be the far corner or over the vise |
| H2 | Cycle-start modal reset | `G90 G17 G40 G80 G94 G91.1` sent before a program when they differ (`worker.rs:313`) | n/a | on | config: `run.reset_modes` | fine; some shops will want it off |
| H3 | Mode restore after `o<..> call` | always | n/a | on | n/a | fine |
| H4 | Travel check | refuses on soft-limit overrun; table only warns | B6 | on | config: `run.travel_check = "refuse" / "warn" / "off"` | fine |
| H5 | Table model | `machine.toml [model]`: PM-728VT (800 x 180 x 55, centre 215,-90, 3 slots, nose 380 above table) | X/Y limits give a centre; nothing gives the nose height | no model drawn, table check off, until `nose_above_table` is set | setup | a stranger sees a PM-728VT table under their part and gets "tool below table" warnings from wrong numbers |
| H6 | Run-from-line preamble | Z to machine top, rapid over, feed down | B5 | n/a | via B5 | see B5 |
| H7 | Probe loaded at cycle start | refused unless the program has `(MYMILL-PROBE` lines | F1 | n/a | via F1 | n/a |

### 4.11 Job, conversational and CAM import

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| J1 | Jobs folder | `PROGRAM_PREFIX/jobs` | A3 | same | n/a | fine |
| J2 | HAL component and interpreter | `mymill-job` pins; F8 | F8 | n/a | check row | job program aborts on its stamp check |
| J3 | Digital input for sync | `M66 E0 L0` (`sync_input: 0`) | `num_dio` | 0 | n/a | fine (E0 wait mode 0 does not read an input) |
| J4 | Blend tolerance | `G64 P0.001` mm in every op header (`compiler.rs:34`) | `RS274NGC_STARTUP_CODE` G64 | 0.001 mm is tight; default 0.01 | config: `job.blend_tolerance` | slow corners on stepper machines |
| J5 | Conv starting values | tools 14/23/110/7, 3200/4250/1800 rpm, feeds 900/500/300/120, all mm (`conv.rs:164`) | D1 | no tool preselected; rpm = min(default, max_rpm); feeds unchanged but converted for inch | code | fields prefilled with tools the user does not have |
| J6 | Units choice | MM/IN per op exists | B3 | machine units | n/a | fine |
| J7 | Fusion import | expects the shape written by `linuxcnc-probing.cps` (this repo's post) | n/a | n/a | docs; ship the post | other posts are refused with a reason, which is the designed behaviour |
| J8 | Thread presets | built in (metric) | n/a | n/a | later | n/a |
| J9 | Markers in built programs | `(MYMILL-JOB STAMP=`, `(MYMILL-PROBE`, `MYMILL-MEASURE` | n/a | n/a | code: accept both, write `MILLGUI-` | existing built jobs must still load: keep reading the old markers |

### 4.12 Display, input and remote

| # | Need | Today | Detect | Default | Set | If missing or wrong |
| --- | --- | --- | --- | --- | --- | --- |
| K1 | Screen size | drawn for 1920x1080, scaled to fit other windows (`main.rs:1015`); `run` forces `SLINT_SCALE_FACTOR=1` | window size | fit | config: `display.scale` | 1280x800 and 1024x768 panels are common on mills: the fit scale makes touch targets small. **verify** what 1280x800 and 1024x768 look like in the simulator and set a minimum supported size |
| K2 | Fullscreen | fullscreen unless `--windowed` | n/a | fullscreen | config: `display.fullscreen` | desktop users |
| K3 | Window system | `winit-x11` only (`app/Cargo.toml`) | `WAYLAND_DISPLAY` | X11 | code: enable the Wayland backend or state X11-only | LinuxCNC 2.9 images use X11; newer Debian defaults to Wayland |
| K4 | Renderer | `[MILLGUI]RENDERER` / `MILLGUI_RENDERER`, auto by GL driver name | EGL | auto | config | fine |
| K5 | Touch or mouse | hold-to-confirm, large targets | n/a | touch | config: `display.hold_ms` | fine |
| K6 | DRO format | 3 decimals linear, 2 rotary | B3 | 3 (mm) / 4 (inch) | derived | inch |
| K7 | Quick buttons | `[buttons]` mist, flood, drawbar, home_all | D7, modules | detected | setup | n/a |
| K8 | MDI library | `millgui/mdi.toml`, text tuned to the mill ("max 4250", "G30 = tool setter", mist only) | D1, D7, G3 | built-in generic copy with rpm and coolant lines generated | file override stays | misleading help text |
| K9 | Remote screen | `REMOTE_PORT` (off), `REMOTE_ROOT`; no access control | n/a | off | config `[remote]`; SETUP row shows "anyone on the network can jog" when on | n/a |
| K10 | Diagnostics | `MILLGUI_LOG_MESSAGES`, `MILLGUI_PERF` env | n/a | off | n/a | n/a |

## 5. What is specific to mymill, and what has to ship

For a standalone release the repo splits along this line.

**Stays in mymill (the PM-728VT profile):** `mymill.ini`, `mymill.hal`,
`millgui.ini`, `xhc-whb04b-6.hal`, `hal_components/probe_manager.comp`,
`outputs.hal`, `drives.hal`, `drawbar.hal`, `interlock.hal`, the nose
reference routine, `mymill_modes_*` and the mode-recovery latches, the M6
remap and its Python, the table model numbers, scratch tools, the deploy
script's default host, the simulator's `make-config` (it rewrites
`mymill.hal`).

**Moves into millgui:**

- The Rust workspace as it is.
- A macro set under `/usr/share/millgui/macros`, added to the user's
  `SUBROUTINE_PATH` by one INI line the checker prints. It must run on a
  machine with no extra HAL: no `mymill_modes_begin`, no M64 pulses, no
  `#<_hal[...]>` except what millgui itself provides. millgui already
  restores modal state after every `o<...> call` (`worker.rs:186`), which is
  what `mymill_modes_*` was for, so the generic versions of power feed,
  auto stock, two face and touch-off can drop it. Rename `mymill_*` subs to
  `millgui_*`.
- The stock probe routines. Today they come from the Probe Basic fork
  (`vendor/probe_basic`, a private-fork submodule pinned to a commit that is
  not on its remote). Options: (a) vendor the needed `.ngc` files from
  upstream Probe Basic into millgui with their licence and authorship
  (they are GPL; **verify** the exact licence text upstream); (b) require
  Probe Basic to be installed. Recommendation: (a). (b) defeats the point
  for users who "did not come from Probe Basic".
- A generic simulator config for tests: a plain 3-axis sim (LinuxCNC's
  `sim` configs as a base) with a probe input, plus an inch variant and an
  XYZA variant. The acceptance test for this whole plan is that millgui
  starts on each with an empty SETUP "blocks" list for core and zero HAL
  errors.
- The Fusion post (`linuxcnc-probing.cps`) if CAM import is advertised.

**Naming to change in code:** HAL component `mymill-job`, sub names
`mymill_powerfeed` and `mymill_probe_*`, markers `MYMILL-*` (J9),
`Machine::pm728vt()` as `Default` (`preview/machine.rs`), comments that
call the INI "Probe Basic's".

**Packaging facts a release needs (not settings, but they decide the
first-run story):**

- The NML shim links `libnml`/`liblinuxcnc.a` and must match the installed
  LinuxCNC's C++ ABI. Today that is done by copying a sysroot off the Pi.
  A release should build against the system's `linuxcnc-uspace-dev`
  (headers in `/usr/include/linuxcnc`) with plain `cargo build`, and ship
  `.deb`s per LinuxCNC series and architecture (2.9 amd64, 2.9 arm64).
  State the supported versions: 2.9.10 is what the Pi and the simulator
  run, and `sim/README.md` records that the 2.9 branch head changed
  M5/at-speed behaviour.
- Linking LinuxCNC's GPL libraries and shipping GPL macros constrains
  millgui's licence, and Slint's licence options need checking against it.
  The repo has no LICENSE file. **Decision for Gardner before any release.**
- millgui itself needs no Python. The simulator scripts and the mymill M6
  remap do; neither ships.

## 6. Build order

Each phase is shippable and tested in the simulator only. Phases 1 and 2
change behaviour on Gardner's mill as little as possible; the mymill
profile file lands in the same commit as anything that would otherwise
change it.

**Phase 1: make core honest (no new UI).**
1. Units: read `LINEAR_UNITS`; one `Units` value threaded to DRO format,
   jog speed labels, power feed, travel-check constants, preview. Decide
   per section 7 whether probing/conv/job run on inch machines yet; if not,
   block them with a reason.
2. Jog increments parsed properly (C1). Unparseable never means continuous.
3. Axis to joint map from COORDINATES/KINS (B2); rotary by joint type (B7, B8).
4. `machine.z_top` replaces every literal `G53 G0 Z0` and the `-5.0` rule (B5).
5. Default modules without `drives drawbar interlock`; `Machine::default()`
   draws no table.
6. Core calls no subroutine: PARK, UNLOAD, GO TO G30 become MDI lines built
   from config; power feed either inlined as MDI or kept behind a
   "macros installed" check.
7. Tests: unit tests for each parser; simulator runs on the stock mm, inch
   and XYZA configs.

**Phase 2: `millgui.toml`, checker, module split.**
1. Config struct, load/merge/write (`toml_edit`), `machine.toml` fallback.
2. `checks.rs` with every row from section 4 that has "check row" or
   "setup"; `millgui --check`.
3. `toolchange` module split from `drawbar` (E1).
4. `[hal]` signal map and generated nets; mymill profile's map; delete the
   need for `hal/*.hal` on stock machines.
5. Feature gating from check results (disabled buttons with reasons).

**Phase 3: SETUP screen.** Rows, fixes, the guided steps, live probe and
setter tests, first-run opening. Reviewer pass in the simulator with
screenshots, as for every millgui milestone.

**Phase 4: probe and setter independence.** millgui-owned probe defaults
written per run (F5), generic macro set (5), probe tool number in one
place (F1), generic setter reference (G4), `millgui-job` rename with old
markers still read.

**Phase 5: release shape.** Repo split, generic simulator configs in CI,
native build, packages, docs ("your first hour with millgui" is the
`--check` output explained).

## 7. Decisions for Gardner

1. **Inch machines.** Recommendation: phase 1 makes core (DRO, jog,
   limits, preview, travel check, MDI) correct in either unit, and blocks
   PROBE, CONV and JOB on inch machines with a clear row until phase 4
   verifies the macros and defaults in inches. The alternative, "mm only
   for now", is less work but must then refuse to start on an inch INI
   rather than run with wrong numbers.
2. **Probe macros.** Recommendation: vendor the needed Probe Basic `.ngc`
   files into millgui (5, option a), licence permitting.
3. **Licence.** Needed before a public repo; it also decides option 2.
4. **INI edits.** Recommendation: millgui prints the lines; an explicit
   `--write-ini` flag may apply them with a backup while LinuxCNC is
   stopped. The GUI never edits the INI.
5. **When to split the repo.** Recommendation: after phase 4, when millgui
   starts clean on a stock simulator config. Splitting earlier means two
   repos change together for every phase.

## 8. Not checked

- Nothing was run. Every "today" cell is from reading the code at `6c68340`.
- The Probe Basic macros were not readable here (the submodule is not
  checked out), so their own INI/HAL needs and unit handling are unknown.
  Read them before phase 4.
- LinuxCNC's rule for persisting numbered parameters (F5), `halcmd -s`
  output format (4.8) and `[SPINDLE_0]` key names across versions (D1) are
  from memory of LinuxCNC's documentation, not tested.
- Screen sizes below 1920x1080 (K1) were not rendered.
