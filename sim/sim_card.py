#!/usr/bin/env python3
"""Stand-in for the 7i76E card in the LinuxCNC simulator (millgui/sim).

Exports the hm2_7i76e.0.* pins mymill.hal's logic uses, so that file runs
unchanged apart from motion: HLFB inputs report ready, the probe trips
inside a stock block and the tool setter trips over the G30 position.
For the simulator only; it never talks to hardware.
"""
import os
import sys
import time

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
    c.newpin(f"sim.{a}", hal.HAL_FLOAT, hal.HAL_IN)
# Hold an input on by hand: halcmd setp hm2_7i76e.0.sim.force-probe 1
c.newpin("sim.force-probe", hal.HAL_BIT, hal.HAL_IN)
c.newpin("sim.force-setter", hal.HAL_BIT, hal.HAL_IN)
c.ready()

# Machine coordinates (mm) of the pretend stock and setter, as heights of
# the tool tip. The tip sits below the spindle nose by the length the tool
# table gives the tool in the spindle, so G43 moves land where they would.
STOCK = dict(x=(150.0, 280.0), y=(-140.0, -40.0), top=-250.0)
# The setter top is where tool_touch_off.ngc expects the nose to trip with
# no tool (#3010 in probe_basic.var), so a touch-off measures the table length.
SETTER = dict(x=float(os.environ.get("SIM_SETTER_X", "0")), y=float(os.environ.get("SIM_SETTER_Y", "-15.26")),
              top=-float(os.environ.get("SIM_SETTER_DEPTH", "286.873159")), r=10.0)
TOOL_TABLE = os.environ.get("SIM_TOOL_TABLE", "tool.tbl")
_lengths = {"mtime": None, "z": {}}


def tool_length():
    """Length of the tool in the spindle, from the tool table (0 when none)."""
    try:
        m = os.path.getmtime(TOOL_TABLE)
        if m != _lengths["mtime"]:
            z = {}
            for line in open(TOOL_TABLE):
                words = line.split(";")[0].split()
                if words and words[0].startswith("T"):
                    for w in words[1:]:
                        if w.startswith("Z"):
                            z[int(words[0][1:])] = float(w[1:])
            _lengths.update(mtime=m, z=z)
        return _lengths["z"].get(int(hal.get_value("iocontrol.0.tool-number")), 0.0)
    except Exception:
        return 0.0


def setin(n, on):
    c[f"{P}input-{n:02d}"] = on
    c[f"{P}input-{n:02d}-not"] = not on


length, length_at = 0.0, 0.0
try:
    while True:
        if time.monotonic() - length_at > 0.05:
            length, length_at = tool_length(), time.monotonic()
        x, y = c["sim.x"], c["sim.y"]
        z = c["sim.z"] - length
        # HLFB ok on every drive (inputs 24-27 high = ok).
        for n in (24, 25, 26, 27):
            setin(n, True)
        # Home switches not pressed (inputs read through -not).
        for n in (28, 29, 30):
            setin(n, True)
        in_stock = STOCK["x"][0] <= x <= STOCK["x"][1] and STOCK["y"][0] <= y <= STOCK["y"][1] and z <= STOCK["top"]
        on_setter = (x - SETTER["x"]) ** 2 + (y - SETTER["y"]) ** 2 <= SETTER["r"] ** 2 and z <= SETTER["top"]
        # Probe and setter are normally closed: the -not pin is the trigger.
        setin(16, not (in_stock or c["sim.force-probe"]))
        setin(22, not (on_setter or c["sim.force-setter"]))
        setin(17, True)
        setin(18, True)
        time.sleep(0.001)
except KeyboardInterrupt:
    sys.exit(0)
