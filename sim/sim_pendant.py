#!/usr/bin/env python3
"""Stand-in for the WHB04B pendant driver (xhc-whb04b-6) in the simulator.

Exports the whb.* pins ferrum/hal/pendant.hal and the sim's pendant nets
use, and acts like the driver's START/PAUSE handling: if task is in AUTO it
raises button.start-pause; otherwise it asks halui for AUTO first
(halui.mode.auto, 10 ms) and waits up to 300 ms for it, as the real driver
does. Press it by hand:

    halcmd setp whb.sim.press-start 1; halcmd setp whb.sim.press-start 0
    halcmd setp whb.sim.wheel 1        # asks for MANUAL, as a wheel turn does
For the simulator only.
"""
import time

import hal

c = hal.component("whb")
for p in ["pendant.is-connected", "pendant.is-sleeping", "button.start-pause"] + [
    f"halui.mode.{m}" for m in ("auto", "manual", "mdi", "joint", "teleop")
] + [f"halui.program.{m}" for m in ("run", "pause", "stop")]:
    c.newpin(p, hal.HAL_BIT, hal.HAL_OUT)
for m in ("auto", "manual", "mdi", "joint", "teleop"):
    c.newpin(f"halui.mode.is-{m}", hal.HAL_BIT, hal.HAL_IN)
c.newpin("sim.press-start", hal.HAL_BIT, hal.HAL_IN)
c.newpin("sim.wheel", hal.HAL_BIT, hal.HAL_IN)
c.newpin("sim.connected", hal.HAL_BIT, hal.HAL_IN)
c.ready()
c["sim.connected"] = 1


def request(mode):
    """The driver's requestMode: hold 10 ms, wait up to 300 ms for the mode."""
    if c[f"halui.mode.is-{mode}"]:
        return True
    c[f"halui.mode.{mode}"] = 1
    time.sleep(0.01)
    ok = False
    for _ in range(60):
        if c[f"halui.mode.is-{mode}"]:
            ok = True
            break
        time.sleep(0.005)
    c[f"halui.mode.{mode}"] = 0
    time.sleep(0.01)
    return ok


last_start = last_wheel = 0
try:
    while True:
        c["pendant.is-connected"] = c["sim.connected"]
        start, wheel = c["sim.press-start"], c["sim.wheel"]
        if start and not last_start:
            if request("auto"):
                c["button.start-pause"] = 1
        if not start and last_start:
            c["button.start-pause"] = 0
        if wheel and not last_wheel:
            request("manual")
        last_start, last_wheel = start, wheel
        time.sleep(0.01)
except KeyboardInterrupt:
    pass
