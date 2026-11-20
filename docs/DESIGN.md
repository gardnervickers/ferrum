# Ferrum visual design

The Run screen and the frame around it (status bar, DRO column, bottom bar)
are the reference. Other screens inherit the shared widgets and should be
brought in line with these rules. Tokens live in `app/ui/theme.slint`,
widgets in `app/ui/widgets.slint`.

## Rules

- **Depth from tone, not borders.** Surfaces step `page < panel < panel2 <
  raised`; wells (`well`) sit below the panel for plots, lists and inputs.
  Cards have no border. Only tappable controls carry a 1 px edge.
- **One accent.** `sel` marks what is chosen or current. A chosen control is
  a tint (`sel-soft`) with a 2 px `sel` edge and `sel-ink` text, never a solid
  accent slab. The same treatment applies to toggles, tabs, nav and list rows.
- **Solid colour means a machine action.** Only `go`, `hold` and `stop`
  buttons are filled, so CYCLE START, FEED HOLD, STOP and E-STOP are found at
  a glance. ENTER, DONE, FINISH, HOME and anything that only edits a table
  are plain plates. A progress bar is the accent, not green. The accent
  edge means "chosen" and nothing else: a key of another kind in a pad (the
  keypad's operators) is marked by `accent` ink, not by `on`. Status tones (`good`, `warn`, `bad`, `info`) colour text and dots,
  not plates. An override off 100 % shows in `warn`.
- **Type.** Bebas Neue (`display`) for labels and buttons, letter-spaced
  about 5 % of the size. Barlow Condensed (`body`) for every value and number
  (weight 600), for G-code and for sentences (never caps). Captions are
  small, spaced and `dim` (`Caption`); the value beside them carries the
  weight. Every size is a step of the scale in `Theme` (`t-tag` 13,
  `t-cap` 16, `t-small` 18, `t-body` 20, `t-ctl` 22, `t-btn` 24, `t-value`
  28, `t-big` 32, `t-title` 38); only the DRO numerals and the splash have
  sizes of their own.
- **Spacing and corners.** `gap` between cards and around a screen, `pad`
  inside a card, `row` (12 px) between its rows, 10 px between buttons in a
  row, 8 px inside a tight group; `dlg-pad` / `dlg-row` in a dialog.
  `radius` for cards, `radius-ctl` for controls and wells, `radius-sm` for
  the pill inside a segmented control. A wider gap (32 to 40 px) is kept
  on purpose between a start and its stop.
- **Heights.** `h-sm` 48 (a stacked pair, a title-row control), `h-ctl` 56
  (the standard button, field and segment), `h-choice` 64 (a row of
  choices, a dialog's foot), `h-big` 84 (a primary action); the cycle
  buttons are 100 and the jog pads have their own.
- **Cards.** A card starts with its `H3` at the left and a hint or state at
  the right (or with the tabs that pick its content), then wells and rows.
  Nothing sits outside a card.
- **Lists.** The chosen row is the tint with a 2 px accent edge. The bar at
  the left edge means only "the machine is here" (the executing line). A
  list shows a whole number of rows (`floor(height / row) * row`), so the
  last one is never cut.
- **Nothing moves.** Changing values, tabs or states must not shift a
  neighbour: give changing text a fixed width and keep a slot for anything
  that appears and disappears. In the top bar, what comes and goes (HOME
  ALL, a toast) sits after the pills and takes room from the message.
- **Touch.** Targets stay at least 48 px tall (primary ones 56 px and up);
  the hold-to-confirm sweep and the HOLD tag are part of `Btn`. The corner
  tag only ever says HOLD, and a hold button's label never says "HOLD TO".
  Buttons of one role in a row are equal (`EqBtn`). A slider (the
  overrides) moves by the distance dragged from wherever the finger lands,
  after 8 px of slack, so a tap or a wobble never jumps it; it shows the
  dragged value at once and sticks at 100 % on the way past. The − and +
  beside it keep their steps (detents), and tapping the value resets it.
- **Glyphs.** Arrows and chevrons are 2 px strokes like the nav icons
  (`Arrows`, `Chevron`, `Btn.menu`), not font triangles.
- **Cheap to draw.** The Pi draws in software: flat fills and rounded
  rectangles only. No gradients or shadows.
- **Quiet motion.** Only a change animates, once and briefly, and every
  animation answers something the user did or the machine reported:
  `Theme.quick` (140 ms) for presses, toggles and tab changes, `Theme.settle`
  (220 ms) for bars and status colours, `Theme.enter` (180 ms, ease-out) for
  something arriving, `Theme.glide` (200 ms) for the nav plate. A press
  lights at once and fades on release. What moves:
  - a screen fades in and rises 12 px as it opens (`Arrive`);
  - the nav bar's chosen plate glides to the tapped tab;
  - a dialog (keypad, keyboard, picker, tool change, messages) rises 24 px
    over a darkening scrim; it closes at once, so a closing dialog is never
    read as still asking for something;
  - the MACHINE STATUS list unfolds down from its pill (the caret flips);
  - a toast drops 12 px into place;
  - after a DRO ZERO the axis's number lights in the accent when the new
    value arrives and settles back to white;
  - a new top-bar message shows bright, then settles to grey.

  Nothing loops, nothing animates at idle, and no animation delays a touch
  or a machine action. An animated property never reads fast data (DRO
  numbers, the current line, progress): Slint re-runs the animation, and
  redraws the screen, at every change it reads. A one-off entrance sets
  constants in `init` (`opacity: 0; init => { self.opacity = 1; }`): a
  binding that flips in `init` starts at its end value and never plays.
  The start-up splash (`app/ui/brand.slint`) has one entrance of its
  own, under a second: the Fe tile rises in, the name's letters follow,
  a rust stroke draws under them. While LinuxCNC starts, the step under
  way blinks hard on/off; it stops as soon as the screen is ready. Rust
  (`Brand.rust`) belongs to the mark and the splash only, so it is never
  read as a status or a control.
  The exception is the probe preview: a small tip plays the selected
  routine twice on the PROBE picture (green, so it is never read as a
  control or an arrow), and it stops while a probe runs.

## Widgets

- `Btn`: plate button; `on` for toggles, `kind` for go/hold/stop, `hold`
  for the 800 ms confirm, `flat` for nav tabs, `icon` for a stroked icon
  over the label, `menu` for a chevron (it opens a list), `cap` for a small
  caption over a value, `sub` for a small line under the label. `EqBtn`:
  the same, equal in width to its neighbours.
- `Seg`: segments placed side by side with spacing 0 share one track; set
  `first` / `last` on the outer ones to round its ends. Every either/or
  choice is one track.
- `NumField`: the one look for a number you tap to change: a well with an
  edge, the value right-aligned, its unit small and dim after it (never in
  the label). `FieldTile`: the same with the caption above, for grids.
- `Kv`: a caption and value to read (`inset` on a card, `tone` for status).
- `Pill`: status dot and label on a quiet tint of its tone; `caret` when it
  opens a list. `Card`, `Label`, `Caption`, `H3`, `Note`, `Chevron`.
- `Scrim` + `Dialog` + `DialogTitle` (overlays.slint): every dialog. Title
  at the top, buttons in a row at the foot: backing out (CANCEL) on the
  left, going on (ENTER, DONE, CLOSE) on the right.

## Applying it to another screen

Replace literal colours and radii with tokens, group mutually exclusive
choices into one `Seg` track with `first` / `last`, change solid `Theme.sel`
fills on rows to the tint-and-edge treatment, and use caption-plus-value
pairs instead of boxed labels.
