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
- [ ] Popups cover only the middle, so E-STOP, power, feed hold, stop, cycle start stay usable.
- [ ] Follow mode changes made by the WHB04B pendant (halui); pendant pill.
- [ ] Message drawer for (abort), (MSG), (PRINT), (DEBUG).

## M2 Manual control

- [ ] Jog: step/continuous, speed in mm/s (DEFAULT_LINEAR_VELOCITY 50 is mm/s), A at its own 180°/s.
- [ ] "A: set reference here".
- [ ] MIST M7/M9; Z TO WORK Z0 and RETRACT 10, XY TO WORK 0 on hold.
- [ ] Power feed parity with powerfeed.py/custom_config.yml: own WCS (powerfeed.wcs-index),
      remembered feed/distance/RPM, GOTO list with save/update/delete and XY preview.
      END OF TRAVEL needs a hold and disables Z.
- [ ] Jog, power feed, spindle, offsets, tools and probe locked while running or paused.
- [ ] MDI with history; keypad overlay.

## M3 Run and files

- [ ] Backplot from the interpreter shim; GPU or software renderer chosen at startup; flat visuals.
- [ ] Files: no USB; list shows ops, time and date; backplot plus op list.
- [ ] CYCLE START has a static outline when it can resume.
- [ ] Run from line: refused inside a job (job_session.cycle_start_problem); design restoring
      tool/spindle/offsets properly or drop it (it is not LinuxCNC behavior).
- [ ] Decide whether [FILTER] keeps .py.

## M4 Tooling

- [ ] Tool change matches toolchange_dialog.py and m6_toolchange_optional_touch_off.ngc:
      seated checkbox gates RESUME; RESUME + TOUCH OFF sets tool-touch-off-request-latch and
      the remap runs tool_touch_off.ngc; disabled for T99; ABORT while released retracts the
      drawbar; T200–249 warn "touch off likely required"; show the spindle nose reference state.
- [ ] Manual drawbar extend outside M6, and M61 Q0, on Tools.
- [ ] Tool table add/delete rows and scratch tools T200–249.
- [ ] Offsets: columns match XYZR; ZERO HERE and CLEAR need a hold; active WCS tagged;
      G92/G52 indicator with CLEAR.

## M5 Probing and util

- [ ] Per-run probe values: write #30xx before the macro, restore defaults after success OR
      abort (on_abort.ngc does not), and at startup.
- [ ] Preconditions from mymill_probe_coordinate_ready.ngc: no cutter comp, no rotated WCS,
      zero G52/G92; check #3031 before offering.
- [ ] spindle-nose-reference-valid resets on any unhome; probe_spindle_nose.ngc needs T0 and an
      invalid reference; show VALID/NEEDS PROBE in Util and tool change; gate CAL → SPINDLE NOSE on T0.
- [ ] Tool setter is G30 (#5181–5183) via set_g30_position.ngc; go-to G30 and park.
- [ ] Probe screen per mockup (START pads, steppers, MORE, DEFAULTS, mode-matched label,
      Write-to only in FIND); probe wired as T99.
- [ ] Warm-up with editable stage minutes and RPM CMD progress.

## M6 Job and conversational

- [ ] Job rules from job_session.py: START AT OP disables earlier ops and needs a rebuild;
      refuse a stale or unbuilt job.

## Before calling parity done

Not reviewed yet: vendor probe_basic main .ui, cust_probe, program_tools, docs/, mymill_job compiler.
