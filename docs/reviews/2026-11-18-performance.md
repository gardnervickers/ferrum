# millgui performance pass (2026-11-18)

Measured in the cloud LinuxCNC 2.9.10 simulator (`millgui/sim`), release
build, 1920x1080 on Xvfb. Never run against the mill. "Frames/s" is Slint's
own count (`SLINT_DEBUG_PERFORMANCE=refresh_lazy,console`); CPU is the
millgui process from `/proc`. With the GPU renderer (what the Pi uses since
Mesa 25) every frame redraws the whole screen, so frames/s is the number
that matters most there. The cloud GPU path is Mesa llvmpipe, which is far
slower than the Pi's V3D, so its CPU figures only show direction.

| State | Frames/s before | after | CPU before (software) | after |
|---|---|---|---|---|
| E-stop, idle (status pill flashing) | 26 to 32 | 4 | 2.8% | 1.0% |
| Ready, idle, RUN | 0 | 0 | 0.7% | 0.7% |
| Program running, RUN | 55 (software), 31 (GPU, llvmpipe maxed out) | 21 | 27.5% | 18.8% |
| Program running, TOOLS | 60 | 21 | | |
| PROBE open, idle | 30, forever | 30 for two loops of the preview, then 0 | 23% (GPU) | 0.6% (GPU) |
| Step jogging, MANUAL | | 11 | | |

GPU renderer on llvmpipe, program running: whole process 236% to 182%
(llvmpipe threads included), millgui thread about 30% to 24%.

## Causes and fixes

1. **Slint 1.18 drew an empty second frame after every change** (its X11
   frame throttle redrew once more even when nothing was pending). Fixed
   upstream; millgui now uses Slint 1.19.
2. **Animated bindings that read fast-changing data.** When a value an
   animated property depends on changes, Slint runs the animation logic
   again and draws one more frame, even if the result is the same. The DRO
   rows fed the ZERO/HOME buttons (animated) and the axis colour, so every
   status (20 a second while moving) cost two frames. `DroRow` now holds
   only the axis and homed state; the numbers are a separate `dro-pos`
   list. The G-code list's current-line tint and the program progress bar
   no longer animate (they move with every line).
3. **Lists replaced every tick.** Setting a list property to a new model
   always counts as a change. The pills (every 200 ms), the UTIL status
   rows (every 250 ms) and the DRO (every status) were rebuilt each time.
   `models::sync` keeps the model and writes only rows that differ; all
   list properties use it (the G-code text excepted, set once per load).
4. **The flashing status pill faded** in and out (220 ms each way), which
   is animation frames most of every second while the machine is in
   E-stop or not homed. It now flashes hard on/off.
5. **Backplot redraws.** It redrew on its own 100 ms timer, a frame apart
   from the DRO's; it now redraws inside the status update when due, so
   both land in one frame. It is not redrawn while RUN is not showing
   (unless a browser is watching). The GPU backplot draws into one texture
   instead of alternating two, so the image property does not change.
6. **PROBE preview** looped forever at 30 frames/s. It plays twice and
   rests on the finished routine; a new shape, target or option, or coming
   back to PROBE, plays it again.

## Not changed

- Status is polled at 20 Hz (50 ms); the NML thread uses about 0.1% CPU.
- The JOB page's op rows animate their background and read the op's
  progress, so a running job op costs an extra frame per progress update
  (the job timer is 4 Hz). Small; left as is.
- Not measured on the Pi. Expect each GPU frame to cost a few ms there;
  the drop in frames is what carries over.

## How to repeat

Build with symbols (`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
CARGO_PROFILE_RELEASE_STRIP=false`), run the sim with
`SLINT_DEBUG_PERFORMANCE=refresh_lazy,console MILLGUI_PERF=1`, and drive it
over the remote port (estop-reset, power, home-all, files-tap, files-load,
cycle-start). LinuxCNC writes the display's stderr to
`/tmp/linuxcnc.debug.*`. A 20,000-segment program makes a run last minutes.
