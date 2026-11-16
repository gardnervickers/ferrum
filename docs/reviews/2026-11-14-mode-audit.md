# millgui mode and state audit, against Probe Basic (2026-11-14)

Two bugs on the mill came from mode state: HOME ALL did nothing after an
E-stop (motion left in teleop, PR #7), and a program's G53 line was refused
because G91 was still on from MDI or a probe macro (PR #10). This audit
compares how millgui and Probe Basic handle modes around every action. It
covers both task modes (MANUAL/MDI/AUTO, joint/teleop) and G-code modal state
(G90/G91, units, WCS, tool offset).

Sources read:
- QtPyVCP `actions/machine_actions.py`, `program_actions.py` and
  `base_actions.py`, and `widgets/button_widgets/subcall_button.py`
  (kcjengr/qtpyvcp main).
- Probe Basic's UI and subroutines (gardnervickers/probe_basic,
  `codex/mymill-2026-09-30`).
- The mill's own Probe Basic tabs in this repo: `custom_ui/tabs/powerfeed`,
  `util`, `cust_probe`, `machine_status`, `template_user_buttons`, and
  `mymill_vcp/auto_stock_panel.py`.
- LinuxCNC 2.9.10 `emctaskmain.cc` and `emctask.cc`.

The QtPyVCP version installed on the Pi was not checked against the source
read here.

Everything was reproduced in the LinuxCNC 2.9.10 simulator (`millgui/sim`).
It ran the Pi's reconstructed `mymill.hal` (git blob `6d3316a9`) and the repo
`mymill.ini`, and was driven through millgui's remote socket. Nothing was
sent to the mill.

## How LinuxCNC behaves

- Task ignores a mode change while an axis jogs ("Ignoring task mode change
  while jogging"). It then takes a program run in MANUAL and drops it with
  no message.
- Any switch to MDI or AUTO calls `emcTaskAbort()`. This includes a switch to
  the mode task is already in. Task only blocks it while AUTO is busy, so a
  switch while an MDI command runs aborts that command.
- MANUAL picks teleop when all joints are homed, and free mode otherwise.
  MDI and AUTO use coordinated mode. E-stop unhomes X/Y/Z but leaves motion
  in whatever mode it was in.
- The pendant wheel (`axis.N.jog-counts`) only moves the machine in teleop,
  which means MANUAL with the machine homed.
- G-code modes (G90/G91, WCS, units, G43) carry over from MDI and macros into
  the next program. Only M2/M30 and `ON_ABORT_COMMAND` reset some of them;
  the mill's `on_abort.ngc` sets G90 and G40.

## Task modes, action by action

| Action | Probe Basic | millgui before | millgui now |
|---|---|---|---|
| Home | MANUAL, `teleop_enable(0)`, home (`_home_joint`) | Same since PR #7 | Same |
| Jog | Refused unless task is already in MANUAL; then teleop | Switches to MANUAL, then teleop if homed, free mode if not | Unchanged (a superset) |
| MDI (typed, buttons, probe and macro calls) | Switches to MDI. When the interpreter goes idle, goes back to the mode it came from (`issue_mdi` with `PREVIOUS_MODE`; the mill's own MDI workers do the same) | **Stayed in MDI**, with motion in coordinated mode. The pendant wheel was dead after every MDI, probe or power feed until a screen jog (reproduced) | **Goes back** to the earlier mode once MDI has been idle for 0.5 s |
| MDI allowed when | On, homed, idle (`_issue_mdi_ok`) | On and idle (LinuxCNC refuses unhomed MDI) | **On, homed and idle** |
| Load program | `program_open` in the current mode, no switch | **Switched to AUTO** (abort and interpreter resync) | **No switch** |
| Cycle start | `setTaskMode(AUTO)` (refused while running), then run; resume when paused | Switch to AUTO, run; resume when paused | Same, and when task refuses the switch the run isn't sent and the reason is shown (PR #8) |
| Single step | Step if paused, else AUTO then step | Separate SetMode, unchecked | Through the worker, checked |
| Feed hold | Only offered while running | Button greyed when idle; the remote screen could still pause | Only sent while running |
| Run from line | Native `AUTO_RUN` at a line | Restart copy (PR #5; native run-from-line skips M6) | Unchanged, preamble order fixed in PR #10 |

Every mode decision now uses a fresh status read, not the last 50 ms poll
(PR #8). A switch decided on stale status could abort a command that had
just started.

## G-code modal state, action by action

| Action | Probe Basic | millgui |
|---|---|---|
| Program start | Nothing; relies on the program (the Fusion post sets G90 G17 G21) | Same |
| Run from line | Nothing | The restart preamble sets G90 G40 G80 etc. before its G53 (PR #10) |
| MDI | Nothing | Same |
| Stock probe macros | Start G91 and force G90 at the end, except bore, pocket, slot and pocket-calibration routines, which **end in G91**. Probe Basic does nothing about it, so the G53 bug would also happen there. | Probe screen puts back the starting distance mode after a run (PR #10). Better than Probe Basic. |
| Mill macros (power feed, touch-off, spindle nose) | `mymill_modes_begin` captures modes and `mymill_modes_complete` restores them. The HAL mode-recovery guard holds motion if one was interrupted, and Utilities offers RESTORE MODES (`o<mymill_restore_modes>`). The power feed tab also forces G90 and the previous WCS afterwards. | Same macros. **millgui has no RESTORE MODES action** (see open item 2). |
| Auto Stock / Two Faces | After a failed run, the panel forces RESTORE MODES (`G90 G94`) before another | The probe screen runs the macros; no forced restore after a failure (PR #10 restores G90/G91 only) |

## Open items, for a decision

1. **The Pi's `mymill.hal` has no mode-recovery guard.** With the repo's
   subroutines, power feed, spindle-nose probing and touch-off stop at once
   on the Pi's HAL with "Named parameter #<_hal[mode-recovery-pending.out]>
   not defined" (seen in the sim). If the Pi has older macros without the
   guard, stopping one partway can leave a different WCS, units or tool
   offset in place, because `on_abort.ngc` only restores G90 and G40. Fixing
   this means changing the Pi's machine config.
2. **No RESTORE MODES in millgui.** Resolved without a button: millgui
   notes the modes before every `o<...> call` (units, plane, feed mode, WCS,
   G90/G91, and G43 for the same tool) and puts back whatever differs once
   the call ends, whether it finished, was stopped, or was cut off by an
   E-stop (then right after homing, before any MDI line or program). Typed
   G-codes are left as typed. Checked in the simulator.
3. **Beyond Probe Basic: G91 at CYCLE START.** Gardner chose not to refuse
   the start. CYCLE START (and a first single step) now sets the modes a
   program expects when they differ: G90, G17, G40, G80, G94 and G91.1, as in
   RS274NGC_STARTUP_CODE. Units, the work system and the tool offset are left
   to the operator. Checked in the simulator.

## Other findings (PR #8)

- **CYCLE START during a jog step:** task kept MANUAL and dropped the run.
  Now the run isn't sent, and millgui says "Program start didn't happen:
  LinuxCNC stayed in MANUAL mode (Ignoring task mode change while jogging)".
- **LinuxCNC errors were cut short in the header.** PR #10 shows them in
  full in a popup that stays until tapped, so PR #8 leaves that to it.
- **Remote actions skip the touch screen's greying** (known). LinuxCNC
  refuses the unsafe cases with its own error.
- **Spindle STOP during a running program** is refused by LinuxCNC, as in
  Axis and Probe Basic; use STOP or FEED HOLD.

Checked and fine: E-stop while in MDI, then jog (switches to MANUAL and free
mode); HOME ALL after an E-stop; MDI unhomed is refused with a clear
message; run-from-line starting from MDI mode; a program run after an MDI
(the switch from MANUAL to AUTO works).
