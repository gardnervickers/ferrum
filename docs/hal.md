# Ferrum and HAL

Ferrum reads position, modes, overrides, coolant, the tool and the limits
from LinuxCNC directly (NML), so a working LinuxCNC config runs it with
nothing added to HAL. HAL is only for what NML cannot see: the probe, the
spindle's real speed, a tool setter, a drawbar, drive or lube alarms, a
pendant.

Every Ferrum pin is listed by `ferrum --hal-pins` (give it your INI to see
only your axes). All of them exist every time Ferrum starts, whatever
screens and modules are on, so your own HAL files can always `net` them.

## Three kinds of pins

**Required: the screen watchdog.** Ferrum flips `ferrum.watchdog.heartbeat`
once per frame it draws. If no frame is drawn for a second (a frozen, hung
or crashed screen), a realtime timer E-stops the machine and holds it there
until E-stop is reset. Ferrum wires this itself: it gates
`iocontrol.0.emc-enable-in`. If your HAL already has an E-stop latch, point
the watchdog at its fault input instead:

```toml
[hal]
screen_watchdog_trip = "estop-latch.0.fault-in"
```

It is recommended, but it can be turned off (SETUP › MACHINE, or
`[machine]screen_watchdog = false`).

**Standard: joined by Ferrum.** These have a LinuxCNC pin every config
has. Ferrum joins whatever signal already reaches that pin, so your signal
names do not matter. If nothing drives the pin (for example
`sets spindle-at-speed true`), the part counts as not fitted and Ferrum
hides what depends on it.

| `[hal]` name | joins | what Ferrum shows |
| --- | --- | --- |
| `probe_tripped` | `motion.probe-input` | probe lamp, live test |
| `spindle_speed_actual` | `spindle.0.speed-in` (rev/s) | RPM ACT / CMD |
| `spindle_at_speed` | `spindle.0.at-speed` | the RPM in green, amber while it comes up |
| `spindle_inhibited` | `spindle.0.inhibit` | SPINDLE INHIBITED |
| `lube_ok` | `iocontrol.0.lube_level` | LUBE LOW |
| `motion_held` | `motion.feed-inhibit` | MOTION HELD (interlock module) |
| `tool_change`, `tool_number`, `tool_changed`, `spindle_on` | `iocontrol.0.tool-change`, `tool-prep-number`, `tool-changed`, `spindle.0.on` | the tool change dialog, when Ferrum answers M6 |
| `pendant_*` | the `whb.*` pins of the WHB04B driver (xhc-whb04b-6) | the pendant module |

**Optional: named by you.** No LinuxCNC pin exists for these, so only you
can say which signal it is. Each is off until `[hal]` names it:
`probe_loaded`, `probe_fault`, `toolsetter_tripped`, `setter_blast`,
`setter_blast_active`, `spindle_fault`, `load`, the drawbar's,
`drive_ready_<axis>`, `drive_fault_<axis>`, `drives_not_ready`, and the
panel's below.

### Panel buttons, knobs and lights

Hardware buttons go through the same checks as the screen's own buttons
(CYCLE START only in AUTO with a program loaded, and so on).

| `[hal]` name | Ferrum pin | what |
| --- | --- | --- |
| `cycle_start`, `feed_hold`, `stop` | `ferrum.panel.cycle-start`, `feed-hold`, `stop` | buttons, acted on as they are pressed |
| `single_block` | `ferrum.panel.single-block` | a button: each press turns SINGLE BLOCK on or off |
| `feed_override_counts`, `spindle_override_counts`, `rapid_override_counts` | `ferrum.panel.*-override-counts` (s32) | an encoder: 1 % per count |
| `unlock` | `ferrum.panel.unlock` | a key switch: while it is wired and off, SETUP is read only |
| `mpg_axis_x` … `mpg_axis_c` | `ferrum.mpg.axis-<axis>` (out) | the axis last jogged on MANUAL, for an MPG's axis select |
| `mpg_increment` | `ferrum.mpg.increment` (out) | MANUAL's jog step, machine units |
| `light_error`, `light_waiting`, `light_running` | `ferrum.status.error`, `waiting`, `running` (out) | a stack light: red for E-stop or a fault, amber while paused or waiting, green while a program runs |

### Alarms

Any other machine input can be shown as an alarm:

```toml
[[alarm]]
pin = "air-pressure-low"     # a bit signal or pin, "!" inverts
text = "Air pressure low"
kind = "warn"                # info, warn (default) or stop
```

Each `[[alarm]]` gets a pin `ferrum.alarm.<n>` joined to `pin`. While it is
on, the top bar shows `text`. A `stop` alarm also holds the program (feed
hold) as it comes on and refuses motion until it clears.

A name given in `[hal]` always wins over the default, and `"none"` says
your machine has no such thing (which also hides the default).

## Writing `[hal]`

```toml
[hal]
toolsetter_tripped = "tool-setter-in"     # a signal
probe_loaded = "probe-manager.selected"   # or a pin
spindle_fault = "!vfd-ok"                 # "!" inverts a bit
load = { target = "vfd-output-current", scale = 0.125 }   # float: in × scale + offset
spindle_at_speed = "none"                 # this machine has no at-speed
```

Ferrum checks each entry before connecting it: the name must exist, its
type must match the Ferrum pin, and a pin Ferrum drives may only go to an input or a signal nothing
else writes. Problems are listed on the message line, in
`ferrum --check`, and on SETUP › HAL WIRING, which also lets you pick each
name from your machine's own pins and signals with their live values.

An invert or a scale is done in HAL by a `not` or `scale` instance Ferrum
loads for you (`ferrum-not-<pin>`, `ferrum-scale-<pin>`), so halshow shows
exactly what happens.

## Your own HAL files

`[display]postgui` lists HAL files Ferrum runs after its pins exist, for
anything `[hal]` does not cover:

```toml
[display]
postgui = ["my-postgui.hal"]
```

LinuxCNC's own `[HAL]POSTGUI_HALFILE` is written for another screen's
pins, so Ferrum does not run it. SETUP (or `ferrum --convert-postgui`)
converts it once: lines that only another screen understands are left
out, each with the reason as a comment; a `net` keeps its machine pins and
loses the other screen's; everything else is copied to
`ferrum-postgui.hal` beside the INI, which is added to `[display]postgui`.
`ferrum.toml` is kept as `ferrum.toml.bak` first, and the old files are
not touched, so another screen can still use them.

## Older profiles

A `ferrum.toml` without a `[hal]` table still runs the per-module files
`ferrum/hal/<module>.hal` beside the INI, as before. Adding `[hal]` turns
them off. They will be removed in a later release.
