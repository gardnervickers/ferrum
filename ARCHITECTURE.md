# Ferrum architecture and milestones

Ferrum replaces Probe Basic with a Rust/Slint GUI that follows
`rust_gui/mockup.html`. Probe Basic keeps running from `mymill.ini`; Ferrum runs
from `ferrum.ini`, which includes the same machine config.

## Layers

```
app (Slint UI, screens, module registry)
 ├── machine    Machine trait, worker thread, Ferrum's mode rules
 │    ├── lcnc::Connection  LinuxCNC over NML (crates/lcnc), Pi and sim
 │    └── Sim   in-process fake machine, Mac development and tests
 ├── lcnc       standalone LinuxCNC bindings, no Ferrum assumptions:
 │              typed Status/Command/Message, C++ NML shim, typed HAL pins
 └── preview    rs274 interpreter shim -> toolpath; renderer trait
      ├── Software  tiny-skia into a Slint image (works on llvmpipe)
      └── Gpu       later; same toolpath, drawn with GL
```

- One thread owns the NML connection. It polls status every 50 ms, sends a
  snapshot to the UI only when it changed, and runs commands from a queue.
  The UI thread never blocks on LinuxCNC.
- The shim copies what the GUI needs from `EMC_STAT` into a flat C struct, so
  Rust never depends on C++ class layout. It is built with clang against the
  Pi's own glibc and libstdc++ (`sysroot/`), the same C++ runtime as `libnml`.
- `Sim` implements the same `Machine` trait. Every screen runs on the Mac
  without a Pi, and screen logic gets tests without hardware.

## Modules (adding and removing features)

Every feature is a module: run, job, jog, probe, tool setter, tools, offsets,
conversational, files, util, power drawbar tool change, rotary, and so on.
A module declares:

- an id and whether it is enabled for this machine (from `ferrum.ini`);
- its nav entry (screen), if it has one;
- the HAL pins it owns and a postgui HAL snippet (`ferrum/hal/<id>.hal`) that
  nets them;
- status pills for the status bar;
- capability flags other screens can check (for example `rotary` adds the
  rotary probe targets; `toolsetter` adds TOUCH OFF buttons).

`[FERRUM]MODULES` in `ferrum.ini` lists the enabled modules. A disabled
module is not constructed at all: no nav entry, no HAL pins, no postgui nets,
no pills, and its screen is never instantiated (Slint `if`). Adding a
feature is a new module file plus a Slint component; nothing else changes.
Some flags are also derived from the machine config (for example rotary needs
an A axis in `[TRAJ]COORDINATES`), and a module can refuse to start with a
clear message when its prerequisites are missing.

## What the GUI is and is not responsible for

- **Correctness and safety interlocks stay out of the GUI.** The drawbar and
  spindle interlock (`spindle_drawbar_guard`), drive readiness and limits are
  HAL and LinuxCNC. A GUI bug or crash must not be able to defeat them.
- **Misuse safeguards live in the GUI:** hold-to-confirm, disabled buttons when
  a command cannot apply, the tool change RESUME gate. They reduce operator
  error but are not relied on for correctness.
- **Probe settings are per run.** A probe run sets its `#30xx` parameters, then
  the defaults saved in `probe_basic.var` are restored when the interpreter
  returns to idle, whether the run finished, failed or was aborted. Ferrum
  also restores them at startup, which covers a GUI crash mid-run.
  (Assumes only Ferrum changes `#30xx` outside of probe runs.)

### A frozen screen stops the machine

The touchscreen is the operator's view of the machine. If it stops drawing
while LinuxCNC, HAL and the pendant run on, taps land on controls nobody can
see. So (app/src/failsafe.rs, the always-on `watchdog` module):

- **HAL screen watchdog.** `ferrum.watchdog.heartbeat` flips once per frame
  actually drawn: femtovg's AfterRendering notifier, or winit's
  RedrawRequested with the software renderer. A timer only *asks* for a
  frame every 250 ms, so a live event loop with a dead window does not beat.
  A realtime `oneshot` (1 s) drives a latched E-stop when the beats stop:
  `estop-latch.0.fault-in` on the mill (`ferrum/hal/watchdog.hal`, no change
  to mymill.hal), or a built-in latch and gate on `iocontrol.0.emc-enable-in`
  elsewhere. It covers a frozen window, a hung or killed Ferrum and a crash.
  The machine cannot be turned on before the screen draws. After a trip the
  status list says `SCREEN FROZE n S: E-STOP` until E-stop is reset.
- **Stale input is dropped.** A tap, key or wheel event that arrives more
  than 600 ms after the last drawn frame is ignored (not in `--sim`). A
  release always goes through: it ends what a fresh press started (a
  continuous jog stops on it).
- **Tested on the real start path.** The harness starts Ferrum with
  `--boot` (the splash launcher, as `ferrum/boot` on the Pi), checks after
  every run that the watchdog is happy and no two frames were more than
  700 ms apart, and `scenarios/failsafe/` freezes Ferrum mid-move (SIGSTOP),
  taps a frozen screen and tries to power on without a screen.

### What runs is what was checked

LinuxCNC reads the program file as it runs. A file copied over the loaded
program (scp, a re-post from CAM) would run the new text while RUN shows
and travel-checks the old one. So (main.rs `reload_if_changed`): while
idle, a change (inode change time, which `scp -p` cannot set back) loads
the file again in LinuxCNC and re-reads the preview; CYCLE START and RUN
FROM SELECTED refuse while the file changed or is still being read for
the travel check; a change under a running program feed holds it and
says so.

CYCLE START stops the PROBE screen's quiet-time defaults write midway
(it goes line by line, and a line that lands after the program starts is
refused); the rest waits for the next quiet time. A start or a reload
restarts that quiet time (`Probe::start_problem`, `hold_restore`).

## Remote (web) access

Requested 2026-11-13: use the GUI from a MacBook's browser. Plan: the same
Slint UI compiled to WebAssembly, with the Pi streaming state over a websocket.
The browser renders, so the Pi's cost is serialization only (VNC would render
and encode on the Pi's two non-RT cores).

```
browser (wasm): Slint UI + Remote: Machine ──ws──> Ferrum on the Pi: `remote` module
                                                     ├─ serves the wasm bundle
                                                     ├─ fans out Status, messages, pills
                                                     └─ admits commands into the worker queue
```

- `Remote` implements the `Machine` trait, so screens do not know where they
  run. HAL pins exist only on the Pi, so the Pi sends the module pills it
  computed alongside each `Status`.
- Commands from the browser enter the same worker queue as the touchscreen's;
  LinuxCNC serializes them and both screens show the same status.

Classification:

- **Correctness (when control is allowed).** (1) The Pi decides whether a
  remote command is admitted; greying out buttons in the browser is only UX.
  (2) A continuous jog must not outlive its sender: a lost "stop" over the
  network would leave an axis moving. Either the browser renews a jog lease
  (for example every 100 ms, stopped by the Pi after 300 ms without one or on
  disconnect), or remote jogs are incremental only. Recommendation: incremental
  only at first; it needs no lease. (3) A remote E-STOP button is a request,
  not an e-stop: the network can drop it. The hardware e-stop chain stays the
  only safety function.
- **Availability.** A browser disconnect or slow client must not stall the
  worker or the touchscreen: per-client bounded queues that drop stale status
  and keep the newest.
- **Decision (Gardner, 2026-11-13):** remote control is enabled for now, with
  no touchscreen grant; the access policy comes later. Remote jogs stay
  incremental only, which is a correctness item above, not policy.
- **Misuse safeguards (policy, deferred).** Read-only by default. The override is a design
  choice with options: a touchscreen grant ("ALLOW REMOTE CONTROL") that ends
  when the browser disconnects, which needs someone at the machine; an INI
  setting `[FERRUM]REMOTE_CONTROL = allow` for a trusted setup; or both
  (recommended: INI permits it, the touchscreen grants it). FEED HOLD and STOP
  could be admitted even when read-only, since they only remove motion; to decide.
  Who can connect: the LAN only, plus a token if the network is shared.
- **Uncertain:** Slint's wasm backend renders through a canvas. Fonts are
  embedded already. The backplot's software renderer should run in the
  browser unchanged, but that is unverified.

## Milestones

| | Milestone | Done when |
| --- | --- | --- |
| M0 | Skeleton | Cross build, `linuxcnc ferrum.ini`, one HAL read and write. Done. |
| M1 | Foundation | Workspace, clang + sysroot toolchain, `linuxcnc` crate (Nml + Sim), module registry from the INI, fixed frame (status bar, DRO, overrides, nav, cycle buttons) live on the Pi. |
| M2 | Manual control | Jog screen (jog, step, power feed, go to, spindle, zero), homing, MDI with history, error toasts, keypad and hold-to-confirm overlays. |
| M3 | Run | Files (no USB), program load, G-code view, software preview through the interpreter shim, run, run from line, next M6, single block, M01. |
| M4 | Tooling | Tools and Offsets screens, tool change overlay (power drawbar module). Ferrum can run a real job end to end. |
| M5 | Probing | Probe screen with per-run settings, tool setter, Util and machine status. |
| M6 | Job and Conv | Job builder and conversational screens over the existing Python CLIs. |
| M7 | GPU preview and polish | GL renderer behind the same trait, Pi performance pass, findings from the UI/UX review. |
| M8 | Remote web | `remote` module: websocket server and wasm bundle, read-only by default with an override, incremental-only remote jogs. Gardner (2026-11-13): do it right after the M3 backplot, before M4. |
