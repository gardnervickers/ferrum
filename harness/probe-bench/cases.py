"""The probe bench's test cases (see bench.py for what is checked).

Positions are machine mm in the mymill simulator's world (world.py,
ferrum/sim/sim_card.py; the same numbers as harness/configs/mymill/vars).
The inch bench runs the same physical cases, converted.

Case keys: name, expect ("ok" must run and be right, "refuse" must fail
without moving, "safe" may fail but must not crash and must not give a
wrong result), steps [{start (x, y, machine z), pre [MDI lines run after
placing], call (a length written {L:mm} is converted to machine
units), stop_at (secs, "abort"|"estop")}], params {#: value in
machine units}, checks [(kind, key, want mm [, tol mm])], obstacles (bit
mask of world.OBSTACLES), tool, tlo_tool, tool_table [MDI lines],
length_error (mm), results_unchecked, refuse_text.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from make_config import PARAMS_MM, PROBE_D_MM, PROBE_L_MM, UNITLESS, conv  # noqa: E402

PROBE_D = PROBE_D_MM
PROBE_L = PROBE_L_MM
TRAVEL_Z = -50.0
WCS0 = (12.5, -7.25, -30.0)
GEOM_TOL = 0.02
ANGLE_TOL = 0.02
L = PROBE_L_MM


def mz(tip):
    """Machine Z that puts the probe tip at a height."""
    return tip + L


def params(units):
    return {n: conv(n, v, units) for n, v in PARAMS_MM.items()}


# ---- The world (sim_card.py) ----------------------------------------------
STOCK = dict(x0=150.0, x1=280.0, y0=-140.0, y1=-40.0, top=-250.0, cx=215.0, cy=-90.0)
STOCK_DEPTH = mz(-255.0)
STOCK_ABOVE = mz(-245.0)
PLATE_TOP = -260.0
PLATE_ABOVE = mz(-250.0)
PLATE_DEPTH = mz(-265.0)
FEATURE_TOP = -250.0
FEATURE_ABOVE = mz(-245.0)
TILT_ABOVE = mz(-250.0)
RING_ABOVE = mz(-260.0)


def base_cases():
    """Every routine once from a good start: (case, its fuzzable knobs)."""
    S = STOCK
    out = []

    def add(name, steps, checks, params=None, **kw):
        d = dict(name=name, expect="ok", steps=steps, checks=checks, params=params or {})
        d.update(kw)
        out.append(d)

    # Edges, at depth beside each face (#3030 = 0 writes G54).
    add("edge left (x_plus)", [dict(start=(135, S["cy"], STOCK_DEPTH), call="o<probe_x_plus_wco> call")],
        [("g5x", "X", S["x0"]), ("g5x", "Y", WCS0[1], 1e-4), ("g5x", "Z", WCS0[2], 1e-4)])
    add("edge right (x_minus)", [dict(start=(292, S["cy"], STOCK_DEPTH), call="o<probe_x_minus_wco> call")],
        [("g5x", "X", S["x1"])])
    add("edge front (y_plus)", [dict(start=(S["cx"], -155, STOCK_DEPTH), call="o<probe_y_plus_wco> call")],
        [("g5x", "Y", S["y0"])])
    add("edge back (y_minus)", [dict(start=(S["cx"], -25, STOCK_DEPTH), call="o<probe_y_minus_wco> call")],
        [("g5x", "Y", S["y1"])])
    add("top (z_minus)", [dict(start=(415, -165, PLATE_ABOVE), call="o<probe_z_minus_wco> call")],
        [("g5x", "Z", PLATE_TOP), ("g5x", "X", WCS0[0], 1e-4)])
    # Outside corners of the STOCK block: 3 in from each face, above the top.
    for nm, x, y, cx, cy in [("front_left", 153, -137, S["x0"], S["y0"]), ("front_right", 277, -137, S["x1"], S["y0"]),
                             ("back_left", 153, -43, S["x0"], S["y1"]), ("back_right", 277, -43, S["x1"], S["y1"])]:
        add(f"outside corner {nm}", [dict(start=(x, y, STOCK_ABOVE), call=f"o<probe_{nm}_top_corner> call")],
            [("g5x", "X", cx), ("g5x", "Y", cy), ("g5x", "Z", S["top"])])
    # Inside corners of the rect pocket (X305..345 Y-105..-75), from over the plate.
    for nm, x, y, cx, cy in [("front_left", 302, -108, 305, -105), ("front_right", 348, -108, 345, -105),
                             ("back_left", 302, -72, 305, -75), ("back_right", 348, -72, 345, -75)]:
        add(f"inside corner {nm}", [dict(start=(x, y, PLATE_ABOVE), call=f"o<probe_{nm}_inside_corner> call")],
            [("g5x", "X", cx), ("g5x", "Y", cy), ("g5x", "Z", PLATE_TOP)])
    add("round boss", [dict(start=(325, -35, FEATURE_ABOVE), call="o<probe_round_boss> call")],
        [("g5x", "X", 325), ("g5x", "Y", -35), ("g5x", "Z", FEATURE_TOP), ("param", 1010, 30)], {3025: 30})
    add("boss 3 point", [dict(start=(324, -33.5, FEATURE_ABOVE), call="o<probe_boss_3_point> call [180] [240] [300]")],
        [("g5x", "X", 325), ("g5x", "Y", -35), ("g5x", "Z", FEATURE_TOP), ("param", 1010, 30)], {3025: 30})
    add("bore", [dict(start=(380, -90, PLATE_DEPTH), call="o<probe_round_pocket_center_start> call")],
        [("g5x", "X", 380), ("g5x", "Y", -90), ("g5x", "Z", WCS0[2], 1e-4), ("param", 1010, 30)], {3025: 30})
    add("bore 3 point", [dict(start=(382, -92, PLATE_DEPTH), call="o<probe_bore_3_point> call [45] [160] [280]")],
        [("g5x", "X", 380), ("g5x", "Y", -90), ("param", 1010, 30), ("pos", "X", 380), ("pos", "Y", -90)], {3025: 30})
    add("rect pocket", [dict(start=(325, -90, PLATE_DEPTH), call="o<probe_rect_pocket_center_start> call")],
        [("g5x", "X", 325), ("g5x", "Y", -90), ("param", 1001, 40), ("param", 1003, 30)], {3026: 40, 3027: 30})
    add("ridge x", [dict(start=(325, -145, FEATURE_ABOVE), call="o<probe_ridge_x> call")],
        [("g5x", "X", 325), ("g5x", "Z", FEATURE_TOP), ("param", 1001, 12), ("g5x", "Y", WCS0[1], 1e-4)], {3028: 15})
    add("ridge y", [dict(start=(380, -145, FEATURE_ABOVE), call="o<probe_ridge_y> call")],
        [("g5x", "Y", -145), ("g5x", "Z", FEATURE_TOP), ("param", 1003, 12)], {3029: 15})
    add("valley x", [dict(start=(180, -90, STOCK_DEPTH), call="o<probe_valley_x_center_start> call")],
        [("g5x", "X", 180), ("param", 1001, 14), ("g5x", "Z", WCS0[2], 1e-4)], {3028: 14})
    add("valley y", [dict(start=(247.5, -65, STOCK_DEPTH), call="o<probe_valley_y_center_start> call")],
        [("g5x", "Y", -65), ("param", 1003, 14)], {3029: 14})
    # Edge angles on the block turned +2 deg (top -255).
    for nm, x, y, axis, edge in [("left", 47.1, -100, "X", 44.1017), ("right", 122.9, -150, "X", 125.8983),
                                 ("front", 60, -162.9, "Y", -165.8983), ("back", 110, -87.1, "Y", -84.1017)]:
        add(f"edge angle {nm}", [dict(start=(x, y, TILT_ABOVE), call=f"o<probe_top_{nm}_edge_angle> call")],
            [("g5x", axis, edge), ("g5x", "Z", -255.0), ("angle", 1012, 2.0)])
    add("skew left edge", [dict(start=(47.1, -100, TILT_ABOVE), call="o<probe_edge_skew> call [0] [-1] [{L:-45}] [{L:5}]")],
        [("rot", "", 2.0), ("g5x", "X", WCS0[0], 1e-4), ("angle", 1012, 2.0)])
    add("auto stock (front start) + apply",
        [dict(start=(S["cx"], -155, STOCK_DEPTH), call="o<probe_auto_stock_center> call [2] [{L:150}] [7]", timeout=900),
         dict(call="o<probe_auto_stock_apply> call [7]")],
        [("g5x", "X", S["cx"]), ("g5x", "Y", S["cy"]), ("g5x", "Z", WCS0[2], 1e-4)])
    add("two face X + apply",
        [dict(start=(135, S["cy"], STOCK_DEPTH), call="o<probe_two_face> call [0] [1] [1] [5]"),
         dict(start=(292, S["cy"], STOCK_DEPTH), call="o<probe_two_face> call [0] [-1] [2] [5]"),
         dict(call="o<probe_two_face_apply> call [5]")],
        [("g5x", "X", S["cx"]), ("g5x", "Y", WCS0[1], 1e-4)])
    add("cal ring", [dict(start=(32, -60, RING_ABOVE), call="o<probe_cal_round_pocket> call")],
        [("param", 3032, 0.059025, 0.01)], {3033: 20, 3032: 0.5})
    add("cal pin", [dict(start=(325, -35, FEATURE_ABOVE), call="o<probe_cal_boss> call")],
        [("param", 3032, 0.059025, 0.01)], {3034: 30, 3032: 0.5})
    add("cal ring + stylus check",
        [dict(start=(32, -60, RING_ABOVE), call="o<probe_cal_round_pocket> call"),
         dict(call="o<probe_cal_stylus> call [0]")],
        [("param", 1002, 45 - WCS0[0]), ("param", 1004, -60 - WCS0[1])], {3033: 20})
    return out


def refuse_cases():
    """Wrong machine states every routine must refuse without moving."""
    edge = dict(start=(135, STOCK["cy"], STOCK_DEPTH), call="o<probe_x_plus_wco> call")
    boss = dict(start=(325, -35, FEATURE_ABOVE), call="o<probe_round_boss> call")
    out = []

    def add(name, step, text, **kw):
        d = dict(name=f"refuse: {name}", expect="refuse", steps=[step], checks=[], refuse_text=text)
        d.update(kw)
        out.append(d)

    for nm, st in (("edge", edge), ("boss", boss)):
        add(f"{nm} in G20 on a mm machine", dict(st, pre=["G20"]), "machine's own units")
        add(f"{nm} with G92 offset", dict(st, pre=["G92 X0"]), "G52 and G92")
        add(f"{nm} with G52 offset", dict(st, pre=["G52 X5"]), "G52 and G92")
        add(f"{nm} in rotated WCS", dict(st, pre=["G10 L2 P1 R5"]), "unrotated")
        add(f"{nm} with no TLO (G49)", dict(st, pre=["G49"]), "tool length offset")
        add(f"{nm} with another tool's TLO", dict(st, pre=["G43 H1"]), "not the probe's")
        add(f"{nm} with cutter comp", dict(st, pre=["G41.1 D3"]), "cutter compensation")
        add(f"{nm} with an end mill loaded", dict(st), "Load the probe", tool=1)
        add(f"{nm} with tip D 0", dict(st), "tip diameter", tool_table=["G10 L1 P99 R0"])
        add(f"{nm} with probe tool unset", dict(st), "probe tool number", params={3014: 0})
        for n, txt in ((3015, "slow feed"), (3016, "fast and traverse"), (3017, "fast and traverse"),
                       (3018, "XY search"), (3019, "XY search"), (3020, "Z search"), (3021, "Z search"),
                       (3022, "Extra probe depth")):
            add(f"{nm} with #{n} < 0", dict(st), txt, params={n: -1.0})
        for n, txt in ((3016, "fast and traverse"), (3017, "fast and traverse"), (3018, "XY search"),
                       (3019, "XY search"), (3020, "Z search"), (3021, "Z search")):
            add(f"{nm} with #{n} = 0", dict(st), txt, params={n: 0.0})
    return out


def obstacle_cases():
    """A fixture in the routine's path: it must stop on contact, not crash."""
    S = STOCK
    out = []

    def add(name, steps, mask, expect="safe", checks=(), **kw):
        d = dict(name=f"obstacle: {name}", expect=expect, steps=steps, checks=list(checks), obstacles=mask)
        d.update(kw)
        out.append(d)

    add("vise jaw at the right corners' step-off",
        [dict(start=(277, -137, STOCK_ABOVE), call="o<probe_front_right_top_corner> call")], 1 << 0)
    add("bolt beside the boss (boss)",
        [dict(start=(325, -35, FEATURE_ABOVE), call="o<probe_round_boss> call")], 1 << 1, params={3025: 30})
    add("bolt beside the boss (3 point leaving it out)",
        [dict(start=(325, -35, FEATURE_ABOVE), call="o<probe_boss_3_point> call [90] [210] [270]")], 1 << 1,
        expect="ok", checks=[("g5x", "X", 325), ("g5x", "Y", -35), ("param", 1010, 30)], params={3025: 30})
    add("bolt beside the boss (3 point touching it)",
        [dict(start=(325, -35, FEATURE_ABOVE), call="o<probe_boss_3_point> call [0] [120] [240]")], 1 << 1,
        params={3025: 30})
    add("post in the bore (bore)",
        [dict(start=(380, -90, PLATE_DEPTH), call="o<probe_round_pocket_center_start> call")], 1 << 2,
        params={3025: 30}, results_unchecked=True)
    add("post in the bore (3 point leaving it out)",
        [dict(start=(380, -90, PLATE_DEPTH), call="o<probe_bore_3_point> call [90] [180] [270]")], 1 << 2,
        expect="ok", checks=[("g5x", "X", 380), ("g5x", "Y", -90), ("param", 1010, 30)], params={3025: 30})
    add("clamp on the tilted block's face (edge angle)",
        [dict(start=(47.1, -100, TILT_ABOVE), call="o<probe_top_left_edge_angle> call")], 1 << 3)
    add("clamp on the tilted block's face (skew)",
        [dict(start=(47.1, -100, TILT_ABOVE), call="o<probe_edge_skew> call [0] [-1] [{L:-45}] [{L:5}]")], 1 << 3)
    add("clamp at the stock's front (auto stock)",
        [dict(start=(S["cx"], -155, STOCK_DEPTH), call="o<probe_auto_stock_center> call [2] [{L:150}] [9]", timeout=900)],
        1 << 4, results_unchecked=True)
    add("clamp at the stock's front (front left corner)",
        [dict(start=(153, -137, STOCK_ABOVE), call="o<probe_front_left_top_corner> call")], 1 << 4)
    add("block where the back left corner steps off in Y",
        [dict(start=(153, -43, STOCK_ABOVE), call="o<probe_back_left_top_corner> call")], 1 << 5)
    return out


def modal_cases():
    """Modal states a routine must work from and leave as they were."""
    out = []
    edge = dict(start=(135, STOCK["cy"], STOCK_DEPTH), call="o<probe_x_plus_wco> call")
    boss = dict(start=(325, -35, FEATURE_ABOVE), call="o<probe_round_boss> call")
    for nm, st, chk, prm in (("edge", edge, [("g5x", "X", STOCK["x0"])], {}),
                             ("boss", boss, [("g5x", "X", 325), ("g5x", "Y", -35), ("g5x", "Z", FEATURE_TOP)], {3025: 30})):
        for pre in (["G91"], ["G93"], ["G18"], ["G91.1"], ["F123"], ["G1 G91 F77"], ["M51 P0"]):
            out.append(dict(name=f"modal: {nm} from {' '.join(pre)}", expect="ok", steps=[dict(st, pre=pre)],
                            checks=chk, params=prm))
        # G55 active: the routine writes G55, not G54.
        out.append(dict(name=f"modal: {nm} in G55", expect="ok", steps=[dict(st, pre=["G10 L2 P2 X1 Y2 Z3", "G55"])],
                        checks=chk, params=prm))
        # 200 % feed override: M50 P0 inside the routine must hold it at 100.
        out.append(dict(name=f"modal: {nm} at 200% feed override", expect="ok",
                        steps=[dict(st, pre=["M50 P1"], override=2.0)], checks=chk, params=prm))
    return out


def stop_cases():
    """Abort and E-stop part way through: nothing may crash."""
    out = []
    boss = dict(start=(325, -35, FEATURE_ABOVE), call="o<probe_round_boss> call")
    corner = dict(start=(153, -137, STOCK_ABOVE), call="o<probe_front_left_top_corner> call")
    for nm, st in (("boss", boss), ("corner", corner)):
        for t in (0.5, 2.0, 5.0, 9.0):
            for how in ("abort", "estop"):
                out.append(dict(name=f"stop: {nm} {how} at {t}s", expect="safe", steps=[dict(st, stop_at=(t, how))],
                                checks=[], params={3025: 30}, results_unchecked=True))
    return out


def selftest_cases():
    """The bench must see a crash in each of these, or it proves nothing."""
    S = STOCK
    side = (135, S["cy"], STOCK_DEPTH)

    def add(name, start, call, **kw):
        d = dict(name=f"selftest: {name}", expect="crash", steps=[dict(start=start, call=call)], checks=[])
        d.update(kw)
        return d

    return [
        add("G1 into the stock's side", side, "G91 G1 X{L:20} F{L:500}"),
        add("G0 down onto the stock's top", (S["cx"], S["cy"], STOCK_ABOVE), "G91 G0 Z{L:-10}"),
        add("G38.2 at 9000 mm/min into the stock (overtravel)", side, "G91 G38.2 X{L:20} F{L:9000}"),
        add("probe body under a strap", (185, -90, STOCK_ABOVE), "G91 G38.3 X{L:30} F{L:500}", obstacles=1 << 6),
        add("end mill G38.2 into the stock", (135, S["cy"], -255.0 + 60.0), "G91 G38.2 X{L:20} F{L:300}", tool=1),
    ]


# ---- Fuzz ---------------------------------------------------------------------
# For each knob: values that may break a routine. "safe": the routine may
# refuse, but never crash or give a wrong answer.
FUZZ_PARAMS = {
    3015: [0, 1, 20, 300, 2000],  # slow feed (0 = no slow touch)
    3016: [20, 200, 1500, 3000],  # fast
    3017: [100, 1500, 3000, 6000],  # traverse
    3018: [0.5, 2, 8, 60],  # XY reach
    3019: [0.05, 1, 10, 30],  # XY clearance
    3020: [1, 6, 100],  # Z reach
    3021: [0.05, 0.5, 8, 30],  # Z clearance
    3022: [0.5, 3, 12],  # extra depth
    3023: [-5, 0, 0.5, 2, 15, 40],  # step off
    3024: [-10, 0, 5, 80],  # edge width
    3025: [-30, 0, 10, 26, 34, 60],  # diameter hint
    3026: [0, 20, 44, 80], 3027: [0, 20, 34, 80], 3028: [0, 6, 18, 40], 3029: [0, 6, 18, 40],
    3032: [-0.5, 0.2, 1.4],  # calibration offset (results then off by it)
}
FUZZ_PRE = [[], ["G91"], ["G93"], ["G18"], ["G20"], ["G92 X1"], ["G10 L2 P1 R1"], ["G49"], ["G43 H1"],
            ["G10 L2 P2 X3 Y-4 Z5", "G55"], ["G1 G91 F50"]]


def fuzz_cases(units, rnd, n):
    base = [b for b in base_cases()]
    out = []
    for i in range(n):
        b = rnd.choice(base)
        case = dict(b, name=f"fuzz {i}: {b['name']}", expect="safe",
                    params={n: conv(n, v, units) for n, v in b["params"].items()},
                    steps=[dict(st) for st in b["steps"]], checks=list(b["checks"]))
        what = []
        for _ in range(rnd.choice([1, 1, 2, 3])):
            k = rnd.choice(["param", "param", "param", "start", "pre", "tip", "length", "obstacle", "stop"])
            if k == "param":
                p = rnd.choice(list(FUZZ_PARAMS))
                v = rnd.choice(FUZZ_PARAMS[p])
                case["params"][p] = conv(p, v, units)
                what.append(f"#{p}={v}")
                if p == 3032:
                    case["results_unchecked"] = True
            elif k == "start" and "start" in case["steps"][0]:
                x, y, z = case["steps"][0]["start"]
                dx, dy, dz = rnd.uniform(-3, 3), rnd.uniform(-3, 3), rnd.uniform(-3, 3)
                case["steps"][0]["start"] = (x + dx, y + dy, z + dz)
                what.append(f"start {dx:+.2f},{dy:+.2f},{dz:+.2f}")
                # A start moved this far can find a different face; it must
                # still not crash, and its numbers are not checked.
                case["results_unchecked"] = True
            elif k == "pre":
                pre = rnd.choice(FUZZ_PRE)
                if units == "inch":
                    pre = ["G21" if x == "G20" else x for x in pre]
                case["steps"][0]["pre"] = pre
                what.append("pre " + " ".join(pre))
            elif k == "tip":
                d = rnd.choice([1.0, 2.0, 5.0, 8.0])
                case["tool_table"] = [f"G10 L1 P99 R{d / 2 * (1 if units == 'mm' else 1 / 25.4):.6f}"]
                what.append(f"table tip D{d} (real {PROBE_D_MM})")
                case["results_unchecked"] = True
            elif k == "length":
                le = rnd.choice([-5.0, -1.0, 1.0, 5.0])
                case["length_error"] = le
                what.append(f"probe {le:+} mm longer than its table length")
                case["results_unchecked"] = True
            elif k == "obstacle":
                m = 1 << rnd.randrange(6)
                case["obstacles"] = case.get("obstacles", 0) | m
                what.append(f"obstacles {m}")
                case["results_unchecked"] = True
            elif k == "stop":
                st = case["steps"][-1]
                st["stop_at"] = (rnd.uniform(0.2, 12.0), rnd.choice(["abort", "estop"]))
                what.append(f"{st['stop_at'][1]} at {st['stop_at'][0]:.1f}s")
        case["name"] += " | " + "; ".join(what)
        out.append(case)
    return out


def all_cases(units, rnd, fuzz):
    def cv(case):
        case = dict(case)
        case["params"] = {n: conv(n, v, units) for n, v in case.get("params", {}).items()}
        return case

    fixed = [cv(x) for x in selftest_cases() + base_cases() + refuse_cases() + obstacle_cases() + modal_cases() + stop_cases()]
    if units == "inch":
        # Inch: the unit check flips (G21 is the wrong one there).
        for x in fixed:
            for st in x["steps"]:
                st["pre"] = ["G21" if p == "G20" else p for p in st.get("pre", [])]
    fz = fuzz_cases(units, rnd, fuzz)
    return fixed + fz
