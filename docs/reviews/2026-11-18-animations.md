# millgui animation pass (2026-11-18)

Short, one-off animations that show where a change came from, without
giving back the performance pass (`2026-11-18-performance.md`). Tried in the
cloud LinuxCNC 2.9.10 simulator (`millgui/sim`) only, never on the mill.
The rules are in `docs/DESIGN.md` (Quiet motion).

## What moves

| Change | Animation | Where |
|---|---|---|
| Open a screen | fades in, rises 12 px (`Theme.enter`, 180 ms) | `Arrive`, widgets.slint; main.slint |
| Tap a nav tab | the chosen plate glides to it (`Theme.glide`, 200 ms) | `BottomBar`, frame.slint |
| Keypad, keyboard, picker, tool change, messages | scrim darkens, dialog rises 24 px; closes at once | `Scrim`, `Dialog`, overlays.slint |
| Tap the status pill | MACHINE STATUS unfolds down from it; caret flips ▼/▲ | main.slint |
| Toast | drops 12 px into place and fades in | main.slint |
| DRO ZERO | the axis number lights in the accent when the new value arrives, settles to white (600 ms) | `DroLine`, frame.slint |
| New top-bar message | bright for 1.2 s, then settles to grey (800 ms) | `StatusBar`, frame.slint |
| FILES top view / G-code, MANUAL power feed / GO TO | panel fades in, as RUN's tabs already did | files.slint, jog.slint |

Nothing loops and nothing runs at idle. Closing is instant everywhere: a
dialog or list that is fading out could be read as still open.

## Cost

Same measuring as the performance pass: release build, software renderer,
1920x1080 Xvfb, Slint's own frame count, millgui CPU from `/proc`.

| State | Frames/s before | after | CPU before | after |
|---|---|---|---|---|
| E-stop, idle (status pill flashing) | 4 | 4 | 0.8% | 0.7% |
| Ready, idle, RUN | 0 | 0 | 0.6% | 0.6% |
| FILES, idle | 0 | 0 | 0.5% | 0.3% |
| Program running, RUN | 21 | 21 | 17.7% | 16.4% |
| Program running, TOOLS | 21 | 21 | 4.0% | 3.5% |
| One nav tap (2 s window) | ~15 frames | ~15 frames | 1.0 to 1.5% | 3 to 5.5% |

A nav tap already drew for about 200 ms (the press fade and the tab's own
toggle), so the glide and the screen's fade land in the same frames; what
they add is per-frame work: while it fades, a screen is drawn into a layer
and blended (roughly 50 ms of CPU per tap in the cloud). At full opacity
Slint draws it directly again (`Opacity::need_layer`). The GPU (femtovg)
renderer was also checked with the keypad opening; it draws the same.

The DRO's ZERO flash animates only its own flag: the number's text is
read by a `changed` handler (no redraw of its own) to light it when the new
value arrives, or after 400 ms if the axis already read 0.

## Found while reviewing

- A binding that flips in `init` (`opacity: shown ? 1 : 0; init => { shown
  = true; }`) never plays: the first evaluation already sees the end
  value. Entrances set constants and assign in `init`, like RUN's tab fade.
- Lighting the number on the ZERO tap lit the old value for a frame before
  0.000 arrived; it now waits for the new value.

## Not changed

- No exit animations, no motion on machine-driven changes that repeat
  (DRO, line, progress). Only opacity and position move, which both
  renderers draw the same.
- The Pi was not measured.
