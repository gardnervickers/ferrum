# millgui against the Pi's running HAL (2026-11-14)

A read-only review by an independent reviewer, checked and summarised here.
Nothing was run on the mill and nothing was sent to it. The Pi's HAL was read
from git blob `6d3316a9` (the copy reconstructed from the Pi), cited as
"Pi:N". The Pi's own `mymill.ini`, subroutines, Python remap and postgui file
were not visible, so every item that depends on them is marked unverified.

## Decisions applied (Gardner, 2026-11-14)

- Item 1: `millgui/hal/outputs.hal` sets the two 7i76E values and
  `millgui.ini` runs it through `[MILLGUI]POSTGUI_HALFILE`. The values are
  the repo's `probe_basic_postgui.hal`; the Pi's copy was not read. Not yet
  run on the mill.
- Item 3: the A axis no longer holds back CYCLE START or power feed. The
  pill still reports it.
- Remote control: `REMOTE_PORT` is commented out in `millgui.ini`.

## Result

All 22 nets in `millgui/hal/*.hal` match the Pi HAL: every signal exists with
the same name, type and direction, none gains a second writer, and none is
created by a typo. The manual tool change handshake is equivalent to the
Probe Basic dialog's. No HAL input that gates motion, the spindle or probing
came from the old GUI, except `tool-changed`, which millgui replaces.

What HAL enforces whatever the GUI does (required for correctness, and
unchanged): the drawbar energises only with `spindle-enable` off (Pi:528-531),
the drawbar or T99 inhibits the spindle (Pi:533-536), HLFB faults, and the
e-stop chain. If millgui or its drawbar module is absent, M6 waits forever,
which is the safe outcome.

## Check before a first real job

### 1. The 7i76E output setup from `probe_basic_postgui.hal` is not applied

- Probe Basic runs `probe_basic_postgui.hal`, which sets
  `hm2_7i76e.0.7i76.0.0.output_source 0x0000F7FF` and
  `output_sink 0x00000800` (lines 25-26 of the repo copy).
- millgui runs only `millgui/hal/<module>.hal` and
  `[MILLGUI]POSTGUI_HALFILE`, which is commented out (`millgui.ini:31`).
- Consequence: unknown. If the card's defaults differ, field outputs behave
  differently (drive enable 00/01, air 08, drawbar 09, mist 10, probe enable
  11). Only output 11 is switched to sinking, so it may be harmless.
- Class: required for correctness of the outputs if the defaults differ.
- Fix: put the two `setp` lines in a `millgui_extra.hal` named by
  `[MILLGUI]POSTGUI_HALFILE` (no change to `mymill.hal`). Not done here: it
  needs the Pi's copy of the postgui file, and it changes what runs on the
  mill.

### 2. The Pi's subroutines must match its HAL

- millgui calls `tool_touch_off`, `probe_spindle_nose` and
  `mymill_powerfeed`. `scripts/deploy` copies no subroutines, so the Pi's own
  copies run.
- The branch's copies call `mymill_modes_begin`, which reads a pin the Pi HAL
  does not have. The Mac working-tree copies need `[PROBE]TOOL_NUMBER`, which
  the branch's `mymill.ini` lacks.
- These are the same macros Probe Basic uses on the Pi, so a touch off that
  works there is good evidence. The six-argument `mymill_powerfeed` call is
  the part to confirm.
- Class: availability (a mismatch fails with an error; it does not move).

### 3. Nothing runs until every axis, A included, reports HLFB ready

- `modules.rs` `Drives::not_ready`, enforced for CYCLE START and power feed.
- If the A drive is unplugged, `a-hlfb-monitor-arm` (Pi:458) never goes true
  and millgui refuses to start anything. HAL itself does not require A.
- Class: availability. This gate is new in millgui; it is a safeguard, not
  something HAL needs.
- Decision for Gardner: exempt A, or leave `drives` out of `MODULES` when the
  rotary is off the table.

## Fixed on this branch

- **CLAMP TOOL with a spindle command pending** (safeguard). The quick
  button and TOOLS clamped without checking the spindle command; the tool
  change overlay did check. The Pi HAL has no re-arm latch, so a pending M3
  would start the spindle as the drawbar retracts. Clamp is now refused with
  a message until the command is off. Unverified: whether `spindle.0.on`
  stays true while the spindle is inhibited; if it does not, this could not
  happen and the check is harmless.
- **Probe tool default** (availability). TOOLS and the tool change overlay
  defaulted the probe tool to 0 when `[PROBE]TOOL_NUMBER` is missing, where
  the rest of millgui uses 99. RESUME + TOUCH OFF could have stayed disabled.
  Both now default to 99.

## Open, for a decision

- **Remote control has no authentication** (safeguard). `REMOTE_PORT = 8088`
  listens on every interface and admits every action. Gardner chose to allow
  it for now; removing the key for the first real job is the cautious option.

## Unchanged from Probe Basic, worth knowing

- The pendant's `halui.program.run` bypasses millgui's CYCLE START checks,
  as it bypassed Probe Basic's.
- The drawbar interlock keys on the spindle command, not on standstill, so
  the drawbar can release while the spindle coasts.
- A HAL file line that fails under `halcmd -k` shows only as a note in the
  status line at startup.

## At the machine, spindle off

1. Read the startup status line for any "HAL ...hal: line N" note.
2. `halcmd show sig tool-change-confirmed`: exactly one writer,
   `millgui.drawbar.tool-changed`. Repeat for `millgui-drawbar-manual` and
   `millgui-touch-off-set`.
3. `halcmd show param hm2_7i76e.0.7i76.0.0.output_source` and
   `output_sink`, compared with a Probe Basic session.
4. On the Pi: `grep -l mymill_modes subroutines/*.ngc` and
   `grep -A1 '\[PROBE\]' mymill.ini`.
5. Machine on: the pill reads DRIVES HLFB OK, A included.
6. MDI `T1 M6`: the overlay opens; RELEASE and CLAMP move the drawbar;
   RESUME completes; ABORT leaves it clamped.
7. Load T99: the spindle refuses to start and CYCLE START is refused.
8. Trip the probe by hand: the pill changes.
9. One power feed move, then an air cut.

## Not verified

- The Pi's `mymill.ini`, subroutines, Python remap and postgui file.
- The 7i76E `output_source` and `output_sink` defaults.
- `spindle.0.on` behaviour while inhibited.
- Whether the A drive is connected.
- millgui's probe preflight against `cust_probe.py`, line by line.
