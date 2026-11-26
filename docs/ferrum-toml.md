# ferrum.toml

Ferrum's own settings for one machine. LinuxCNC's settings (axes, units,
limits, velocities, spindle speeds, paths) stay in the INI, and Ferrum only
reads them. `ferrum.toml` holds what LinuxCNC does not know.

It lives beside the INI. `[FERRUM]CONFIG = <path>` in the INI moves it.
Every key is optional: leave one out and Ferrum uses what it detects, or
its default. A table with a bad key is reported (the message bar and
`ferrum --check`) and only that table falls back to its defaults.

Without a `ferrum.toml`, Ferrum still reads the older places: the INI's
`[FERRUM]` keys (`MODULES`, `POSTGUI_HALFILE`, `REMOTE_PORT`,
`REMOTE_ROOT`, `RENDERER`, `MDI_LIBRARY`) and `ferrum/machine.toml` (or
`[FERRUM]MACHINE_CONFIG`) for `[buttons]` and `[model]`. When both name a
setting, `ferrum.toml` wins.

The SETUP screen (always last in the nav bar) edits it for you, on nine
pages: MACHINE, SPINDLE, TOOL CHANGE, PROBE and TOOL SETTER (the same
checks as `ferrum --check`, the settings each needs, live tests of the
probe and tool setter inputs), then SCREENS (which screens are in the nav
bar and in what order, and the machine modules), HAL WIRING (each `[hal]`
name, picked from the machine's own pins and signals with their values
shown live), DISPLAY (quick buttons, the backplot table, the renderer, the
MDI library, the browser screen) and SOFTWARE (the version and its updates,
through the installer's `ferrum-update`). A change is written to
`ferrum.toml` at once, keeping your comments; the quick buttons change at
once, the rest when LinuxCNC next starts. With no `ferrum.toml`, SETUP
opens first; FINISH writes the file so it stops doing that.

The saved changes wait on a bar under the pages. When Ferrum started
LinuxCNC (`ferrum --boot`), RESTART LINUXCNC (hold; only with the machine
off and no program running) stops LinuxCNC, adds the INI lines queued with
ADD TO INI on a check's row (keeping the old INI as `<ini>.bak`), and starts
everything again, through the installer's launcher when there is one, so a
downloaded update goes into use at the same time.

Check what Ferrum makes of a machine, with or without LinuxCNC running:

```sh
ferrum --check ~/linuxcnc/configs/mymachine/mymachine.ini
```

It prints what was read, what is missing and what is switched off because
of it, and exits non-zero only when Ferrum cannot run the machine at all.
With `--write-ini` (LinuxCNC stopped) it also adds the INI lines it asks
for, such as `HAL_PIN_VARS = 1`, keeping the old file as `<ini>.bak`. On
the screen, SETUP shows the lines and writes them only as part of RESTART
LINUXCNC.
The same check runs when Ferrum starts: PROBE, JOB and CONV show the reason
in place of the screen when they are off.

## Keys

```toml
[machine]
z_top = -0.5              # machine Z that retracts go to; default: just under [AXIS_Z]MAX_LIMIT
park = "machine-zero"     # PARK after Z is up: "machine-zero" (X0 Y0, rotary 0), "g28", or [x, y]

[modules]
# In nav order. Default: run job manual probe tools offsets conv files util,
# plus toolchange when nothing in the machine's HAL answers M6. SETUP is
# always added last.
# drives, drawbar, interlock, pendant and load need matching HAL.
enabled = "run manual files offsets tools util"

[spindle]
max_rpm = 3000            # default [SPINDLE_0]MAX_FORWARD_VELOCITY; speeds above it are refused
reverse = false           # default: false when [SPINDLE_0]MAX_REVERSE_VELOCITY = 0; hides CCW

[toolchange]
style = "dialog"          # "dialog" | "external" | "drawbar"; default detected (below)
scratch_tools = [200, 249]  # TOOLS > NEW SCRATCH numbers; a change to one warns about touch-off

[probe]
enabled = true            # false: no probe, PROBE leaves the nav bar
tool = 99                 # default [PROBE]TOOL_NUMBER, else 99; the macros read it as #3014

[probe.defaults]          # the probe macros' values (machine units); see "Probe values"
slow_feed = 25            # #3015, per minute
fast_feed = 300           # #3016
traverse_feed = 1500      # #3017
max_xy = 15               # #3018 search distance
xy_clearance = 5          # #3019
max_z = 15                # #3020 search distance
z_clearance = 5           # #3021
extra_depth = 0           # #3022
step_off = 10             # #3023
edge_width = 20           # #3024
calibration_offset = 0    # #3032, written by CAL
ring_gauge = 25           # #3033
pin_gauge = 10            # #3034, the pin CAL › PROBE TIP › PIN uses (was cal_block_x)
cal_block_y = 25.4        # #3035

[toolsetter]
enabled = true            # default: on when UTIL is
fast_feed = 300           # #3004, per minute
slow_feed = 25            # #3005
traverse_feed = 1500      # #3006
max_z = 120               # #3007 travel down from G30; no default
retract = 2               # #3009
reference_height = 286.9  # #3010 spindle zero height; no default (CAL › nose writes it on mymill)
breakage_tolerance = 0.5  # #3037: TOOLS › CHECK and a job's TOOL CHECK stop beyond it (a 0 in the parameter file counts as not set)

[hal]                     # logical name = a HAL signal or pin of your machine
probe_tripped = "motion.probe-input"

[remote]
port = 8088               # browser screen; 0 = off. Anyone who can reach the port can run the machine
root = "ferrum/web/dist"

[display]
renderer = "auto"         # "gpu" | "software" | "auto"
mdi_library = "ferrum/mdi.toml"
postgui = ["ferrum/hal/outputs.hal"]   # HAL files run after Ferrum's pins exist
postgui_converted = ["custom_postgui.hal"]   # set by the one-time conversion of [HAL]POSTGUI_HALFILE (hal.md)

[buttons]                 # quick buttons under the overrides
mist = true
flood = true
drawbar = true
home_all = true

[model]                   # the table drawn in the backplot (machine units)
show = true
table = [800, 180, 55]    # length (X), depth (Y), thickness
table_center = [215, -90] # machine X Y with the spindle over the middle of the table
slots = 3
nose_above_table = 380    # spindle nose above the table at machine Z 0; 0 = no table
```

## Probe values

The probe and tool setter macros read their feeds and distances from
LinuxCNC parameters #3004 to #3037. Ferrum owns them: each comes from
`ferrum.toml`, else from the parameter file (a machine set up with Probe
Basic has them there), else a starting value for a 2 mm tip touch probe
(the values above, in mm; an inch machine gets them converted). SETUP lists
them under PROBE and TOOL SETTER, marks the starting values, and `--check`
says which are starting values or missing. The setter's max Z travel and
reference height have no starting value: touch-off stops until they are
set.

Ferrum writes the whole set to LinuxCNC once it is on, homed and idle
(when LinuxCNC does not already hold them), and before a probe run that
comes first. A value changed in SETUP or on PROBE › DEFAULTS, and a
calibration result, is saved here at once and used from the next run on.

SETUP and PROBE › DEFAULTS refuse a probe slow, fast or traverse feed
(#3015 to #3017) or a setter fast or slow feed (#3004, #3005) that could
not stop within 1 mm (0.04 in) of a touch: a probe stylus deflects only a
few mm before it breaks. The limit comes from the INI's
`[AXIS_X/Y/Z]MAX_ACCELERATION` (the slowest of them; Z only for the
setter) with 5 ms for the trip to reach motion; at 900/900/500 mm/s² it
is about 1750 mm/min. A value already in this file is not checked.

## Tool change

Who answers M6 (`iocontrol.0.tool-change`):

| style | when | Ferrum does |
| --- | --- | --- |
| `external` | something in the machine's HAL already drives `iocontrol.0.tool-changed` (an ATC, `hal_manualtoolchange`, a loop) | shows the tool number |
| `dialog` | nothing does | the `toolchange` module answers with the tool change dialog |
| `drawbar` | the `drawbar` module is on (mymill) | the dialog with the power drawbar and touch-off |

Detected in that order of evidence: `[toolchange]style`, then the `drawbar`
module, then what HAL drives `iocontrol.0.tool-changed`.

## HAL names

`[hal]` maps Ferrum's HAL pins to your machine's signals and pins, and
`[[alarm]]` adds alarm inputs. Both are described, with every pin, in
[hal.md](hal.md); `ferrum --hal-pins` lists the pins.

```toml
[machine]
screen_watchdog = true          # recommended; false turns the screen watchdog off

[hal]
screen_watchdog_trip = "estop-latch.0.fault-in"
toolsetter_tripped = "tool-setter-in"
spindle_fault = "!vfd-ok"
spindle_at_speed = "none"

[[alarm]]
pin = "air-pressure-low"
text = "Air pressure low"
kind = "warn"
```

A profile without a `[hal]` table runs the older per-module files
`ferrum/hal/<module>.hal` instead.

## Cutting load records

With the `load` module on and its pin wired, every program run in AUTO is
recorded: the load is sampled five times a second and summed per program
line, per tool and per JOB step. When the run ends, the RUN backplot shows
its peak (`PEAK 92% · LINE 212 · T3`; tap it to pick that line on the
G-code list) and the record is saved as JSON in
`~/.local/state/ferrum/loads/` (`<date>-<time>-<program>.json`, the newest
200 kept; the simulator's go to `~/.cache/ferrum/sim-loads/`). Without the
module, or with the pin missing on the machine, nothing is recorded or
shown.

A record (`"format": "ferrum-load", "version": 1`) has the program path,
when it started, the machine units (`mm` or `in`; feeds are units per
minute), `last_line` (the last line that ran, to tell a full run from one
stopped part way), `peak` (load, line, tool, JOB step), and `overall`,
`tools[]`, `ops[]` and `hot_lines[]` (the 100 lines with the highest
peaks, with their G-code), each with `peak` and `mean` load (0 to 1),
`seconds`, `hot_seconds` (at or over 80 %) and the mean `feed` and `rpm`
while sampled. A feeds calculator can read these to see where a job
worked the spindle hardest.
