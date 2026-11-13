# millgui requirements

From the final mockup (`docs/mockup-v16.html`, artifact
https://claude.ai/artifact/NUsDy6ER3vuD5qTEN5ivQ3) and the review handoff of
2026-11-13. Milestones are in ARCHITECTURE.md. `[x]` = built.

## Frame (M1)

- [x] E-STOP trips on first touch; RESET and POWER need the 800 ms hold.
- [x] HOME ALL (hold) in the status bar while not homed; e-stop unhomes (VOLATILE_HOME).
- [x] Homed pill never implies A is referenced (A has no switch: "A REF").
- [x] Spindle and drawbar pills show commanded state.
- [x] MOTION HELD: <reason> (motion.feed-inhibit = probe fault OR drives not ready).
- [x] Prompt for M5 and a fresh start when spindle_drawbar_guard.rearm_required is set
      (needs the current mymill.hal on the Pi; the Pi's copy predates the pin).
- [x] Tapping an override % resets that row; rapid steps 25%.
- [x] EXIT (hold) in the status bar ends the session; disabled while running.
- [x] Popups cover only the middle, so E-STOP, power, feed hold, stop, cycle start stay usable.
- [ ] Follow mode changes made by the WHB04B pendant (halui); pendant pill.
- [x] Message drawer for (abort), (MSG), (PRINT), (DEBUG) (tap the message line).

## M2 Manual control

- [x] Jog: step/continuous, speed in mm/s (DEFAULT_LINEAR_VELOCITY 50 is mm/s), A at its own 180°/s.
- [x] "A: set reference here".
- [x] MIST M7/M9; Z TO WORK Z0 and RETRACT 10, XY TO WORK 0 on hold.
- [x] Power feed parity with powerfeed.py/custom_config.yml: own WCS (powerfeed.wcs-index),
      remembered feed/distance/RPM, GOTO list with save/update/delete and XY preview.
      END OF TRAVEL needs a hold and disables Z. (Settings and GOTO list persist in
      ~/.local/state/millgui/settings; Probe Basic kept GOTO points in memory only.)
- [ ] Spindle start preflight from powerfeed.py (_spindle_start_block_reason: probe selected,
      drawbar requested/extended, rearm) as a clear message; HAL still enforces it.
- [ ] Jog, power feed, spindle, offsets, tools and probe locked while running or paused.
- [x] MDI with history; keypad overlay.
- [x] MDI screen per mockup v17: command line, help line (library match), categories with
      RECENT as history, code cards tagged MOVES, G-code keypad; library in millgui/mdi.toml
      ([MILLGUI]MDI_LIBRARY overrides; a built-in copy is the fallback).

## M3 Run and files

- [x] Backplot from stand-alone rs274 (private HOME, copied var/tool files, REMAPs stripped) on a
      background thread; tiny-skia software renderer; ISO/TOP/FRONT, colours per op, tool marker and trail.
- [ ] Backplot: GPU renderer choice at startup if software rendering is too slow on big programs.
- [x] Run: G-CODE | MDI | JOG tabs; JOG keeps the backplot visible (mockup v18). JOG nav renamed MANUAL
      (POWER FEED, GO TO, spindle card); MODULES accepts the old "jog" id.
- [x] Files: no USB; browse PROGRAM_PREFIX (G-code and folders, newest first), preview, LOAD.
- [ ] Files: ops and estimated time per file; top-view preview plus op list (needs the interpreter).
- [x] Run: G-code view follows the executing line; NEXT M6 finds the next tool change.
- [ ] CYCLE START has a static outline when it can resume.
- [ ] Run from line: refused inside a job (job_session.cycle_start_problem); design restoring
      tool/spindle/offsets properly or drop it (it is not LinuxCNC behavior).
- [ ] Decide whether [FILTER] keeps .py.

## M4 Tooling

- [x] Tool change matches toolchange_dialog.py and m6_toolchange_optional_touch_off.ngc:
      seated checkbox gates RESUME; RESUME + TOUCH OFF sets tool-touch-off-request-latch and
      the remap runs tool_touch_off.ngc; disabled for T99; ABORT while released retracts the
      drawbar; T200–249 warn "touch off likely required"; show the spindle nose reference state.
- [x] Manual drawbar extend outside M6, and M61 Q0, on Tools.
- [x] Tool table add/delete rows and scratch tools T200–249.
- [x] Offsets: columns match XYZR; ZERO HERE and CLEAR need a hold; active WCS tagged;
      G92/G52 indicator with CLEAR.
- [ ] Check on the Pi whether LinuxCNC rewrites PARAMETER_FILE after G10 while running;
      inactive WCS rows come from it at startup and from edits made in millgui.

## M5 Probing and util

- [x] Per-run probe values: write #30xx before the macro, restore defaults after success OR
      abort (on_abort.ngc does not), and at startup.
- [x] Preconditions from mymill_probe_coordinate_ready.ngc: no cutter comp, no rotated WCS,
      zero G52/G92; check #3031 before offering.
- [x] spindle-nose-reference-valid resets on any unhome; probe_spindle_nose.ngc needs T0 and an
      invalid reference; show VALID/NEEDS PROBE in Util and tool change; gate CAL → SPINDLE NOSE on T0.
- [x] Tool setter is G30 (#5181–5183) via set_g30_position.ngc; go-to G30 and park.
- [x] Probe screen per mockup (START pads, steppers, MORE, DEFAULTS, mode-matched label,
      Write-to only in FIND); probe wired as T99.
- [x] Warm-up with editable stage minutes and RPM CMD progress.
- [ ] Probe preview animation (the mockup's 1× loop); the picture shows the approach arrows only.
- [ ] Warm-up holds the spindle override at 100 % and stops if it changes; util.py disables the
      override instead (needs a spindle-override-enable command in crates/linuxcnc).
- [ ] Check the macro EVAL result names and the probe_auto_stock_apply context check on the Pi.

## M6 Job and conversational

- [ ] Job rules from job_session.py: START AT OP disables earlier ops and needs a rebuild;
      refuse a stale or unbuilt job.

## M8 Remote web (see ARCHITECTURE.md, Remote access)

- [x] Slint wasm build of the same UI (software renderer into a canvas, fills and resizes with
      the window); Live/Actions mirrored over a websocket (uimirror generates the glue).
- [x] `remote` on the Pi ([MILLGUI]REMOTE_PORT): serves the bundle (gzip), streams property diffs
      at 20 Hz and the backplot PNG at 4 Hz.
- [x] Remote control enabled for now (Gardner, 2026-11-13); access policy (read-only default, grant) later.
- [x] Remote jogs step-only (the Pi refuses CONT); remote E-STOP labelled as a request.
- [x] Slow or dropped clients never stall the worker or the touchscreen (merged pending slot per client).
- [ ] Access policy (read-only default, grant); show who is connected on the touchscreen.

## Before calling parity done

Not reviewed yet: vendor probe_basic main .ui, cust_probe, program_tools, docs/, mymill_job compiler.
