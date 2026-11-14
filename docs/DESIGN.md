# millgui visual design

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
  a glance. Status tones (`good`, `warn`, `bad`, `info`) colour text and dots,
  not plates. An override off 100 % shows in `warn`.
- **Type.** Bebas Neue (`display`) for labels and buttons, letter-spaced
  about 5 % of the size. Barlow Condensed (`body`) for every value and number
  (weight 600) and for sentences. Captions are small, spaced and `dim`; the
  value beside them carries the weight.
- **Spacing and corners.** `gap` between cards, `pad` inside them, 12 px
  between rows. `radius` for cards, `radius-ctl` for controls and wells,
  `radius-sm` for the pill inside a segmented control.
- **Nothing moves.** Changing values, tabs or states must not shift a
  neighbour: give changing text a fixed width and keep a slot for anything
  that appears and disappears.
- **Touch.** Targets stay at least 48 px tall (primary ones 56 px and up);
  the hold-to-confirm sweep and the HOLD tag are part of `Btn`.
- **Cheap to draw.** The Pi draws in software: flat fills and rounded
  rectangles only. No gradients or shadows.
- **Quiet motion.** Only a change of state animates, and briefly:
  `Theme.quick` (140 ms) for presses, toggles and tab changes, `Theme.settle`
  (220 ms) for bars and status colours. A press lights at once and fades on
  release. Nothing moves position, nothing loops, nothing animates at idle,
  and no animation delays a touch or a machine action.

## Widgets

- `Btn`: plate button; `on` for toggles, `kind` for go/hold/stop, `hold`
  for the 800 ms confirm, `flat` for nav tabs.
- `Seg`: segments placed side by side with spacing 0 share one track; set
  `first` / `last` on the outer ones to round its ends.
- `Pill`: status dot and label. `Card`, `Label`, `H3`, `Note`, `NumField`.

## Applying it to another screen

Replace literal colours and radii with tokens, group mutually exclusive
choices into one `Seg` track with `first` / `last`, change solid `Theme.sel`
fills on rows to the tint-and-edge treatment, and use caption-plus-value
pairs instead of boxed labels.
