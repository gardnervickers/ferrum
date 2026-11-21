#!/usr/bin/env python3
"""The probe bench's world: workpieces, fixtures and a probe with a body.

A HAL user component (`loadusr -Wn world python3 world.py`) for the probe
bench's LinuxCNC simulator. It trips `motion.probe-input` when the probe's
ball or stylus touches anything, and it records a CRASH whenever the
machine hurts the probe:

  contact   the ball or stylus touches something, or presses further into
            it, during a move that is not a probing move (G0, G1, a jog:
            motion.motion-type is not 5). Backing away from a contact is
            fine; pushing into one is a crash.
  overtravel during a probing move the ball has pushed further into a
            surface than the probe can deflect (OVERTRAVEL, mm). A probe
            stops a little past the contact (deceleration plus the servo
            period); far past it, it breaks.
  body      the probe body or the spindle nose touches anything, in any
            move. They do not trip the probe input: on a real machine
            nothing stops them.
  tool      a tool that is not the probe touches anything (it never trips
            the probe input, as an end mill would not).

The workpieces are the mymill simulator's (`ferrum/sim/sim_card.py`, the
same machine coordinates, so expected results are the same numbers), plus
OBSTACLES: fixtures switched on per test case with the `obstacles` pin
(bit n = OBSTACLES[n]) to put a vise jaw or a clamp in a routine's path.

The probe (machine mm; the inch config scales positions in): a ball of the
tool table diameter less PRETRAVEL (so a calibrated #3032 reports true
edges), on a stylus of radius STYLUS_R up to STYLUS_LEN above the tip,
then a body of radius BODY_R up to the spindle nose (tool length above the
tip), then the spindle nose of radius NOSE_R. Every solid is a vertical
prism (nothing overhangs), so moving straight up never hits anything.

Every crash is appended as one JSON line to $BENCH_EVENTS. Pins:
  in  x y z (machine position), tool, motion-type, obstacles, phase,
      length-error (the probe is this much longer than its table length),
      force-probe, reset (clears crashed and max-pen)
  out probe (to motion.probe-input), contact, crashed, crashes,
      max-pen (deepest ball penetration since reset, mm)
For the simulator only.
"""
import json
import math
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "sim"))
import sim_card as sc  # noqa: E402  (geometry only; its HAL main is not run)

UNITS = os.environ.get("BENCH_UNITS", "mm")
TO_MM = 25.4 if UNITS == "inch" else 1.0
PRETRAVEL = float(os.environ.get("BENCH_PRETRAVEL_MM", "0.059025"))
OVERTRAVEL = float(os.environ.get("BENCH_OVERTRAVEL_MM", "1.0"))
PROBE_TOOL = int(os.environ.get("BENCH_PROBE_TOOL", "99"))
STYLUS_LEN = 30.0
STYLUS_R = 1.0
BODY_R = 10.0
NOSE_R = 35.0
EPS = 0.002  # mm: deeper than this counts as pressing in
STEP = 0.05  # mm: positions between samples are checked this finely
TOOL_TABLE = os.environ.get("BENCH_TOOL_TABLE", "tool.tbl")

rect, circle, minus, BOTTOM = sc.rect, sc.circle, sc.minus, sc.BOTTOM

SOLIDS = list(sc.SOLIDS)

# Fixtures for test cases: (name, footprint, bottom Z, top Z), machine mm.
OBSTACLES = [
    # 0: a vise jaw 3 mm off the STOCK block's right face (X 280), 2 mm
    # below its top: the right-hand corners' step-off lands on it.
    ("jaw right of stock", rect(291.5, -90.0, 17.0, 120.0), BOTTOM, -252.0),
    # 1: a clamp bolt beside the round boss (D30 at 325,-35), standing 10
    # above it, where the boss routines step out to (+X side).
    ("bolt beside boss", circle(348.0, -35.0, 8.0), BOTTOM, -240.0),
    # 2: a post inside the bore (D30 at 380,-90) near its +X wall: a
    # 3-point bore that leaves 0 deg out still works.
    ("post in bore", circle(391.0, -90.0, 5.0), -270.0, -255.0),
    # 3: a clamp on the tilted block's left face, in the path of the
    # edge-angle routine's second touch.
    ("clamp on tilted face", circle(40.0, -145.0, 10.0), BOTTOM, -245.0),
    # 4: a toe clamp in the front-left corner region of the STOCK block's
    # front face, where the auto-stock walk and the corner's step-off pass.
    ("clamp at stock front", rect(160.0, -146.0, 10.0, 8.0), BOTTOM, -240.0),
    # 5: a tall block just off the STOCK block's back left corner, where
    # the back-left corner routine steps off and plunges.
    ("block at stock back left", rect(153.0, -33.0, 8.0, 6.0), BOTTOM, -235.0),
    # 6: for the bench's self test only: a strap hanging in the air over
    # the STOCK block, high enough that only the probe body can reach it.
    ("strap above stock (self test)", rect(215.0, -90.0, 10.0, 60.0), -200.0, -190.0),
]


def prism_dist(solid, x, y, z):
    """3-D distance from a point to a prism solid (0 inside)."""
    _, fp, z0, z1 = solid
    dxy = sc.distance(fp, x, y)
    dz = max(z0 - z, 0.0, z - z1)
    return math.hypot(dxy, dz)


def column_hits(solid, x, y, za, zb, r):
    """Whether a vertical cylinder (axis x, y; Z za..zb; radius r) meets it."""
    _, fp, z0, z1 = solid
    if zb < z0 or za > z1:
        return False
    return sc.distance(fp, x, y) <= r


class Probe:
    def __init__(self):
        self.mtime = None
        self.z, self.d = {}, {}

    def table(self, tool):
        try:
            m = os.path.getmtime(TOOL_TABLE)
            if m != self.mtime:
                z, d = {}, {}
                for line in open(TOOL_TABLE):
                    words = line.split(";")[0].split()
                    if words and words[0].startswith("T"):
                        n = int(words[0][1:])
                        for w in words[1:]:
                            if w.startswith("Z"):
                                z[n] = float(w[1:]) * TO_MM
                            elif w.startswith("D"):
                                d[n] = float(w[1:]) * TO_MM
                self.mtime, self.z, self.d = m, z, d
        except (OSError, ValueError):
            pass
        return self.z.get(tool, 0.0), self.d.get(tool, 0.0)


def check(solids, x, y, mz, length, dia, is_probe):
    """At one spindle position: (ball/stylus penetration, what it touches,
    what the body/nose/tool hits). mm, machine coordinates."""
    tip = mz - length
    pen, touched, hit = -1e9, None, None
    if is_probe:
        r = max(dia / 2.0 - PRETRAVEL, 0.0)
        cz = tip + r
        for s in solids:
            p = r - prism_dist(s, x, y, cz)
            # The stylus is thinner than the ball: it only matters when
            # something reaches over the ball's top.
            _, fp, z0, z1 = s
            if z1 > cz and z0 < tip + STYLUS_LEN:
                p = max(p, STYLUS_R - sc.distance(fp, x, y))
            if p > pen:
                pen, touched = p, s[0]
        for s in solids:
            if column_hits(s, x, y, tip + STYLUS_LEN, mz, BODY_R):
                hit = f"probe body on {s[0]}"
                break
    else:
        r = dia / 2.0
        for s in solids:
            if column_hits(s, x, y, tip, mz, r):
                hit = f"tool T on {s[0]}"
                break
    if hit is None and length > 0:
        for s in solids:
            if column_hits(s, x, y, mz, mz + 1000.0, NOSE_R):
                hit = f"spindle nose on {s[0]}"
                break
    return pen, touched, hit


def main():
    import hal

    c = hal.component("world")
    for p in ("x", "y", "z", "length-error"):
        c.newpin(p, hal.HAL_FLOAT, hal.HAL_IN)
    for p in ("tool", "motion-type", "phase"):
        c.newpin(p, hal.HAL_S32, hal.HAL_IN)
    c.newpin("obstacles", hal.HAL_U32, hal.HAL_IN)
    c.newpin("force-probe", hal.HAL_BIT, hal.HAL_IN)
    c.newpin("reset", hal.HAL_BIT, hal.HAL_IN)
    c.newpin("probe", hal.HAL_BIT, hal.HAL_OUT)
    c.newpin("contact", hal.HAL_BIT, hal.HAL_OUT)
    c.newpin("crashed", hal.HAL_BIT, hal.HAL_OUT)
    c.newpin("crashes", hal.HAL_S32, hal.HAL_OUT)
    c.newpin("max-pen", hal.HAL_FLOAT, hal.HAL_OUT)
    c.ready()

    events = os.environ.get("BENCH_EVENTS", "events.jsonl")
    probe = Probe()
    prev = None  # (x, y, z) mm
    prev_pen = -1e9
    in_episode = {}  # crash kind -> still in that episode (log once)
    mask_now, solids = None, SOLIDS
    tool_at, tool, length, dia = 0.0, 0, 0.0, 0.0
    max_pen = 0.0
    probe_seen = -1.0
    last_loop = time.monotonic()
    late = False  # this contact was seen late: the loop stalled

    def log(kind, what, pos, mtype, pen):
        c["crashed"] = True
        c["crashes"] = c["crashes"] + 1
        rec = {"t": time.time(), "kind": kind, "what": what, "pos": [round(v, 4) for v in pos],
               "motion_type": mtype, "pen": round(pen, 4), "phase": c["phase"], "tool": tool}
        with open(events, "a") as f:
            f.write(json.dumps(rec) + "\n")

    try:
        while True:
            if c["reset"]:
                c["crashed"] = False
                max_pen = 0.0
                in_episode.clear()
            mask = c["obstacles"]
            if mask != mask_now:
                solids = SOLIDS + [o for i, o in enumerate(OBSTACLES) if mask & (1 << i)]
                mask_now = mask
            now = time.monotonic()
            gap = now - last_loop
            last_loop = now
            if now - tool_at > 0.02:
                tool = c["tool"]
                length, dia = probe.table(tool)
                tool_at = now
            is_probe = tool == PROBE_TOOL and tool > 0
            phys_len = length + (c["length-error"] * TO_MM if is_probe else 0.0)
            mtype = c["motion-type"]
            cur = (c["x"] * TO_MM, c["y"] * TO_MM, c["z"] * TO_MM)
            mtype2 = c["motion-type"]
            # A probing move that has tripped is aborted: motion may report
            # no move type (0) while it decelerates. That stop is still
            # the probing move's, so it counts as probing for a moment.
            if mtype == 5 or mtype2 == 5:
                probe_seen = now
            probing = now - probe_seen < 0.1
            if prev is None:
                prev = cur
            dist = math.dist(prev, cur)
            n = max(1, int(dist / STEP) + 1) if dist > 0 else 1
            pen_last = prev_pen
            touching = False
            for i in range(1, n + 1):
                f = i / n
                p = tuple(a + (b - a) * f for a, b in zip(prev, cur))
                pen, touched, hit = check(solids, p[0], p[1], p[2], phys_len, dia, is_probe)
                if hit:
                    kind = "tool" if hit.startswith("tool") else "body"
                    if not in_episode.get(kind):
                        log(kind, hit, p, mtype, 0.0)
                        in_episode[kind] = True
                else:
                    in_episode["body"] = in_episode["tool"] = False
                if pen > 0:
                    if pen_last <= 0:
                        # A contact begins: if this loop came late (the
                        # simulator, not the machine, was slow) the probe
                        # input trips late and the overtravel is ours.
                        late = gap > 0.01
                    touching = True
                    if probing:
                        max_pen = max(max_pen, pen)
                        if pen > OVERTRAVEL and not in_episode.get("overtravel"):
                            if late:
                                log("late", f"pushed {pen:.3f} mm into {touched} after a {gap * 1000:.0f} ms stall", p, mtype, pen)
                            else:
                                log("overtravel", f"pushed {pen:.3f} mm into {touched}", p, mtype, pen)
                            in_episode["overtravel"] = True
                    elif dist > 0 and pen > pen_last + EPS and not in_episode.get("contact"):
                        log("contact", f"non-probing move into {touched}", p, mtype, pen)
                        in_episode["contact"] = True
                else:
                    in_episode["contact"] = in_episode["overtravel"] = False
                pen_last = pen
            prev, prev_pen = cur, pen_last
            c["contact"] = touching
            c["probe"] = bool(is_probe and touching) or c["force-probe"]
            c["max-pen"] = max_pen / TO_MM
            time.sleep(0.0003)
    except KeyboardInterrupt:
        sys.exit(0)


if __name__ == "__main__":
    if "--describe" in sys.argv[1:]:
        for s in SOLIDS:
            print("solid   ", s[0], s[1], s[2], s[3])
        for i, s in enumerate(OBSTACLES):
            print(f"obstacle {i}", s[0], s[1], s[2], s[3])
    else:
        main()
