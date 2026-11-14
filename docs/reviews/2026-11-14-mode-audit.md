# millgui mode and state audit (2026-11-14)

After PR #7 (HOME ALL did nothing after an E-stop because motion stayed in
teleop), every millgui command was checked for the same kind of assumption:
joint vs teleop, MANUAL/MDI/AUTO switches, E-stop and machine on, homed vs
unhomed, interpreter idle vs running or paused, and whether LinuxCNC's errors
reach the operator in full. Reference: LinuxCNC 2.9.10 `emctaskmain.cc` (which
commands task accepts in each state), `emctask.cc` (`emcTaskSetMode`), Axis
and QtPyVCP (Probe Basic).

Reproduced in the LinuxCNC 2.9.10 simulator (`millgui/sim`) running the Pi's
reconstructed `mymill.hal` (git blob `6d3316a9`) and the repo `mymill.ini`,
driven through millgui's remote socket. Nothing was sent to the mill.

## How LinuxCNC behaves (the rules millgui has to live with)

- Task mode changes are refused while an axis jogs ("Ignoring task mode
  change while jogging") and while AUTO is not idle.
- A program run (`EMC_TASK_PLAN_RUN`) received in MANUAL is accepted and
  dropped with no message.
- Switching to MDI or AUTO, even to the mode task is already in, calls
  `emcTaskAbort()`: it aborts motion and the interpreter. Task only blocks
  this when the current mode is AUTO and busy, so a switch while an MDI
  command runs aborts that command.
- MANUAL picks teleop when every joint is homed and free mode otherwise.
  E-stop unhomes X/Y/Z (VOLATILE_HOME) but leaves motion where it was.
- Homing only works in MANUAL and free mode (PR #7).
- `EMC_TASK_PLAN_PAUSE` is taken while idle too; task then reports PAUSED
  until a resume.
- Spindle on/off are refused while a program runs (Axis is the same).
  Coolant and the overrides work in every mode.

## Findings, by risk

| # | Finding | Risk | Status |
|---|---------|------|--------|
| 1 | Mode switch not checked: CYCLE START pressed while a jog step is still moving left task in MANUAL, and the run was dropped. The only message was LinuxCNC's "Ignoring task mode change while jogging". Same for single step, MDI and homing. | Medium: fails safe, but the operator is told the wrong thing | Fixed |
| 2 | The worker chose mode switches from the last periodic poll (up to 50 ms old, older within a batch of commands). A switch decided from stale status can land while something just started, and task aborts it. Seen as a redundant AUTO switch between PROGRAM OPEN and RUN in run-from-line; harmless there. | Medium (latent) | Fixed |
| 3 | LinuxCNC errors only appeared in the header, elided to a few words ("mus..." in PR #7). The full text was in the MESSAGES drawer only. | Medium | Fixed |
| 4 | The Pi's `mymill.hal` has no mode-recovery guard. With the repo's subroutines, power feed, spindle-nose probing and tool touch-off stop at once on the Pi HAL ("Named parameter #<_hal[mode-recovery-pending.out]> not defined"; seen in the sim). With older subroutines that don't use the guard, an aborted macro can leave a changed WCS, G20/G21 or tool offset: `on_abort.ngc` only restores G90, G40, M5, M9. | Medium to high, depending on the Pi's subroutines | Needs a decision (machine config) |
| 5 | FEED HOLD while idle (remote screen or pendant; the touch button is greyed out) left task PAUSED in MANUAL. MDI then failed with "Must be in MDI mode to issue MDI command" until CYCLE START. | Low | Fixed for millgui's button |
| 6 | Remote actions skip the touch screen's greying (known: "the Pi admits every remote command"). Zero XYZ, M6, unload and drawbar have no Rust-side state check; LinuxCNC refuses the unsafe cases with its own error. | Low | Open |
| 7 | Spindle STOP during a running program is refused by LinuxCNC ("can't do that ... in auto mode"). Same as Axis and Probe Basic; STOP or FEED HOLD is the way. | Info | As LinuxCNC |

Checked and fine:

- E-stop in MDI mode leaves task in MDI/COORD; the next jog switches to
  MANUAL and to free mode (joint jogs) because X/Y/Z are unhomed.
- HOME ALL after an E-stop and power cycle homes (PR #7), from MANUAL or MDI.
- MDI while unhomed: LinuxCNC says "Can't issue MDI command when not homed".
- Run from selected line while task is in MDI: switches to AUTO, runs the
  restart copy, then reloads the original.
- Jog, MDI, HOME ALL, offsets, probe and power feed are greyed out or refused
  while the interpreter is busy, as in Probe Basic.
- The NML error text is 255 characters at most (LinuxCNC's limit); millgui's
  256-byte buffer keeps all of it.

## How Axis and Probe Basic do it

- Axis `ensure_mode()` switches and waits for task to finish the command
  (`wait_complete`) before the run, MDI or home command.
- QtPyVCP `setTaskMode()` refuses while running and waits the same way.
  Homing sets MANUAL and `teleop_enable(False)` first (as millgui does since
  PR #7). Jogging is refused unless task is already in MANUAL; millgui
  switches instead.
- Neither checks that the switch took effect (Axis `ensure_mode` returns
  True whatever task did), so a run sent during a jog is dropped there too
  unless the button is greyed out. Axis only pauses when AUTO is reading or
  waiting, as millgui's FEED HOLD now does. Errors appear in full: Axis in
  a notification box, QtPyVCP in its error dialog or status line.

## Fixes (this branch)

- `worker.rs`: mode decisions use a fresh status read. When task keeps the
  old mode after a switch, the command is not sent and the operator sees
  "Program start didn't happen: LinuxCNC stayed in MANUAL mode (Ignoring task
  mode change while jogging)". A spindle stop is still sent. Single step
  switches through the worker like run.
- LinuxCNC errors also show in the toast, which grows to fit the whole
  message and stays up about one second per 15 characters (5 s at least).
- FEED HOLD is only sent while a program or MDI command runs.
