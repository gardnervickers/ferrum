# millgui.toml

millgui's own settings for one machine. LinuxCNC's settings (axes, units,
limits, velocities, spindle speeds, paths) stay in the INI, and millgui only
reads them. `millgui.toml` holds what LinuxCNC does not know.

It lives beside the INI. `[MILLGUI]CONFIG = <path>` in the INI moves it.
Every key is optional: leave one out and millgui uses what it detects, or
its default. A table with a bad key is reported (the message bar and
`millgui --check`) and only that table falls back to its defaults.

Without a `millgui.toml`, millgui still reads the older places: the INI's
`[MILLGUI]` keys (`MODULES`, `POSTGUI_HALFILE`, `REMOTE_PORT`,
`REMOTE_ROOT`, `RENDERER`, `MDI_LIBRARY`) and `millgui/machine.toml` (or
`[MILLGUI]MACHINE_CONFIG`) for `[buttons]` and `[model]`. When both name a
setting, `millgui.toml` wins.

Check what millgui makes of a machine, with or without LinuxCNC running:

```sh
millgui --check ~/linuxcnc/configs/mymachine/mymachine.ini
```

It prints what was read, what is missing and what is switched off because
of it, and exits non-zero only when millgui cannot run the machine at all.
The same check runs when millgui starts: PROBE, JOB and CONV show the reason
in place of the screen when they are off.

## Keys

```toml
[machine]
z_top = -0.5              # machine Z that retracts go to; default: just under [AXIS_Z]MAX_LIMIT
park = "machine-zero"     # PARK after Z is up: "machine-zero" (X0 Y0, rotary 0), "g28", or [x, y]

[modules]
# In nav order. Default: run job manual probe tools offsets conv files util,
# plus toolchange when nothing in the machine's HAL answers M6.
# drives, drawbar, interlock, pendant and load need matching HAL.
enabled = "run manual files offsets tools util"

[spindle]
max_rpm = 3000            # default [SPINDLE_0]MAX_FORWARD_VELOCITY; speeds above it are refused
reverse = false           # default: false when [SPINDLE_0]MAX_REVERSE_VELOCITY = 0; hides CCW

[toolchange]
style = "dialog"          # "dialog" | "external" | "drawbar"; default detected (below)
scratch_tools = [200, 249]  # TOOLS > NEW SCRATCH numbers; a change to one warns about touch-off

[probe]
tool = 99                 # default [PROBE]TOOL_NUMBER, else 99

[toolsetter]
enabled = true            # default: on when UTIL is

[hal]                     # logical name = a HAL signal or pin of your machine
probe_tripped = "motion.probe-input"

[remote]
port = 8088               # browser screen; anyone who can reach the port can run the machine
root = "millgui/web/dist"

[display]
renderer = "auto"         # "gpu" | "software" | "auto"
mdi_library = "millgui/mdi.toml"
postgui = ["millgui/hal/outputs.hal"]   # HAL files run after millgui's pins exist

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

## Tool change

Who answers M6 (`iocontrol.0.tool-change`):

| style | when | millgui does |
| --- | --- | --- |
| `external` | something in the machine's HAL already drives `iocontrol.0.tool-changed` (an ATC, `hal_manualtoolchange`, a loop) | shows the tool number |
| `dialog` | nothing does | the `toolchange` module answers with the tool change dialog |
| `drawbar` | the `drawbar` module is on (mymill) | the dialog with the power drawbar and touch-off |

Detected in that order of evidence: `[toolchange]style`, then the `drawbar`
module, then what HAL drives `iocontrol.0.tool-changed`.

## HAL names

Modules connect their pins themselves. Each pin has a logical name; `[hal]`
maps it to a signal or pin of your machine, and some have a default. A
target that is a signal is joined; a pin on a signal joins that signal; a
pin on no signal gets a new `millgui-...` signal. A module with its own
`millgui/hal/<module>.hal` beside the INI uses that file instead, and only
the names listed in `[hal]` are connected on top.

| module | name | default | what |
| --- | --- | --- | --- |
| probe | `probe_tripped` | `motion.probe-input` | the probe input |
| probe | `probe_loaded` | none: the probe tool number in the spindle | a wireless probe is selected |
| probe | `probe_fault` | none | the probe reports a fault |
| probe | `toolsetter_tripped` | none | the tool setter input |
| probe | `setter_blast`, `setter_blast_active` | none | tool setter air blast request and state |
| toolchange | `tool_change`, `tool_number`, `tool_changed` | `iocontrol.0.tool-change`, `iocontrol.0.tool-prep-number`, `iocontrol.0.tool-changed` | the M6 handshake |
| toolchange | `spindle_on` | `spindle.0.on` | RESUME waits for the spindle to be off |
| interlock | `motion_held` | `motion.feed-inhibit` | why motion is held |
| interlock | `probe_fault`, `drives_not_ready` | none | the reasons |
| load | `load` | none | cutting load, 0 to 1, colours the backplot trail |
