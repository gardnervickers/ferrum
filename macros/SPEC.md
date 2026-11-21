# Probe macro behaviour specification (clean-room)

This document specifies, from observed behaviour and from the interfaces
Ferrum depends on, what each probing subroutine that Ferrum calls must do.
It exists so that new LinuxCNC probe macros can be written under the MIT
licence **without reading Probe Basic's GPL-3 G-code** (or the repo files
derived from it).

- It describes inputs, outputs, geometry, formulas, checks and end states in
  prose and tables. It deliberately contains no G-code from the existing
  macros, no copied comments and no description of their code structure
  (block numbering, line order, local variable names).
- Facts that are an interface (parameter numbers, widget names, argument
  order, LinuxCNC system parameters, formulas) are stated exactly.
- Where the existing behaviour looks like a defect, it is described as
  observed and marked **Quirk**, with a recommendation. An implementer should
  either reproduce it or change it on purpose, together with ferrum.
- Implementers of the MIT macros should work from this file, Ferrum's
  source (MIT) and LinuxCNC's documentation only.

Reference environment: LinuxCNC 2.9.10, `mymill.ini`
(`[RS274NGC]SUBROUTINE_PATH = subroutines:subroutines/fusion_probing:vendor/probe_basic/configs/probe_basic/subroutines`,
so a file in `subroutines/` wins over a vendor file of the same name),
`HAL_PIN_VARS` and `INI_VARS` on, parameter file `probe_basic.var`, probe
tool T99.

---

## 1. Inventory and provenance

### 1.1 Macros Ferrum calls

`MACROS` in `ferrum/app/src/probe.rs` lists 34 names (the first call of each
PROBE target). The two multi-step flows add two more calls that are not in
`MACROS`: `probe_auto_stock_apply` and `probe_two_face_apply`. All 36 were
found on the subroutine path.

| Macro | Where it is resolved from | Provenance | Calls GPL macros? |
|---|---|---|---|
| probe_x_minus_wco, probe_x_plus_wco, probe_y_minus_wco, probe_y_plus_wco | vendor/probe_basic | Probe Basic, GPL-3 | no (self-contained) |
| probe_z_minus_wco | vendor | GPL-3 | no |
| probe_front_left_top_corner, probe_front_right_top_corner, probe_back_left_top_corner, probe_back_right_top_corner | vendor | GPL-3 | probe_z_minus_sub, probe_x_±, probe_y_± |
| probe_front_left_inside_corner, probe_front_right_inside_corner, probe_back_left_inside_corner, probe_back_right_inside_corner | vendor | GPL-3 | same |
| probe_top_left_edge_angle, probe_top_right_edge_angle, probe_top_front_edge_angle, probe_top_back_edge_angle | vendor | GPL-3 | same |
| probe_round_boss | vendor | GPL-3 | same |
| probe_round_pocket_center_start, probe_rect_pocket_center_start | vendor | GPL-3 | probe_x_±, probe_y_± |
| probe_valley_x_center_start, probe_valley_y_center_start | vendor | GPL-3 | probe_x_± / probe_y_± |
| probe_ridge_x, probe_ridge_y | vendor | GPL-3 | probe_z_minus_sub, probe_x_± / probe_y_± |
| probe_cal_round_pocket | vendor | GPL-3 | probe_z_minus_sub, probe_x_±, probe_y_± |
| tool_touch_off | `subroutines/` (overrides vendor) | **GPL-derived** (Probe Basic author header) | no; calls repo-own helpers |
| probe_spindle_nose | `subroutines/` (overrides vendor) | **GPL-derived** (Probe Basic author header) | no; calls repo-own helpers |
| probe_two_face, probe_two_face_apply | `subroutines/` | repo-own | **yes**: probe_two_face calls probe_x_plus / probe_x_minus / probe_y_plus / probe_y_minus |
| probe_auto_stock_center, probe_auto_stock_apply | `subroutines/` | repo-own | **yes**: via probe_auto_edge, which calls probe_x_± / probe_y_± |
| probe_a_rotary, probe_a_setup_cylinder_center, probe_a_measure_diameter, probe_a_measure_runout, probe_a_measure_deflection | `subroutines/` | repo-own | no GPL calls; they call `f360_*` helpers from `subroutines/fusion_probing/` (provenance not established in the repo: no licence header; treat as unknown and replace too) |

### 1.2 GPL helpers that Ferrum's macros need

These are not called by Ferrum directly but by the macros above. An MIT set
must provide equivalents (any names; the specified ones keep the repo-own
callers working unchanged):

| Helper | Used by | Specified in |
|---|---|---|
| probe_x_minus, probe_x_plus, probe_y_minus, probe_y_plus | every XY family, probe_two_face, probe_auto_edge | §3.2 "Edge touch" |
| probe_z_minus_sub | corners, edge angle, boss, ridge, calibration | §3.3 "Top touch" |

Not needed by Ferrum: `set_g30_position`, `go_to_g30`, `go_to_home`,
`unload_spindle`, `toolsetter_wco`, the `probe_cal_*` variants other than the
round pocket, the non-center-start pocket/valley variants, and the
`probe_*_top_side` / `probe_corner_*_edge_angle` macros. Ferrum does SET G30
with `G30.1` itself and GO TO G30 with its own MDI lines; the simulator
(`crates/machine/src/sim.rs`) fakes `set_g30_position`, `go_to_home` and
`unload_spindle` output only for compatibility, and `util.rs` still accepts
`x_tool_change_position`, `y_tool_change_position`, `z_tool_change_position`
EVAL lines if some macro sends them.

### 1.3 Repo-own helpers (MIT-able, may be reused as they are)

`mymill_probe_assert_no_contact`, `mymill_probe_coordinate_ready`,
`mymill_probe_safe_move_x|y|z|xy`, `mymill_modes_begin|restore|complete`,
`mymill_restore_modes`, `probe_auto_context|edge|scan|axis_move|axis_touch`,
`probe_two_face_context`, `m6_toolchange_optional_touch_off`, `on_abort`.
Their behaviour is summarised where it matters (§3.4, §11, §12, §13).

---

## 2. How Ferrum calls a macro (the caller's contract)

### 2.1 Call lines

| PROBE target (mode / shape / spot) | Call line Ferrum sends | Arguments |
|---|---|---|
| FIND / block / LEFT EDGE | `o<probe_x_plus_wco> call` | none |
| FIND / block / RIGHT EDGE | `o<probe_x_minus_wco> call` | none |
| FIND / block / FRONT EDGE | `o<probe_y_plus_wco> call` | none |
| FIND / block / BACK EDGE | `o<probe_y_minus_wco> call` | none |
| FIND / block / edge, "ALSO MEASURE" angle | `o<probe_top_{left,right,front,back}_edge_angle> call` | none |
| FIND / block / corner spot | `o<probe_{front,back}_{left,right}_top_corner> call` | none |
| FIND / block / STOCK CENTER (X and Y) | `o<probe_auto_stock_center> call [side] [span] [token]`, later `o<probe_auto_stock_apply> call [token]` | side 0 LEFT, 1 RIGHT, 2 FRONT, 3 BACK; span = "Max span" local (default 150), 3 decimals; token = positive integer, incremented per new measurement |
| FIND / block / CENTER IN X or Y only | `o<probe_two_face> call [axis] [dir] [stage] [token]`, then stage 2, then `o<probe_two_face_apply> call [token]` | §11 |
| FIND / pocket / inside corner spot | `o<probe_{front,back}_{left,right}_inside_corner> call` | none |
| FIND / pocket / POCKET CENTER | `o<probe_rect_pocket_center_start> call` | none |
| FIND / top | `o<probe_z_minus_wco> call` | none |
| FIND / round boss | `o<probe_round_boss> call` | none |
| FIND / boss (round or block), size hint 0 | `o<probe_boss_scan> call [step] [max]` | scan step 10 mm (0.4 in), at most 0.7 × `#3018`; max scan distance 150 mm (6 in) |
| FIND / bore | `o<probe_round_pocket_center_start> call` | none |
| FIND / slot, RIDGE, across X or Y | `o<probe_ridge_x> call` / `o<probe_ridge_y> call` | none |
| FIND / slot, SLOT, across X or Y | `o<probe_valley_x_center_start> call` / `o<probe_valley_y_center_start> call` | none |
| FIND / rotary / CENTERLINE Y, Z | `o<probe_a_setup_cylinder_center> call [est] [1] [0]` | reference size estimate (default 30), rotary direction 1, verify pass 0 |
| FIND / rotary / A0 ON A FLAT | `o<probe_a_rotary> call [1]` | rotary direction 1 |
| MEASURE / diameter | `o<probe_a_measure_diameter> call [dia] [1]` | diameter estimate (default 25), direction 1 |
| MEASURE / runout | `o<probe_a_measure_runout> call [dia] [1]` | same |
| MEASURE / deflection | `o<probe_a_measure_deflection> call [dia] [span] [1]` | diameter estimate (25), X span (50), direction 1 |
| CAL / probe tip | `o<probe_cal_round_pocket> call` | none |
| CAL / tool length | `o<tool_touch_off> call` | none (also called by the M6 remap, §10.1) |
| CAL / spindle nose | `o<probe_spindle_nose> call` | none |

All numbers are sent as plain decimals inside brackets. Most macros take no
arguments and read everything from `#30xx`.

### 2.2 What Ferrum does around the call

1. **GUI pre-checks** (`Probe::check`), before anything is sent: machine on,
   all homed, idle; for probe targets T`#3014` in the spindle, cutter
   compensation off (G40), G52/G92 offset all zero, target WCS rotation 0,
   `#3031` = 0; tool-setter target needs a tool that is not the probe and
   is > 0; nose target needs T0 and the HAL nose reference not already
   valid; rotary targets need an A axis. (When this was written Ferrum
   refused probing on inch machines; it no longer does, and Ferrum's own
   macros work in machine units.)
2. If the chosen FIND work system differs from the active one, Ferrum sends
   e.g. `G55` first, so **the macro always writes the active WCS (`#5220`)**
   and never needs a WCS argument.
3. Per-run `#30xx` values that differ from the saved defaults are written as
   `#n=v` MDI lines before the call; for FIND with the probe, Ferrum also
   forces `#3030 = 0` (write work offset). MEASURE and CAL runs do **not**
   force `#3030`: the parameter-file value applies (0 on mymill).
4. The call line is sent.
5. Afterwards, whatever happened (success, abort, STOP), Ferrum writes the
   defaults back, re-selects the original WCS and re-sends the original
   distance mode (G90/G91). Independently, the machine worker
   (`crates/machine/src/worker.rs`) snapshots units, plane, feed mode, WCS,
   distance mode and G43 state before every `o<…> call` and puts back the
   ones that changed once the call is over. Macros should still leave sane
   modes; see each family's "end state".

### 2.3 Inside a job (jobcore `probe.rs`)

A job op (FIND or MEASURE) emits: M5 M9, a modal reset (G21 G90 G94 G17 G40
G80 G91.1 G49), loads the probe and applies G43 H<probe>, selects the WCS,
saves and sets the per-run `#30xx` (and `#3030` = 0 FIND / 1 MEASURE),
descends to the taught start with a guarded move, then runs the call lines
only when `#<_task> EQ 1` (never in preview), then G90, restores the
`#30xx`, goes to the top of Z.

Constraints this places on macros:

- **Names must start with `probe_` or `mymill_probe_`, contain only
  lower-case letters, digits and `_`, and take only numeric literal
  arguments**, or jobcore refuses the op.
- MEASURE reads **numbered result parameters** after the call. It presets the
  one it needs to −999999 first, so a macro that does not write it is seen
  as "no reading":

  | Job value | Parameter | Written by |
  |---|---|---|
  | diameter (boss, bore) | `#1010` | probe_round_boss, probe_round_pocket_center_start |
  | x (pocket, slot, ridge) | `#1001` | probe_rect_pocket_center_start, probe_valley_x_center_start, probe_ridge_x |
  | y (pocket, slot, ridge) | `#1003` | probe_rect_pocket_center_start, probe_valley_y_center_start, probe_ridge_y |
  | z (top) | `#1009` | probe_z_minus_wco |

- Routines a job may run: stock (center + apply with token 1), corner,
  icorner, edge (no angle), top, boss, bore, pocket, slot, ridge.

### 2.4 Result messages (the interface contract)

Results reach Ferrum as LinuxCNC DEBUG messages whose text (after the
interpreter strips `DEBUG,` and substitutes parameters) has the form

    EVAL[vcp.getWidget{"<name>"}.setValue{<number>}]
    EVAL[vcp.getWidget{"<panel>"}.<method>{<n1>,<n2>,...}]

`parse_eval` requires exactly the prefix `EVAL[vcp.getWidget{"` (after
trimming whitespace), the name, then `"}.`, a method name, `{`,
comma-separated numbers, `}`. A `setValue` line is keyed by `<name>`; any
other method by `<panel>.<method>`. Numbers are whatever LinuxCNC prints for
a parameter (six decimals); any `f64`-parsable text works.

Rules that implementers must keep:

1. **Widget names are exact and case-sensitive** (list in §14). Ferrum maps
   known names to labels in `result_label` and shows unknown names verbatim.
2. Every EVAL line received during a run is listed in the result panel, in
   order, one line per message (first value only). Duplicates are shown
   twice.
3. **A name whose last `_`-separated part is a number 3004..3037** (e.g.
   `calibration_offset_3032`) is treated as "this is the new value of
   `#30NN`": Ferrum stores it as the default and saves it to `ferrum.toml`
   immediately, even if the run later fails. Do not use such names for
   anything else.
4. The two flows parse panel methods positionally:
   `auto_stock_panel.receive_result` must have exactly 9 values with value 0
   equal to the token; `two_face_probe_panel.receive_result` exactly 6
   values with value 1 equal to the token (§11).
5. **Any operator error message during a run fails the run**; its text is
   shown as "`<target>` failed: `<text>`". Ferrum does not match on abort
   wording anywhere, and the harness scenarios do not either, so abort texts
   may be reworded freely. Plain `(DEBUG,…)` or `(MSG,…)` lines without
   `EVAL[` are harmless. Never use ABORT for information.
6. Result lines can arrive up to 400 ms after LinuxCNC reports idle; that is
   handled by ferrum.

Note: the repo's Python test suite under `tests/` (for the old QtPyVCP
screen) asserts on the *source text* of several macros
(`test_probe_macro_safety.py`, `test_stock_probe_parity.py`,
`test_two_face.py`, `test_probe_settings_contract.py`,
`test_probe_hal_contract.py`, `test_mode_recovery.py`,
`test_probe_callback_contract.py`, `test_auto_stock_macro_safety.py`).
Replacing the files will break those textual tests; they need rewriting as
behaviour tests, not porting.

---

## 3. Shared conventions and primitives

### 3.1 Parameters and system values

`#30xx` settings (names as Ferrum's PROBE/SETUP screens show them; mm on
mymill):

| # | Meaning | Read by |
|---|---|---|
| 3004 | Setter fast feed | tool_touch_off, probe_spindle_nose |
| 3005 | Setter slow feed (0 = skip the slow touch) | same |
| 3006 | Setter traverse feed | same |
| 3007 | Setter max Z travel (search distance down from the start height) | same; written by probe_spindle_nose |
| 3008 | Setter max XY travel | read, unused |
| 3009 | Setter retract after first touch | same |
| 3010 | Spindle zero height (nose reference distance) | tool_touch_off; written by probe_spindle_nose |
| 3011 | Tool diameter probe mode (1 = not implemented → abort) | tool_touch_off |
| 3012 | Diameter offset mode (1 = touch off-centre by a radius) | tool_touch_off, probe_spindle_nose |
| 3013 | Offset direction 0 X−, 1 X+, 2 Y−, 3 Y+ | same |
| 3014 | Probe tool number | all probe macros |
| 3015 | Probe slow feed (0 = skip the slow touch) | all probe macros |
| 3016 | Probe fast feed | all |
| 3017 | Probe traverse feed (used for all positioning moves) | all |
| 3018 | Max XY search distance | XY touches |
| 3019 | XY clearance (retract after a touch; slow-touch distance is 2×) | XY touches, rotary |
| 3020 | Max Z search distance | Z touches |
| 3021 | Z clearance | Z touches, rotary |
| 3022 | Extra probe depth | corner, edge-angle, boss, ridge, cal plunge depth |
| 3023 | Step off width | corners, edge-angle, boss, ridge, cal |
| 3024 | Edge width (distance between the two edge-angle touches) | edge-angle |
| 3025 | Diameter hint | boss, bore |
| 3026 / 3027 | Rect pocket X / Y hint | rect pocket |
| 3028 / 3029 | Ridge/valley X / Y hint | ridge, valley |
| 3030 | Result mode: 0 write work offset, 1 position only | all FIND macros, cal, rotary setup, flows' apply |
| 3031 | WCO rotation: 1 also writes the edge angle as WCS rotation | edge-angle (Ferrum requires 0) |
| 3032 | Calibration offset (mm subtracted from the tip radius) | all XY compensation; written by cal |
| 3033 | Ring gauge diameter | cal |
| 3034 / 3035 | Cal block X / Y width | not used by Ferrum's macros |
| 3036 | Cal axis selector | cal (see Quirk Q3) |
| 3037 | Breakage tolerance | ferrum_tool_check (Ferrum's own, not a Probe Basic macro) |

LinuxCNC system parameters used: `#5061..#5063` probed X/Y/Z (current
program coordinates, including tool length offset), `#5070` 1 if the last
G38 move made contact, `#5181..#5183` G30 position (machine), `#5211..#5219`
G52/G92 offset, `#5220` active WCS index (1 = G54 … 9 = G59.3), WCS offsets
at `#[5201 + 20·n] + axis` (X +0, Y +1, Z +2, A +3), rotation at
`#[5210 + 20·n]` (= `#5230` for G54), `#5400` tool in spindle, `#5410` its
diameter, `#5420..#5423` current X, Y, Z, A in program coordinates.
Named: `#<_task>` (1 on the machine, 0 in preview), `#<_metric>`,
`#<_absolute>`, `#<_ccomp>`, `#<_tool_offset>`, `#<_feed>`,
`#<_feed_override>`, `#<_abs_x>` etc.

Direction names: "plus/minus" in a macro name is the **direction the probe
moves**, not the face. `x_plus` moves +X and therefore touches a face whose
material lies on the +X side (the left face of a block, or the right wall of
a pocket). Front = −Y (towards the operator), back = +Y, left = −X,
right = +X.

Tip geometry:

- `d` = `#5410` (probe tool diameter from the tool table).
- **Effective radius `r_eff = d/2 − #3032`.** The calibration offset is
  *subtracted* (it models pre-travel: the ball triggers before its nominal
  surface touches).
- Outside width (boss, ridge): `|raw| − (d − 2·#3032)` = `|raw| − 2·r_eff`.
- Inside width (bore, pocket, valley, ring): `raw + (d − 2·#3032)` =
  `raw + 2·r_eff`.
- `raw` is the difference of the two stylus-centre touch positions; a centre
  is their mean (compensation cancels).
- Z touches are not compensated: the probed Z is the tip's lowest point as
  given by the tool length offset (G43 H<probe> is assumed active; nothing
  checks it).

### 3.2 Edge touch (probe_x_minus, probe_x_plus, probe_y_minus, probe_y_plus)

Signature (all four): `[tool] [slow feed] [fast feed] [traverse feed] [max
distance] [clearance] [calibration offset]`; callers pass
`#3014 #3015 #3016 #3017 <reach> #3019 #3032`, where reach is usually
`#3018` (see each family for exceptions).

Behaviour, for direction `s` = +1 (plus) or −1 (minus) along axis A:

1. Cancel G92 offsets (side effect, kept for parity; Ferrum refuses to
   start with G92 set anyway).
2. Abort if the tool in the spindle is not `tool` (message: the probe tool N is not in the spindle).
3. Fast probing move (contact required: LinuxCNC errors out if the full
   distance is travelled without contact) of `max distance` in direction `s`
   at fast feed. Abort if no point was recorded.
4. Absolute move at traverse feed to `touch − s·clearance`.
5. If slow feed > 0: probing move of `2·clearance` in direction `s` at slow
   feed (contact required); then absolute move at traverse feed to
   `touch − s·clearance` again. The slow touch replaces the fast one.
6. `edge = touch + s·r_eff`.
7. Emit the edge (widget and parameter below) and return `edge` as the
   subroutine's return value (caller reads `#<_value>`,
   `#<_value_returned>` = 1). `#5061`/`#5062` still hold the last stylus
   centre touch, and several callers use that instead.

| Helper | Axis, direction | Widget | Param |
|---|---|---|---|
| probe_x_minus | X, −1 | `x_minus_probed_position` | #1005 |
| probe_x_plus | X, +1 | `x_plus_probed_position` | #1006 |
| probe_y_minus | Y, −1 | `y_minus_probed_position` | #1007 |
| probe_y_plus | Y, +1 | `y_plus_probed_position` | #1008 |

End state: absolute distance mode, feed = traverse, probe `clearance` away
from the face on the side it came from, same Z. No offset writes.

### 3.3 Top touch (probe_z_minus_sub)

Signature: `[tool] [slow] [fast] [traverse] [max Z distance] [Z clearance]
[result mode]`; callers pass `#3014 #3015 #3016 #3017 #3020 #3021 #3030`
(the calibration macro passes an extra eighth value, ignored).

Same pattern as §3.2 along −Z with `max Z distance` and `Z clearance`:
fast touch, retract to `surface + clearance`, optional slow touch of
`2·clearance`, retract again. `surface` = probed Z (not compensated). Emits
`z_minus_probed_position` / `#1009` = surface (in the coordinates in effect
*before* any write). If result mode = 0, sets the active WCS Z offset so the
surface becomes Z0 (offset = surface + current Z offset). Returns
`surface + clearance`. Ends `Z clearance` above the surface, absolute mode.

Callers then move relative to that height: they treat "the clearance plane"
as the current Z after this call.

### 3.4 Guarded positioning move

Every XY or Z positioning move between touches inside the probe macros is a
"probe towards, no error" move (G38.3) at the stated feed, followed by a
check: if contact occurred (`#5070` = 1) abort with "Probe made unexpected
contact during protected positioning move" (repo-own helper
`mymill_probe_assert_no_contact`). An implementation must keep that
fail-closed behaviour: a positioning move that hits something must stop and
abort, never continue. (The repo-own `mymill_probe_safe_move_x|y|z` do one
such incremental move plus the check.)

`z_stack` = `#3021 + d + #3022`: the plunge from the clearance plane to
probing depth. After a plunge the tip's lowest point is `d + #3022` below the
probed top.

### 3.5 Common frame of the Probe Basic family

Applies to every vendor macro in §4–§9 unless stated otherwise:

- Reads `#3014..#3024`, `#3030`, `#3032` (and the family's hints) at the
  start; the probe-tool check aborts when the spindle does not hold
  tool `#3014`.
- Cancels G92 offsets; turns feed override off (M50 P0) for the run and back
  on (M50 P1) at the end. After an abort, `on_abort` turns it back on.
- Work-offset writes use `G10 L2 P<active WCS>` with value
  `result + current offset of that axis`, so the result becomes 0 in the
  active WCS. They happen only when `#3030` = 0, and after the final move.
- No `#<_task>` guard: these macros move the machine in preview too, which is
  why the job wraps them (§2.3). A new implementation may add the guard.
- In position-only mode (`#3030` = 1) the corner/edge-angle macros abort if
  an edge helper returned no value (message: no X/Y edge value came
  back).
- Units: none assumed; everything is in program units. Ferrum only runs
  them in mm.

---

## 4. Single edge and top face

### 4.1 probe_x_plus_wco, probe_x_minus_wco, probe_y_plus_wco, probe_y_minus_wco

Start: probe already at probing depth beside the face (no Z motion at all),
within `#3018` of it, approaching from outside.

Behaviour: exactly one edge touch (§3.2) with reach `#3018`, then, if
`#3030` = 0, write that axis of the active WCS so the edge is 0.

| Macro | Ferrum spot | Probe moves | Edge | Widget / param | Writes |
|---|---|---|---|---|---|
| probe_x_plus_wco | LEFT EDGE | +X | touch + r_eff | x_plus_probed_position / #1006 | X |
| probe_x_minus_wco | RIGHT EDGE | −X | touch − r_eff | x_minus_probed_position / #1005 | X |
| probe_y_plus_wco | FRONT EDGE | +Y | touch + r_eff | y_plus_probed_position / #1008 | Y |
| probe_y_minus_wco | BACK EDGE | −Y | touch − r_eff | y_minus_probed_position / #1007 | Y |

End: `#3019` off the face, same Z, G90, F = traverse.

### 4.2 probe_z_minus_wco

Start: probe above the top face within `#3020`. One top touch (§3.3) with
`#3020`, `#3021`, writing Z when `#3030` = 0. Emits
`z_minus_probed_position` / `#1009` (pre-write work Z of the surface). Ends
`#3021` above the surface (in old coordinates; after the write that is
Z = `#3021`). Reads only `#3014..#3017`, `#3020`, `#3021`, `#3030`.

---

## 5. Corners outside (probe_{front,back}_{left,right}_top_corner)

Start: probe above the stock's top, near the chosen corner, less than
`#3023` (step off) from both faces that form it, within `#3020` of the top.

Sequence:

1. Top touch (§3.3); sets Z0 at the top when `#3030` = 0.
2. Guarded move outward along X by `#3023` (off the stock, past the X face).
3. Guarded plunge `z_stack` at fast feed.
4. Edge touch along X towards the stock (reach `#3018`) → X edge.
5. Guarded rise `z_stack`.
6. Guarded diagonal move: back towards the stock in X by `#3023` and outward
   in Y by `#3023`.
7. Guarded plunge `z_stack`; edge touch along Y towards the stock → Y edge.
8. Guarded rise `z_stack`; guarded absolute move to (X edge, Y edge): the
   probe ends over the corner at the clearance height.
9. If `#3030` = 0: X and Y offsets so the corner is X0 Y0.

| Macro | Corner | First off-move | X touch | Y off-move | Y touch |
|---|---|---|---|---|---|
| probe_front_left_top_corner | X−, Y− | −X | +X (x_plus) | −Y | +Y (y_plus) |
| probe_front_right_top_corner | X+, Y− | +X | −X (x_minus) | −Y | +Y (y_plus) |
| probe_back_left_top_corner | X−, Y+ | −X | +X (x_plus) | +Y | −Y (y_minus) |
| probe_back_right_top_corner | X+, Y+ | +X | −X (x_minus) | +Y | −Y (y_minus) |

Outputs: `z_minus_probed_position`/#1009, the two edge widgets of §3.2
(#1005..#1008). No centre or width values. Writes Z (step 1), X, Y. End G90.

Geometric precondition: `#3023` must exceed the start's distance to each
face plus `r_eff` plus a margin, or the plunge lands on the stock (the
guarded plunge then aborts).

---

## 6. Corners inside (probe_{front,back}_{left,right}_inside_corner)

"Front left inside corner" is the front-left corner of a pocket (or of an
L-shaped step): the walls are on the −X and −Y sides of the corner.

Start: probe above the top surface **outside** the pocket, diagonally beyond
the corner, less than `#3023` from each wall line, within `#3020` of the top.

Sequence:

1. Top touch on the surrounding top surface (Z0 when `#3030` = 0).
2. Guarded diagonal move by `#3023` in both X and Y, into the pocket.
3. Guarded plunge `z_stack`; remember this X/Y as the start point.
4. Edge touch along X towards the X wall → X edge; guarded absolute move
   back to the start X.
5. Edge touch along Y towards the Y wall → Y edge; guarded back to the start
   Y.
6. Guarded rise `z_stack`; guarded absolute move to (X edge, Y edge).
7. If `#3030` = 0: X and Y offsets so the corner is X0 Y0.

| Macro | Pocket corner | Diagonal step | X touch | Y touch |
|---|---|---|---|---|
| probe_front_left_inside_corner | X−, Y− | +X +Y | −X (x_minus) | −Y (y_minus) |
| probe_front_right_inside_corner | X+, Y− | −X +Y | +X (x_plus) | −Y (y_minus) |
| probe_back_left_inside_corner | X−, Y+ | +X −Y | −X (x_minus) | +Y (y_plus) |
| probe_back_right_inside_corner | X+, Y+ | −X −Y | +X (x_plus) | +Y (y_plus) |

Outputs and writes as §5. End G90 over the corner at clearance height.

---

## 7. Edge angle (probe_top_{left,right,front,back}_edge_angle)

Start: above the top near the chosen edge (inside the stock by less than
`#3023`), with at least `#3024` of straight edge available in the direction
of the second point.

Sequence:

1. Set the active WCS rotation to 0 (always, in every result mode).
2. Remember the start coordinate of the axis along the edge.
3. Top touch (Z0 when `#3030` = 0).
4. Guarded move outward by `#3023` (off the edge); guarded plunge
   `z_stack`.
5. Edge touch towards the stock (reach `#3018`) → first edge value.
6. Guarded move along the edge by `#3024` at fast feed (at depth,
   `#3019` off the face).
7. Second edge touch → second edge value.
8. `delta` and `angle = atan2(delta, #3024)` in degrees (positive =
   counter-clockwise rotation of the part seen from above).
9. Guarded rise `z_stack`; guarded absolute move to (first edge, start
   coordinate along the edge).
10. If `#3030` = 0: write the probed axis so the first edge is 0, and the
    other axis (see Quirk Q5). If additionally `#3031` = 1, also write
    rotation = angle.

| Macro | Edge | Off-move | Touch | Second point | delta |
|---|---|---|---|---|---|
| probe_top_left_edge_angle | left (X−) | −X | +X | −Y by #3024 | second − first |
| probe_top_right_edge_angle | right (X+) | +X | −X | +Y by #3024 | first − second |
| probe_top_front_edge_angle | front (Y−) | −Y | +Y | +X by #3024 | second − first |
| probe_top_back_edge_angle | back (Y+) | +Y | −Y | −X by #3024 | first − second |

Outputs: `z_minus_probed_position`/#1009, two edge widgets, `edge_delta` /
#1011, `edge_angle` / #1012 (degrees). Ferrum labels the angle "Edge angle
(not applied)" because it requires `#3031` = 0.

---

## 8. Round and rectangular pocket and boss

### 8.1 probe_round_pocket_center_start (Ferrum "BORE")

Start: inside the bore near its centre, tip already below the top edge.
No Z motion, no Z write. Hint `D_h` = `#3025`.

Two passes, each:

1. X− edge touch, reach `#3018 + D_h/2`; X+ edge touch, reach
   `#3018 + D_h`; X centre = mean of the two stylus touches; guarded
   absolute move to X centre.
2. Y+ edge touch, reach `#3018 + D_h/2`; Y− edge touch, reach
   `#3018 + D_h`; Y centre likewise; guarded move to Y centre.

First pass emits `x_center_probed`/#1002 and `y_center_probed`/#1004.
Second pass emits `x_probed_width`/#1001 (inside width), `x_center_probed`,
`y_probed_width`/#1003, `y_center_probed`, then
`averaged_diam`/#1010 = mean of the two widths. Each touch also emits its
§3.2 edge widget. If `#3030` = 0 writes X and Y so the centre is X0 Y0.
Ends at the centre, same Z, G90.

### 8.2 probe_rect_pocket_center_start (Ferrum "POCKET CENTER")

Like 8.1 but **one pass**, separate hints: X reaches `#3018 + #3026/2` then
`#3018 + #3026`; Y reaches `#3018 + #3027/2` then `#3018 + #3027`.
Order: X−, X+, move to X centre, Y+, Y−, move to Y centre. Emits
`x_probed_width`/#1001, `x_center_probed`/#1002, `y_probed_width`/#1003,
`y_center_probed`/#1004 (no averaged diameter). Writes X and Y when
`#3030` = 0. No Z.

### 8.3 probe_round_boss

Start: above the boss's centre within `#3020`. Hint `D_h` = `#3025`.

1. Top touch (Z0 at the boss top when `#3030` = 0).
2. X pass: guarded −X by `D_h/2 + #3023`; plunge `z_stack`; X+ edge touch
   (reach `#3018`); rise `z_stack`; guarded +X by
   `D_h + #3023 + #3019`; plunge; X− edge touch; X centre = mean of the
   stylus touches; rise; guarded absolute move to X centre.
3. Y pass the same with −Y / Y+ touch then +Y / Y− touch; move to Y centre.
4. Repeat 2 and 3 once from the refined centre; compute widths
   (outside width formula) on this second pass.
5. Rise; guarded absolute move to (X centre, Y centre).
6. If `#3030` = 0, write X and Y so the centre is X0 Y0.

Outputs, in order: `z_minus_probed_position`, then (first pass)
`x_center_probed`, `y_center_probed`, then `x_probed_width`/#1001,
`x_center_probed`/#1002, `y_probed_width`/#1003, `y_center_probed`/#1004,
`averaged_diam`/#1010, plus the per-touch edge widgets. Ends over the centre
at the clearance plane, G90.

Precondition: the far-side plunge point is (first touch's retract point) +
`D_h + #3023 + #3019`; with a poor hint the plunge can land on the boss,
which the guarded plunge turns into an abort.

### 8.4 probe_boss_scan (Ferrum BOSS with a size hint of 0; Ferrum's own)

Arguments `[step] [max]`. Start: anywhere above the part's top within
`#3020`; no size hint. Needs `[step]` < `#3018`.

1. Top touch, as §8.3.
2. X scan (`ferrum_span_scan`): for −X then +X, guarded steps of `[step]`
   outward at the clearance plane; after each, a G38.3 straight down to the
   `z_stack` depth at `#3016`. A contact within half that depth of the top
   means still over the top (rise, step again); a lower contact is something
   beside the part above the probing depth, such as a vise jaw (rise, abort);
   none means the top has ended and the probe is at depth beside the part
   (edge touch back towards the part with reach `#3018`; rise; back over the
   start). Aborts if a side is still over the top after `[max]`.
   Centre and width as the outside width formula; guarded move over the
   centre.
3. Y scan the same.
4. X and Y again with `ferrum_span_outside`, using the scanned widths as the
   hint (§8.3 step 2), from the refined centre; these count.
5. If `#3030` = 0, write X Y Z as §8.3.

Outputs: the same as §8.3, plus one
`ferrum_scan.receive_point{kind, x, y, z}` per touch (kind 0 a touch-down
that found nothing, 1 a touch-down on the top, 2 a side face point with the
tip radius taken out; current work coordinates) for the 3D view.

---

## 9. Ridge and valley (slot)

### 9.1 probe_valley_x_center_start / probe_valley_y_center_start (Ferrum "SLOT")

Start: inside the slot, roughly centred, tip below the top. No Z motion.
X version: X− touch with reach `#3018 + #3028/2`, then X+ touch with reach
`#3018 + #3028`; centre = mean; inside width. Y version: Y+ touch
(`#3018 + #3029/2`) then Y− (`#3018 + #3029`). One pass.

| Macro | Emits | Writes when #3030 = 0 |
|---|---|---|
| probe_valley_x_center_start | x_probed_width/#1001, x_center_probed/#1002 | X |
| probe_valley_y_center_start | y_probed_width/#1003, y_center_probed/#1004 | Y |

Ends at the centre, same Z, G90.

### 9.2 probe_ridge_x / probe_ridge_y (Ferrum "RIDGE")

Start: above the ridge's middle within `#3020`. Hint `W` = `#3028` (X) or
`#3029` (Y).

1. Top touch (Z0 at the ridge top when `#3030` = 0).
2. Guarded move −(`W/2 + #3023`) across the ridge; plunge `z_stack`;
   + touch (reach `#3018`).
3. Rise `z_stack`; guarded move +(`W + #3023`) (note: unlike the boss, no
   `#3019` added); plunge; − touch.
4. Centre = mean; width = outside width.
5. Rise; guarded absolute move to the centre; write that axis when
   `#3030` = 0.

| Macro | Emits | Writes |
|---|---|---|
| probe_ridge_x | z_minus_probed_position/#1009, x_probed_width/#1001, x_center_probed/#1002 (+ edge widgets) | Z, X |
| probe_ridge_y | z_minus_probed_position/#1009, y_probed_width/#1003, y_center_probed/#1004 (+ edge widgets) | Z, Y |

Quirk Q8: in the existing Y version the two plunges are not followed by the
unexpected-contact check. An implementation should check them.

---

## 10. Tool setter and spindle nose (repo files, GPL-derived)

Both run with the setter input, not the probe: the probe manager in HAL
routes the tool setter to `motion.probe-input` when the probe is not the
loaded tool. Both use the repo-own mode-capture helpers and HAL latches.

### 10.1 tool_touch_off

Callers: Ferrum CAL › TOOL LENGTH (no arguments), and the M6 remap
`m6_toolchange_optional_touch_off` (repo-own) after the tool change when the
HAL pin `tool-touch-off-request-latch.out` is set (it first pulses digital
out 6 to reset the request and sets the tool with M61).

Inputs: `#3004..#3013`, `#3014`, `#5181..#5183` (G30 = setter position),
`#5400`, `#5410`, `#5422`.

Checks (abort, in this order):

- no tool loaded (`#5400` ≤ 0): a tool must be loaded;
- probe loaded (`#5400` = `#3014`): the probe is calibrated on a reference
  surface, not on the tool setter;
- diameter offset mode on with a tool diameter > 0 and `#3013` outside 0..3;
- coordinate readiness (§12): cutter comp, WCS rotation, G52/G92;
- `#3004` ≤ 0, `#3005` < 0 or `#3006` ≤ 0; `#3007` ≤ 0 or `#3009` ≤ 0;
- `#3011` = 1: tool diameter probing is not implemented;
- mode capture (§13) refuses if a previous recovery is pending.

Motion:

1. Spindle stop (M5). Capture modes (kind 1), which also selects mm and
   units-per-minute feed. Feed override off.
2. Touch point = G30 X/Y. If `#3012` = 1 and the tool diameter > 0, shift it
   by the tool radius: `#3013` 0 → X−, 1 → X+, 2 → Y−, 3 → Y+.
3. Tool length offset off (G49). Absolute, machine-coordinate moves at
   `#3006`: Z to machine 0, then X/Y to the touch point.
4. Air blast: digital out 5 on for 1 s, then off.
5. Machine-coordinate move down to the G30 Z (`#5183`). Remember the current
   program Z there (`z_start`).
6. Probing move down `#3007` at `#3004`, contact optional (G38.3); if no
   contact, abort (message: no tool contact, restore probe modes).
7. Retract up `#3009`. If `#3005` > 0: probing move down `2·#3009` at
   `#3005` (abort on miss), then move to
   trip + `#3009`.
8. Tool length offset off again; machine Z to 0.
9. `TLO = | #3010 + z_trip − z_start |` (z_trip = last trip Z, slow if done).
   Wait for motion to finish, then write the tool table length of tool
   `#5400` (G10 L1 Z).
10. Restore modes with kind "new measured table offset" (applies G43 H<tool>)
    and release the recovery latch.

Outputs: tool table entry only. No DEBUG/EVAL lines (Ferrum shows "Done").
Units: assumes mm (forces G21 during the run).

Quirk Q9: the formula is right only when the G30 Z equals machine Z 0,
because `#3010` is measured from machine Z 0 (§10.2) while the tool descent
starts at G30 Z. With G30 Z = h (below 0), TLO comes out as `L − h`. On
mymill G30 Z ≈ 0.001.

### 10.2 probe_spindle_nose

Purpose: measure the distance from machine Z 0 down to where the bare
spindle nose trips the setter (`#3010`), and mark the nose reference valid
in HAL until the next unhome.

Inputs: `#3004`, `#3005`, `#3006`, `#3007`, `#3009`, `#3012`, `#3013`, INI
`[TOOL_SETTER]SPINDLE_NOSE_DIAMETER` (40 on mymill), HAL pin
`spindle-nose-reference-valid.out`, `#5181`, `#5182`, `#5400`, `#5422`.

Checks (abort): tool in spindle ≠ 0; nose reference already valid since
homing; diameter offset mode on with nose diameter ≤ 0 or `#3013` outside
0..3; coordinate readiness (§12); any of fast feed, traverse, max Z, retract
≤ 0 or slow < 0.

Motion:

1. Spindle stop; clear digital out 7; capture modes (kind 2); feed override
   off.
2. Touch point = G30 X/Y, shifted by the nose radius (INI diameter / 2) in
   the `#3013` direction when `#3012` = 1.
3. G49; machine Z 0; machine X/Y to the touch point; remember the program Z
   there (`z_top`).
4. Probing move down `#3007` at `#3004` (contact optional; abort on miss); move to trip + `#3009`.
5. If `#3005` > 0: probing move down `2·#3009` at `#3005` (abort on miss);
   move to trip + `#3009`.
6. `#3010 = | z_trip − z_top |`; machine Z 0; restore modes (kind normal).
7. Publish: `#1009` = z_trip, `#3010` as above, `#3007 = 1.001 · #3010`.
   Emit `z_minus_probed_position` (= z_trip), `spindle_zero_height_3010`,
   `z_max_travel_3007` (Ferrum saves both as new defaults, §2.4 rule 3).
8. Pulse digital out 7 (sets the HAL nose-reference flip-flop), wait, and
   abort if the pin did not go true (publication failed). Release the recovery latch.

Units: mm (explicitly "calibration in mm").

---

## 11. Multi-step flows (repo-own; they call GPL edge helpers)

These keep state in global named parameters between calls, so the stage
calls must run in the same LinuxCNC session. Both return immediately without
moving or changing state when `#<_task>` = 0. Both save and auto-restore
modal state (M73), so a successful call leaves modes as found.

### 11.1 probe_two_face (Ferrum "CENTER IN X" / "CENTER IN Y")

Arguments: `[axis]` 0 = X, 1 = Y; `[dir]` −1 or +1 (direction the probe
moves); `[stage]` 1 or 2; `[token]` positive integer.

Ferrum sends stage 1 with dir +1 when the first side is LEFT or FRONT and
−1 for RIGHT or BACK; stage 2 with the opposite dir after the operator jogs
to the other side.

Start (each stage): probe at depth beside the face, within `#3018`,
approaching from outside the stock.

Behaviour:

1. Invalidate any previous two-face result first.
2. Validate arguments (aborts: invalid axis / direction / stage / token).
3. Stage 2 only: the stored result must be a valid stage 1, same axis,
   opposite direction, and `token` > the stage-1 token, else abort
   "Opposite face does not match first touch" (or "First face is not
   valid"). Then the context must be unchanged (below).
   Stage 1: capture the context.
4. Abort if `#3015` < 0 or any of `#3016..#3019` ≤ 0; abort if
   `#3032` ≥ `#5410 / 2`.
5. Feed override off, G90 G94; one edge touch (§3.2, reach `#3018`) in
   `dir` → compensated edge.
6. Validate context again.
7. Stage 1: first = edge, others 0. Stage 2: second = edge,
   `width = (second − first) · dir₁` (dir₁ = stage-1 direction), abort "do
   not enclose stock" if ≤ 0; `center = (first + second)/2`.
8. Store token and stage as the valid state; emit
   `two_face_probe_panel.receive_result{stage, token, first, second, width, center}`.

Context = units, active WCS, tool number, tool diameter, tool-offset mode,
`#3032`, all nine offsets of the active WCS, the G52/G92/tool transforms on
every axis, and `#3014..#3019`. Context checks also abort on cutter comp,
WCS rotation or G52/G92 (§12 wording: "…before two-face probing").

Ferrum requires 6 values and value 1 = its token. **Quirk Q4.**

### 11.2 probe_two_face_apply

Argument `[token]`. No motion. Consumes the state first; aborts unless a
stage-2 result is stored ("Two completed faces are required"), the token
equals the stored token ("Stale two-face result"), the context is unchanged,
and `#3030` = 0 ("PROBE POS N is measurement only - select WCO mode to set
zero"). Then sets the current WCS (G10 L20 P0) on that axis so the
current position reads `current − center` (i.e. the centre becomes 0).
Emits `two_face_probe_panel.receive_result{3, token, first, second, width, center}`.

### 11.3 probe_auto_stock_center (Ferrum "STOCK CENTER")

Arguments: `[side]` 0 LEFT, 1 RIGHT, 2 FRONT, 3 BACK (the face the probe
starts beside); `[span]` max outward search span (> 0); `[token]` positive
integer.

Start: probe at depth beside the chosen face (outside the stock, within
`#3018`). **No Z motion, no work-offset write, no move across the stock.**

Checks: side, span, token, feeds/reach/clearance (`#3015` ≥ 0, `#3016..#3019`
> 0), tip (`#5410` > 0 and `r_eff` > 0); capture context (as §11.1 but with
the current absolute Z included, so Z must not move between measure and
apply).

Definitions: normal axis = X for LEFT/RIGHT, Y for FRONT/BACK; normal dir =
+1 for LEFT/FRONT, −1 for RIGHT/BACK (direction of the first touch);
tangent axis = the other one. `step = min(#3018/4, (#3018 − 2·r_eff − 2·#3019)/2)`;
abort if `step` ≤ 0 or `span` < 2·step.

Exploratory touch = probing move (no error on miss) along an axis for up to
`#3018` at `#3016`; on a hit, back off `#3019` along the ray (never past the
start), then return to the start point with a guarded move. Result hit/miss.
Scan = repeat (guarded step along the scan axis by `step`, exploratory touch)
until the first miss; abort if `span` is used up first.

Sequence:

1. Precise edge touch (§3.2) along the normal → `near_edge`; remember the
   current normal coordinate (the "lane", `#3019` outside the face) and the
   tangent coordinate.
2. Scan along +tangent probing along the normal → `t_plus_miss` (just past
   the face's + end). Return to the start tangent coordinate; scan along
   −tangent → `t_minus_miss`.
3. `inside = near_edge + dir · (r_eff + 2·#3019)` (a lane just inside the
   near face); abort if it is ≥ span from `near_edge`. Guarded move along
   the normal to `inside` (beside the stock, beyond its −t end).
4. Precise edge touch along +tangent → `t_minus_edge`. Guarded back to
   `t_minus_miss`, back to the lane, along the lane to `t_plus_miss`, in to
   `inside`; precise touch along −tangent → `t_plus_edge`; guarded back to
   `t_plus_miss`.
5. Exploratory touch along −tangent must hit ("Adjacent face was not found
   at the interior sample").
6. Remaining span = `span − |inside − near_edge|` (abort if < step); scan
   along +normal·dir probing along −tangent → `far_miss`.
7. `far_lane = far_miss + dir · #3019` (abort if beyond span + `#3019`);
   guarded to it, then along the tangent to the mid of the tangent edges;
   precise touch along −normal·dir → `far_edge`.
8. Validate context and consistency (tangent edges ordered and ≤ 2·span
   apart; far edge beyond near edge in dir and ≤ span from it; the inside
   lane short of the far edge).
9. Assemble X/Y min and max, widths, centres; store them (valid = 1);
   emit `auto_stock_panel.receive_result{token, x_min, x_max, y_min, y_max, width_x, width_y, center_x, center_y}`
   (only when `#<_task>` = 1), plus two plain DEBUG summary lines. The
   existing macro also writes a trace log to
   `/tmp/mymill-auto-stock-trace.log` (optional).

Ends `#3019` outside the far face at the tangent midpoint, at the start Z.
Ferrum requires 9 values with value 0 = token; it reads size from values
5, 6 and centre from 7, 8.

### 11.4 probe_auto_stock_apply

Argument `[token]`. No motion. Consumes the stored result first; aborts
unless a valid measurement exists ("No completed automatic stock
measurement"), token matches ("Stale automatic stock result"), context is
unchanged (including Z height and the WCS offsets), widths > 0, and
`#3030` = 0 (same "PROBE POS N…" wording). Sets the current WCS (G10 L20 P0)
so the current position reads `current − center` on X and Y. Emits
`auto_stock_panel.receive_applied{token, center_x, center_y}` (Ferrum
ignores it and reports "Center written to G5x").

---

## 12. Coordinate readiness (shared precondition, repo-own)

Used by tool_touch_off, probe_spindle_nose and all `probe_a_*`. No motion.
Aborts when cutter compensation is on ("Cancel cutter compensation before
probing"), when the active WCS has a rotation ("Probing requires an
unrotated work coordinate system"), or when any of `#5211..#5219` (G52/G92)
is non-zero ("Clear G52 and G92 offsets before probing"). The two flows and
auto stock have their own copies with family-specific wording.

## 13. Mode capture and recovery latch (repo-own)

Used by tool_touch_off (kind 1) and probe_spindle_nose (kind 2).

- Begin: abort if cutter comp is on, or HAL pins `mode-recovery-pending.out`
  or `mode-recovery-inhibit.out` are set ("Restore probe modes on Utilities
  before starting another action"). Record units, distance mode, feed mode,
  feed, feed override, tool, WCS, G43 state and `motion.tooloffset.x|y|z|a`.
  Pulse digital out 8 (arms the HAL latch) and abort if
  `mode-recovery-pending.out` did not go true. Select G21 G94.
- Restore (argument 0 normal, 1 "new table offset" → G43 H<tool>, 2
  explicit recovery): abort if nothing saved, no recovery pending, tool
  changed, motion disabled, or spindle on; put back units, tool offset (G43
  H, or the recorded G43.1 values), feed mode and feed, distance mode, feed
  override; restores the WCS only for kind 3 (unused here).
- Complete: pulse digital out 9 (clears the latch); abort if it did not
  clear.

Quirk Q10: after an abort inside these macros the latch stays set and every
later touch-off/nose run aborts until `o<mymill_restore_modes> call` runs.
The old screen had a Utilities button for it; **Ferrum has none**.

---

## 14. Rotary A family (repo-own; uses `f360_*` helpers)

Common to all five:

- Coordinate readiness (§12). Abort if `#3021` < 10 ("Rotary probing
  requires Z clearance of at least 10 mm"); the diameter, deflection and
  cylinder routines also abort if `#3019` < 5 mm.
- Save modal state (M70) and restore it (M72) on every exit path they
  control; feed override off during the run. Force G21 G90 G94.
  **mm-only**: the 10 mm / 5 mm / 2.5 mm / 2 mm / 0.1 mm / 0.10 mm
  constants below are millimetres.
- Abort if `#5400` ≠ `#3014` ("Load configured probe tool…").
- A axis is assumed parallel to X; "rotary direction" argument: 1 means +A
  moves the top of the work towards the operator, −1 the other way, 0
  is taken as 1. Ferrum always passes 1.
- Helper semantics (any implementation): tip radius = `r_eff`; traverse =
  `#3017` (abort if ≤ 0); Z clearance = `#3021`; XY clearance = `#3019`.
  "Probe Z/Y (dir, dmax)": contact-required probing move of
  `min(dmax, #3020)` (Z) or `min(dmax, #3018)` (Y) at `#3016`; if `#3015`
  > 0, probe-away (G38.4) back off by the clearance, guarded completion of
  the back-off, and a contact-required slow touch of 2× the clearance.
  **Ends at contact (no retract)**; callers back off themselves.
  "Update Y/Z (wcs, hit, expected)": new offset = old offset + (hit −
  expected), written only when `#<_task>` = 1.
- Radial plausibility: `tol = max(0.25 · nominal radius, 2.0 mm)`; every
  measured radius must be within `tol` of the nominal, else abort naming the
  angle ("A+90 side radius is too far from diameter estimate", etc.).

### 14.1 probe_a_setup_cylinder_center `[est] [dir] [verify]`

Finds the rotary axis's Y and Z and makes it Y0 Z0 of the active WCS.
Start: above a convex reference on the rotary with the WCS Y0/Z0 roughly on
the axis. `est` = reference size estimate (> 0, used as a generous upper
bound), `margin = max(10 mm, #3019)`.

1. Guarded move to Y0. Probe Z down (span `est + 2·#3021 + margin + 4`) →
   `top`; probe-away back off `#3021`. Abort if `top` ≤ 0 or
   `top > est + margin`.
2. `standoff = est + margin + r_eff`; side span
   `2·standoff + 2·r_eff + 4`; clear height `top + est + #3021 + margin`.
3. Rise to clear height; rotate to `A_start + dir·90` (angles normalised to
   0..360); Y to −standoff; guarded Z down to Z0; probe +Y →
   `y1 = touch + r_eff`; probe-away back off `#3019`; rise.
4. Rotate to `A_start − dir·90`; Y to +standoff; guarded Z to 0; probe −Y
   → `y2 = touch − r_eff`; back off; rise.
5. `y_axis = (y1 + y2)/2`, `R = |y2 − y1|/2` (abort if 0),
   `z_axis = top − R`.
6. Verify pass (argument > 0.5; Ferrum passes 0): repeat 1–5, abort if
   either centre moved by more than 0.10 mm, average the two.
7. Rise; rotate back to `A_start`. If `#<_task>` = 1 and `#3030` = 0:
   add `y_axis` to the WCS Y offset and `z_axis` to the Z offset.

No EVAL output. Ends at clear height, Y = +standoff, A = start.

### 14.2 probe_a_rotary `[dir]`

Sets A0 of the active WCS where a flat on the work is level.
Start: probe above the flat at least 2.5 mm from WCS Y0 (abort otherwise).

1. Probe Z down `2·#3021` → `z1` at Y = `y1`; move to `z1 + #3021`.
2. Guarded Y move to −`y1` (mirror across Y0); probe Z → `z2`; move to
   `z2 + #3021`.
3. `angle = atan2(z(+Y side) − z(−Y side), 2·|y1|)` in degrees.
4. If `#<_task>` = 1 and `#3030` = 0: new A offset =
   `old A offset − dir·angle + current A (#5423)`, so the current position
   reads `A = dir·angle` and A0 is the level orientation.

No EVAL output. Ends above the mirrored point, A unchanged.

### 14.3 probe_a_measure_diameter `[dia] [dir]`

Requires WCS Y0/Z0 on the rotary axis. Start above the cylinder near Y0.

1. Guarded Y to 0. At `A_start`: probe Z (span `dia + 2·#3021`) →
   `r_top = Z` (must be > 0 and plausible). Clear height =
   `r_top + dia + #3021`.
2. Guarded Y to −(`dia/2 + #3019 + r_eff + 2`); rotate `A_start + dir·90`;
   guarded Z to 0; probe +Y (span `dia + 2·#3019 + 2·r_eff + 4`, capped at
   `#3018`) → `r1 = |touch + r_eff|`; back off `#3019`; rise.
3. Same from +standoff at `A_start − dir·90`, probing −Y →
   `r2 = |touch − r_eff|`.
4. `diameter = 2·mean(r_top, r1, r2)`, `spread = 2·(max − min)`.
5. Emit (only when `#<_task>` = 1) `measure_diameter_value`,
   `measure_diameter_spread_value`. Rotate back to `A_start`; guarded Y to 0.

### 14.4 probe_a_measure_runout `[dia] [dir]`

Guarded Y to 0; four top touches at `A_start + k·dir·90`, k = 0..3, each
radius = probed Z (> 0, plausible), rising to `z0 + dia + #3021` between.
`TIR = max − min`; `high angle = k·dir·90` of the first maximum (0 if the
first touch is highest; signed, not normalised); `diameter = 2·mean`.
Emits `measure_runout_tir_value`, `measure_runout_high_angle_value`,
`measure_runout_diameter_value`. Ends at `A_start`, Y0, clear height. No
tip-radius use.

### 14.5 probe_a_measure_deflection `[dia] [span] [dir]`

`|span|` ≥ 0.1 mm. Two stations: the start X and start X + span (guarded X
move between, at Y0, A start).

At each station: top touch at `A_start` (`z0`), side touches at
`A_start ± dir·90` as in 14.3 (`y+`, `y−`, signed), top touch at
`A_start + 2·dir·90` (`z180`), each radius plausibility-checked. Station
centre: `yc = (y+ + y−)/2`, `zc = (z0 + z180)/2`.

`y_drift = yc_far − yc_near`, `z_drift = zc_far − zc_near`,
`y_angle = atan2(y_drift, span)`, `z_angle = atan2(z_drift, span)` (deg).
Emits `measure_deflect_y_delta_value`, `measure_deflect_z_delta_value`,
`measure_deflect_y_angle_value`, `measure_deflect_z_angle_value`. Returns
to `A_start`, Y0 and the start X (guarded).

---

## 15. Calibration: probe_cal_round_pocket (Ferrum CAL › PROBE TIP)

Ring gauge of known diameter `#3033`. Start: above the gauge's top face,
near the **left (X−) wall**, less than `#3023` outside the bore edge, within
`#3020` of the top.

1. Set `#3032` = 0 and emit `calibration_offset_3032` = 0 (Quirk Q2).
2. Probe-tool check; `r_eff` is now `d/2`.
3. Top touch on the gauge (Z0 there if `#3030` = 0, Quirk Q6).
4. Guarded +X by `#3023` into the bore; guarded plunge `z_stack`.
5. X− touch (reach `#3018`), X+ touch (reach `#3018 + #3033`); X centre;
   guarded to it. Y+ touch (`#3018 + #3033/2`), Y− touch
   (`#3018 + #3033`); Y centre; guarded to it. (Emits first-pass
   `x_center_probed`, `y_center_probed`.)
6. Second pass from the centre: X− (`#3018 + #3033/2`), X+
   (`#3018 + #3033`), widths with `d` (inside formula, cal = 0), then Y.
   Emits `x_probed_width`, `x_center_probed`, `y_probed_width`,
   `y_center_probed`, `averaged_diam` (#1001..#1004, #1010).
7. Guarded move to Y centre; guarded rise `z_stack` (incremental — the
   macro ends in G91, Quirk Q7).
8. Per axis `e = |#3033 − width| / 2`. `#3032` = |mean(eX, eY)| when
   `#3036` = 0, |eX| when 1, |eY| otherwise. Emit
   `calibration_offset_3032` with the new value (Ferrum saves it).
9. If `#3030` = 0: write X and Y of the active WCS to the gauge centre.

Formula check: with a true effective radius `r_t`, width (cal 0) =
`D − 2·r_t + d`, so `e = d/2 − r_t` and `r_eff = d/2 − e = r_t`. Correct
only when `r_t ≤ d/2` (Quirk Q1).

---

## 16. Quirks implementers must not miss

| # | Quirk | Recommendation |
|---|---|---|
| Q1 | The calibration offset is *subtracted* from the radius, and the calibration routine takes the absolute value, so it can never represent an effective radius larger than `d/2` (tool-table tip diameter too small). | Keep the sign convention (`r_eff = d/2 − #3032`); allow a signed result, or check that the table diameter is not below the true ball. |
| Q2 | The cal routine emits `calibration_offset_3032 = 0` before measuring; Ferrum saves it as the default at once, so a failed calibration leaves the saved offset at 0. | Emit `calibration_offset_3032` once, only with the final value. Set `#3032` = 0 internally. |
| Q3 | `#3036` semantics: macro 0 = average of X and Y, 1 = X, 2 = Y. Ferrum labels the values 0 "X", 1 "Y", 2 "X+Y". | Decide one meaning and fix Ferrum's label table (`PARAMS` in probe.rs) or the macro together. |
| Q4 | probe_two_face stage 2 requires `token` > stage-1 token, but Ferrum sends the **same** token in stage 2 (and that token again to apply), so with the current macro the second face always aborts "does not match first touch". The old QtPyVCP panel used a new, larger token for stage 2 and passed that one to apply. | Either accept `token ≥` the stored token in stage 2, or change Ferrum to send a new token for stage 2 and that stage-2 token to apply (apply must equal the stage-2 token). Keep value 1 of the result = the token sent. |
| Q5 | Edge-angle macros write the *other* axis' offset as the raw start coordinate (Y for left/right, X for front/back) instead of "start + current offset", which is only meaningful when that offset was 0. They also always zero the WCS rotation, even in position-only mode. | Leave the other axis unchanged (or zero it at the start point properly) and only touch rotation when `#3031` = 1. Coordinate with Ferrum's result text. |
| Q6 | CAL (ring) and MEASURE runs do not force `#3030`; with the mymill parameter file (`#3030` = 0) the ring calibration writes Z (ring top) and X/Y (ring centre) into the active WCS. | Calibration should never write a WCS; ignore `#3030` there. |
| Q7 | probe_cal_round_pocket ends in G91. Ferrum and the machine worker restore the distance mode, but a new macro should end in G90. | End all macros in G90. |
| Q8 | probe_ridge_y's plunges are unchecked for contact. | Check every positioning move. |
| Q9 | tool_touch_off is exact only when G30 Z = machine Z 0 (§10.1). | Measure the nose reference and the tool from the same start height, or include `#5183` in the formula. |
| Q10 | Mode-recovery latch with no Ferrum button (§13). | Add the button or make the macros self-recovering. |
| Q11 | Vendor macros have no `#<_task>` guard and cancel G92 silently. | Guard motion and state with `#<_task>`; abort instead of cancelling G92. |
| Q12 | Round boss / bore / ring emit centre values twice (first and second pass), and every edge touch emits its own `*_probed_position` line; Ferrum lists all of them. | Optional to keep; the final values must come last. |
| Q13 | The edge helper's fast and slow moves are contact-required (G38.2), so a miss is a LinuxCNC probe error rather than the macro's own "no probe point recorded" message. | Either is acceptable to Ferrum (any error fails the run). |
| Q14 | Many routines report `#5061`/`#5062` (stylus centre) rather than the compensated edge returned by the helper; centres are therefore uncompensated means and widths are compensated afterwards. | Equivalent results; keep the documented formulas. |

---

## 17. Widget names and numbered results (complete list)

| Widget name (exact) | Value | Param | Emitted by |
|---|---|---|---|
| x_minus_probed_position | compensated edge, probe moving −X | #1005 | probe_x_minus(_wco) and every caller |
| x_plus_probed_position | compensated edge, moving +X | #1006 | probe_x_plus(_wco) and callers |
| y_minus_probed_position | compensated edge, moving −Y | #1007 | probe_y_minus(_wco) and callers |
| y_plus_probed_position | compensated edge, moving +Y | #1008 | probe_y_plus(_wco) and callers |
| z_minus_probed_position | probed top Z (pre-write coords); nose: raw trip Z | #1009 | z touches, probe_spindle_nose |
| x_probed_width | width in X | #1001 | pockets, bore, boss, valley X, ridge X, cal |
| x_center_probed | centre X | #1002 | same |
| y_probed_width | width in Y | #1003 | pockets, bore, boss, valley Y, ridge Y, cal |
| y_center_probed | centre Y | #1004 | same |
| averaged_diam | mean of X and Y widths | #1010 | bore, boss, cal |
| edge_delta | second − first edge (signed per §7) | #1011 | edge angle |
| edge_angle | degrees, CCW positive | #1012 | edge angle |
| calibration_offset_3032 | new `#3032` | #3032 | cal |
| spindle_zero_height_3010 | new `#3010` | #3010 | probe_spindle_nose |
| z_max_travel_3007 | new `#3007` | #3007 | probe_spindle_nose |
| measure_diameter_value / measure_diameter_spread_value | mm | — | probe_a_measure_diameter |
| measure_runout_tir_value / measure_runout_high_angle_value / measure_runout_diameter_value | mm, deg, mm | — | probe_a_measure_runout |
| measure_deflect_y_delta_value / measure_deflect_z_delta_value / measure_deflect_y_angle_value / measure_deflect_z_angle_value | mm, mm, deg, deg | — | probe_a_measure_deflection |
| two_face_probe_panel.receive_result{stage, token, first, second, width, center} | 6 values | — | probe_two_face (stage 1/2), apply (stage 3) |
| auto_stock_panel.receive_result{token, x_min, x_max, y_min, y_max, width_x, width_y, center_x, center_y} | 9 values | — | probe_auto_stock_center |
| auto_stock_panel.receive_applied{token, center_x, center_y} | 3 values | — | probe_auto_stock_apply |

Digital outputs and HAL pins used: out 5 setter air blast, out 6 touch-off
request reset (remap), out 7 nose reference publish, out 8 / out 9 mode
recovery arm / clear; pins `mode-recovery-pending.out`,
`mode-recovery-inhibit.out`, `spindle-nose-reference-valid.out`,
`tool-touch-off-request-latch.out`, `motion.tooloffset.x|y|z|a`,
`motion.motion-enabled`, `spindle.0.on`; INI
`[TOOL_SETTER]SPINDLE_NOSE_DIAMETER`.

Units summary: the vendor (Probe Basic) families are unit-agnostic; the
tool setter, spindle nose and rotary macros force or assume mm. Ferrum's
own macros work in machine units (mm or inch) and refuse to run when the
program units differ from the machine's.

---

## 18. Test geometry needs

What a simulator (Ferrum's sim or the LinuxCNC harness, whose only part
today is an outside block plus the setter at G30) needs so each family can
be exercised. Sizes are suggestions in mm; all walls vertical and at least
`d + #3022 + 2` deep so a plunge of `z_stack` reaches them; tip diameter as
in the tool table (harness T99: D2; mymill: D3), and a configurable
pre-travel to exercise calibration.

| Family | Feature | Notes |
|---|---|---|
| Single edge, top face | Rectangular block (exists: X40–120, Y−60..−10, top −100 in sim-mm) | each of the four faces and the top |
| Corners outside | Same block | all four corners; start inside the step-off square |
| Corners inside | Rectangular pocket, e.g. 60 × 40, 15 deep, or an L-shaped step | all four inside corners; a top surface around the pocket wide enough for the Z touch beyond each corner |
| Edge angle | The block rotated a known angle (e.g. 0.5° and −2°) about Z | faces at least `#3023 + #3024` long |
| Round pocket (bore) | Bore Ø25–40, 15 deep, off-centre start | plus the hint `#3025` |
| Rect pocket | Same pocket as inside corners | hints `#3026/#3027` |
| Round boss | Cylinder Ø40, 15 high, standing on a flat larger than Ø40 + 2·(#3023 + #3019) | |
| Ridge | Rib 20 wide, 10 high, running along Y (ridge X) and along X (ridge Y) | room on both sides for the step-off |
| Valley / slot | Slot 20 wide, 10 deep, both orientations | |
| Two faces / auto stock | The block; for auto stock a start beside each of the four faces and a span larger than the block | stock must be fully clear of fixtures within `span` |
| Calibration | Ring gauge: bore Ø`#3033` (12.7 on mymill, 25 default) with a top face around it ≥ `#3023` wide; tip with known pre-travel | check `#3032` ≈ pre-travel |
| Tool setter | Setter plate at G30 X/Y, trip height known; tools of several lengths and diameters; diameter-offset mode on and off | |
| Spindle nose | Same setter; empty spindle; nose diameter from INI | HAL `spindle-nose-reference-valid` flip-flop, digital out 7 |
| Rotary centre / flat | A axis along X carrying a cylinder (Ø30) with its axis offset from rough Y0/Z0 by a few mm, and a part with a flat at a known A angle | needs `sim-xyza`-style kinematics |
| Rotary diameter / runout | Cylinder Ø25 on A, plus an eccentric variant (e.g. 0.05 offset) for TIR | |
| Rotary deflection | Cylinder ≥ 60 long whose axis is tilted a known small angle in Y and Z relative to A | |

A simulator also needs: contact to be geometric for G38.2/3/4/5 (ball of
radius `d/2` minus pre-travel against the solids), `#5061..#5063` and
`#5070` set accordingly, and the probe-manager routing (probe input only
with the probe loaded, setter input otherwise).
