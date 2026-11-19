#!/usr/bin/env python3
"""Stand-in for the 7i76E card in the LinuxCNC simulator (ferrum/sim).

Exports the hm2_7i76e.0.* pins mymill.hal's logic uses, so that file runs
unchanged apart from motion: HLFB inputs report ready, the probe trips when
the probe tip touches the simulated workpieces below, the tool setter trips
under the tool or spindle nose at the G30 position, and the X/Y/Z home
switches close a little way from where each joint started, in the direction
mymill.ini's HOME_SEARCH_VEL searches, so homing runs the real search and
latch moves.
For the simulator only; it never talks to hardware.

The workpieces (machine coordinates, mm; `python3 sim_card.py --describe`
prints them). Every solid is a vertical prism, a 2-D footprint swept from a
bottom Z to a top Z; pockets are holes in a footprint plus a floor prism.

  STOCK block   X150..280 Y-140..-40, top Z-250 (plain outside: edges,
                corners, top face, auto stock, two faces), with two slots
                10 deep: X valley X173..187 Y-120..-60 (floor -260) and
                Y valley X225..270 Y-72..-58 (floor -260).
  PLATE         X295..425 Y-175..-5, top Z-260, carrying
                round boss  D30 at (325,-35), top -250
                rect boss   40 x 24 at (380,-35), top -250
                rect pocket 40 x 30 at (325,-90), floor -270
                bore        D30 at (380,-90), floor -270
                X ridge     12 wide (X) x 40 at (325,-145), top -250
                Y ridge     40 x 12 wide (Y) at (380,-145), top -250
  TILTED block  80 x 80 centred (85,-125), turned +2.000 deg (CCW seen from
                above), top -255: edge angles.
  RING gauge    bore D20.000, outside D36, centred (45,-60), top -265:
                probe tip calibration (CAL).
They sit so that each routine's longest probing move (#3018 = 25 plus the
size hint) stays inside mymill.ini's travel: LinuxCNC refuses a G38 whose
end is past a soft limit.

The probe tip is a ball. The tool tip (the bottom of the ball) sits below
the spindle nose by the tool's length; the ball's radius is half the tool
table diameter (D) of the tool in the spindle, less PRETRAVEL: a real probe
trips a little after first touch, which its calibration (#3032 in
probe_basic.var) takes out. So the input trips when a ball of radius
D/2 - PRETRAVEL whose lowest point is the tool tip touches a solid: on a
flat top exactly at the top, on a wall at D/2 - PRETRAVEL from it. With
#3032 = PRETRAVEL a macro that adds D/2 - #3032 reports the true edge.

Tool lengths come from the tool table, except tools listed in
SIM_TOOL_LENGTHS ("7=111.0,4=98.794101" by default), whose physical length is fixed, so that
a touch-off has something to measure and a check can find a "broken" tool.

The tool setter is a disc of radius SETTER["r"] at the G30 spot whose top is
where tool_touch_off.ngc expects the nose to trip with no tool (#3010 in
probe_basic.var). It trips when the bottom face of the tool (a disc of the
tool's diameter; the spindle nose's with no tool) overlaps it and the tip
is at or below its top.
"""
import math
import os
import sys
import time

# ---- Workpieces --------------------------------------------------------------

# The plain block of stock the earlier scenarios use: its middle (215, -90)
# stays flat at -250.
STOCK = dict(x=(150.0, 280.0), y=(-140.0, -40.0), top=-250.0)
BOTTOM = -1000.0  # every solid stands on something far below reach
PRETRAVEL = float(os.environ.get("SIM_PROBE_PRETRAVEL", "0.059025"))


def rect(cx, cy, w, h, deg=0.0):
    """A w x h rectangle centred (cx, cy), turned deg (CCW from above)."""
    a = math.radians(deg)
    return ("rect", cx, cy, w / 2.0, h / 2.0, math.cos(a), math.sin(a))


def circle(cx, cy, d):
    return ("circle", cx, cy, d / 2.0)


def minus(outer, *holes):
    """outer with holes (inside it, apart from each other and its edge)."""
    return ("minus", outer, holes)


def signed(fp, x, y):
    """Signed 2-D distance from (x, y) to a rect or circle (negative inside)."""
    if fp[0] == "circle":
        return math.hypot(x - fp[1], y - fp[2]) - fp[3]
    _, cx, cy, hw, hh, c, s = fp
    lx = (x - cx) * c + (y - cy) * s
    ly = -(x - cx) * s + (y - cy) * c
    dx, dy = abs(lx) - hw, abs(ly) - hh
    return math.hypot(max(dx, 0.0), max(dy, 0.0)) + min(max(dx, dy), 0.0)


def distance(fp, x, y):
    """2-D distance from (x, y) to a footprint (0 inside it)."""
    if fp[0] == "minus":
        d = signed(fp[1], x, y)
        if d > 0:
            return d
        for h in fp[2]:
            dh = signed(h, x, y)
            if dh < 0:
                return -dh
        return 0.0
    return max(signed(fp, x, y), 0.0)


def touches(solid, x, y, z, r):
    """Whether a ball of radius r centred (x, y, z) meets a prism solid.

    The widest slice of the ball inside the prism's Z range is at the height
    nearest the centre; the ball meets the prism iff that slice meets the
    footprint."""
    _, fp, z0, z1 = solid
    h = min(max(z, z0), z1)
    dz = h - z
    if abs(dz) > r:
        return False
    return distance(fp, x, y) <= math.sqrt(r * r - dz * dz)


TILT_DEG = 2.0
X_VALLEY = rect(180.0, -90.0, 14.0, 60.0)
Y_VALLEY = rect(247.5, -65.0, 45.0, 14.0)
POCKET = rect(325.0, -90.0, 40.0, 30.0)
BORE = circle(380.0, -90.0, 30.0)
RING_BORE = circle(45.0, -60.0, 20.0)

# (name, footprint, bottom Z, top Z)
SOLIDS = [
    ("stock", minus(rect(215.0, -90.0, 130.0, 100.0), X_VALLEY, Y_VALLEY), BOTTOM, STOCK["top"]),
    ("x valley floor", X_VALLEY, BOTTOM, -260.0),
    ("y valley floor", Y_VALLEY, BOTTOM, -260.0),
    ("plate", minus(rect(360.0, -90.0, 130.0, 170.0), POCKET, BORE), BOTTOM, -260.0),
    ("rect pocket floor", POCKET, BOTTOM, -270.0),
    ("bore floor", BORE, BOTTOM, -270.0),
    ("round boss", circle(325.0, -35.0, 30.0), -260.0, -250.0),
    ("rect boss", rect(380.0, -35.0, 40.0, 24.0), -260.0, -250.0),
    ("x ridge", rect(325.0, -145.0, 12.0, 40.0), -260.0, -250.0),
    ("y ridge", rect(380.0, -145.0, 40.0, 12.0), -260.0, -250.0),
    ("tilted block", rect(85.0, -125.0, 80.0, 80.0, TILT_DEG), BOTTOM, -255.0),
    ("ring gauge", minus(circle(45.0, -60.0, 36.0), RING_BORE), BOTTOM, -265.0),
]


def probe_contact(x, y, tip, diameter):
    """Whether the probe ball (tip = its lowest point) touches a workpiece."""
    r = max(diameter / 2.0 - PRETRAVEL, 0.0)
    return any(touches(s, x, y, tip + r, r) for s in SOLIDS)


def describe():
    for name, fp, z0, z1 in SOLIDS:
        print(f"{name:18} {fp}  Z {z0}..{z1}")
    print(f"probe pretravel {PRETRAVEL}")


# ---- Tool setter and tools ---------------------------------------------------

# The setter top is where tool_touch_off.ngc expects the nose to trip with
# no tool (#3010 in probe_basic.var), so a touch-off measures the length.
SETTER = dict(x=float(os.environ.get("SIM_SETTER_X", "0")), y=float(os.environ.get("SIM_SETTER_Y", "-15.26")),
              top=-float(os.environ.get("SIM_SETTER_DEPTH", "286.873159")), r=10.0)
TOOL_TABLE = os.environ.get("SIM_TOOL_TABLE", "tool.tbl")
# Tools whose physical length is not the tool table's (number=length).
PHYSICAL = {int(k): float(v) for k, v in (p.split("=") for p in os.environ.get("SIM_TOOL_LENGTHS", "7=111.0,4=98.794101").split(",") if p)}


def nose_diameter():
    """[TOOL_SETTER]SPINDLE_NOSE_DIAMETER from the config's mymill.ini."""
    try:
        section = ""
        for line in open("mymill.ini"):
            s = line.strip()
            if s.startswith("["):
                section = s
            elif section == "[TOOL_SETTER]" and s.split("=")[0].strip() == "SPINDLE_NOSE_DIAMETER":
                return float(s.split("=", 1)[1].split("#")[0])
    except (OSError, ValueError):
        pass
    return 40.0


_table = {"mtime": None, "z": {}, "d": {}}


def tool_data(tool):
    """(length, diameter) of a tool: the tool table's, or the physical length."""
    try:
        m = os.path.getmtime(TOOL_TABLE)
        if m != _table["mtime"]:
            z, d = {}, {}
            for line in open(TOOL_TABLE):
                words = line.split(";")[0].split()
                if words and words[0].startswith("T"):
                    n = int(words[0][1:])
                    for w in words[1:]:
                        if w.startswith("Z"):
                            z[n] = float(w[1:])
                        elif w.startswith("D"):
                            d[n] = float(w[1:])
            _table.update(mtime=m, z=z, d=d)
    except (OSError, ValueError):
        pass
    if tool <= 0:
        return 0.0, NOSE_D
    return PHYSICAL.get(tool, _table["z"].get(tool, 0.0)), _table["d"].get(tool, 0.0)


NOSE_D = nose_diameter()

# Home switches: input, joint, search direction (sign of HOME_SEARCH_VEL in
# mymill.ini) and how far from the start position they close (mm).
HOME_SWITCHES = [(28, "x", -1), (29, "y", 1), (30, "z", 1)]
HOME_TRAVEL = float(os.environ.get("SIM_HOME_TRAVEL", "40"))


def main():
    import hal

    P = "7i76.0.0."
    c = hal.component("sim_card", "hm2_7i76e.0")
    INPUTS = [16, 17, 18, 22, 24, 25, 26, 27, 28, 29, 30]
    for n in INPUTS:
        c.newpin(f"{P}input-{n:02d}", hal.HAL_BIT, hal.HAL_OUT)
        c.newpin(f"{P}input-{n:02d}-not", hal.HAL_BIT, hal.HAL_OUT)
    for n in [0, 1, 8, 9, 10, 11]:
        c.newpin(f"{P}output-{n:02d}", hal.HAL_BIT, hal.HAL_IN)
    for name, t in [("spinmode", hal.HAL_U32), ("spinout", hal.HAL_FLOAT), ("spinena", hal.HAL_BIT),
                    ("spindir", hal.HAL_BIT), ("spinout-minlim", hal.HAL_FLOAT), ("spinout-maxlim", hal.HAL_FLOAT),
                    ("spinout-scalemax", hal.HAL_FLOAT)]:
        c.newpin(P + name, t, hal.HAL_IN)
    c.newpin(P + "output_source", hal.HAL_U32, hal.HAL_IN)
    c.newpin(P + "output_sink", hal.HAL_U32, hal.HAL_IN)
    for a in "xyz":
        c.newpin(f"sim.{a}", hal.HAL_FLOAT, hal.HAL_IN)  # motor position (continuous through homing)
    # Hold an input on by hand: halcmd setp hm2_7i76e.0.sim.force-probe 1
    c.newpin("sim.force-probe", hal.HAL_BIT, hal.HAL_IN)
    c.newpin("sim.force-setter", hal.HAL_BIT, hal.HAL_IN)
    c.ready()

    def machine(j):
        """Joint (machine) position; the motor position shifts when homed."""
        try:
            return float(hal.get_value(f"joint.{j}.pos-fb"))
        except Exception:
            return 0.0

    def setin(n, on):
        c[f"{P}input-{n:02d}"] = on
        c[f"{P}input-{n:02d}-not"] = not on

    length, diameter, tool_at = 0.0, 0.0, 0.0
    try:
        while True:
            if time.monotonic() - tool_at > 0.05:
                try:
                    tool = int(hal.get_value("iocontrol.0.tool-number"))
                except Exception:
                    tool = 0
                length, diameter = tool_data(tool)
                tool_at = time.monotonic()
            x, y = machine(0), machine(1)
            z = machine(2) - length
            # HLFB ok on every drive (inputs 24-27 high = ok).
            for n in (24, 25, 26, 27):
                setin(n, True)
            # Home switches (normally closed, read through -not): pressed once the
            # motor has gone HOME_TRAVEL past its start in the search direction.
            for n, a, d in HOME_SWITCHES:
                setin(n, not (c[f"sim.{a}"] * d >= HOME_TRAVEL))
            in_stock = probe_contact(x, y, z, diameter)
            on_setter = (math.hypot(x - SETTER["x"], y - SETTER["y"]) <= SETTER["r"] + diameter / 2.0
                         and z <= SETTER["top"])
            # Probe and setter are normally closed: the -not pin is the trigger.
            setin(16, not (in_stock or c["sim.force-probe"]))
            setin(22, not (on_setter or c["sim.force-setter"]))
            setin(17, True)
            setin(18, True)
            time.sleep(0.0005)
    except KeyboardInterrupt:
        sys.exit(0)


if __name__ == "__main__":
    if "--describe" in sys.argv[1:]:
        describe()
    else:
        main()
