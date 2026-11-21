#!/usr/bin/env python3
"""The probe bench driver: LinuxCNC's display program in the bench config.

Runs every test case of cases.py (or the ones BENCH_ONLY names) against the
simulated world (world.py) through LinuxCNC's own Python interface, the way
Ferrum sends them: probe values as #30xx, a G54 that is not zero, the
probe loaded with G43, the probe placed at its start, then one
`o<...> call`. After each case it checks:

  - no crash (world.py's events: a non-probing move into anything, the probe
    body or spindle nose hitting anything, a probing move far past contact);
  - a case that must run ran without an error and its results (work offset
    written, reported sizes) are the true geometry;
  - a case that must be refused failed with that message and did not move;
  - a case that may fail either way (fuzz) did not crash, and if it claimed
    success, its results are right (a wrong result is a later crash);
  - after a successful run the modal state is what it was before (G90/G91,
    G20/G21, feed mode, plane, TLO, WCS, feed) and the probe is not touching;
  - after an aborted run, how the modal state was left (reported: Ferrum
    restores modes after every call, the bench does not).

Writes $BENCH_OUT/results.json and report.md. For the simulator only.
"""
import json
import math
import os
import random
import re
import sys
import time
import traceback

import hal
import linuxcnc

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import cases as C  # noqa: E402

OUT = os.environ.get("BENCH_OUT", os.getcwd())
UNITS = os.environ.get("BENCH_UNITS", "mm")
K = 1.0 if UNITS == "mm" else 1 / 25.4  # mm -> machine units
UNIT = "G21" if UNITS == "mm" else "G20"
EVENTS = os.environ.get("BENCH_EVENTS", "events.jsonl")

s = linuxcnc.stat()
c = linuxcnc.command()
e = linuxcnc.error_channel()
messages = []  # (kind, text) since the last clear


def log(*a):
    print("[bench]", *a, flush=True)


def drain():
    while True:
        m = e.poll()
        if not m:
            return
        kind, text = m
        kind = "error" if kind in (linuxcnc.NML_ERROR, linuxcnc.OPERATOR_ERROR) else "text"
        messages.append((kind, text))


def errors():
    return [t for k, t in messages if k == "error"]


_comp = None


def setp(pin, v):
    global _comp
    if _comp is None:  # hal.set_p needs a component of our own
        _comp = hal.component("bench")
        _comp.ready()
    hal.set_p(pin, str(v))


def wait_idle(timeout=600.0, stop_at=None):
    """Until the interpreter is idle, the queue empty and motion still.
    stop_at = (seconds, action): abort or E-stop that far in."""
    t0 = time.time()
    n = 0
    stopped = None
    while time.time() - t0 < timeout:
        s.poll()
        drain()
        if stop_at and stopped is None and time.time() - t0 >= stop_at[0]:
            stopped = stop_at[1]
            if stopped == "estop":
                c.state(linuxcnc.STATE_ESTOP)
            else:
                c.abort()
        idle = s.interp_state == linuxcnc.INTERP_IDLE and s.queue == 0 and s.inpos
        n = n + 1 if idle else 0
        if n >= 5 and time.time() - t0 > 0.08:
            return True, stopped
        time.sleep(0.01)
    return False, stopped


def ensure_ready():
    s.poll()
    if s.task_state == linuxcnc.STATE_ESTOP:
        c.state(linuxcnc.STATE_ESTOP_RESET)
        c.wait_complete()
    s.poll()
    if s.task_state != linuxcnc.STATE_ON:
        c.state(linuxcnc.STATE_ON)
        c.wait_complete()
    s.poll()
    if not all(s.homed[:3]):
        c.mode(linuxcnc.MODE_MANUAL)
        c.wait_complete()
        c.teleop_enable(0)
        c.wait_complete()
        c.home(-1)
        t0 = time.time()
        while time.time() - t0 < 30:
            s.poll()
            if all(s.homed[:3]):
                break
            time.sleep(0.05)
    s.poll()
    if s.task_mode != linuxcnc.MODE_MDI:
        c.mode(linuxcnc.MODE_MDI)
        c.wait_complete()


def mdi(line, timeout=600.0, stop_at=None):
    ensure_ready()
    c.mdi(line)
    c.wait_complete(5)
    return wait_idle(timeout, stop_at)


def snap():
    s.poll()
    return {
        "pos": list(s.actual_position[:3]),
        "g5x": list(s.g5x_offset[:3]),
        "g5x_index": s.g5x_index,
        "rot": s.rotation_xy,
        "g92": list(s.g92_offset[:3]),
        "tlo": s.tool_offset[2],
        "tool": s.tool_in_spindle,
        "gcodes": list(s.gcodes),
        "mcodes": list(s.mcodes),
        "feed": s.settings[1],
        "units": s.program_units,
        "contact": bool(hal.get_value("world.contact")),
    }


# Active G-code groups as linuxcnc.stat().gcodes reports them (x10): the
# ones a macro must leave as it found them (all but the sequence number,
# the motion mode and the non-modal group).
MODAL_IDX = list(range(3, 16))  # [16] is G92.3 (offsets on), checked elsewhere


def modal_diff(a, b):
    out = []
    for i in MODAL_IDX:
        if i < len(a["gcodes"]) and a["gcodes"][i] != b["gcodes"][i]:
            out.append(f"G{a['gcodes'][i] / 10:g} -> G{b['gcodes'][i] / 10:g}")
    # In G93 every feed move carries its own F, so the F left over does not
    # matter (and LinuxCNC's M73 does not restore it there).
    # M73 gives F back rounded (78.7402 -> 78.7): a 0.1 % slip is not a change.
    if abs(a["feed"] - b["feed"]) > 1e-3 * max(abs(a["feed"]), 1e-6) + 1e-6 and a["gcodes"][7] != 930:
        out.append(f"F{a['feed']:g} -> F{b['feed']:g}")
    return out


def events_since(offset):
    try:
        with open(EVENTS) as f:
            f.seek(offset)
            data = f.read()
            return [json.loads(l) for l in data.splitlines() if l.strip()], f.tell()
    except FileNotFoundError:
        return [], offset


def events_end():
    try:
        return os.path.getsize(EVENTS)
    except FileNotFoundError:
        return 0


RESULT_RE = re.compile(r"BENCH (.*)")


def read_results():
    """#1001..#1012 and #3032 through a DEBUG line."""
    names = [1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1009, 1010, 1011, 1012, 3032]
    before = len(messages)
    mdi("(DEBUG,BENCH " + " ".join(f"{n}=#{n}" for n in names) + ")")
    out = {}
    for _, t in messages[before:]:
        m = RESULT_RE.search(t)
        if m:
            for kv in m.group(1).split():
                k, v = kv.split("=")
                try:
                    out[int(k)] = float(v)
                except ValueError:
                    pass
    return out


WIDGET_RE = re.compile(r'getWidget\{"([a-z_0-9]+)"\}\.setValue\{([-0-9.eE+]+)\}')


def widgets(msgs):
    out = {}
    for _, t in msgs:
        for name, v in WIDGET_RE.findall(t):
            out[name] = float(v)
    return out


def place(start, travel_mz):
    """Probe to a start (machine mm): straight up, across, down."""
    x, y, z = start
    mdi(f"{UNIT} G90 G53 G0 Z{travel_mz * K:.6f}")
    mdi(f"G90 G53 G0 X{x * K:.6f} Y{y * K:.6f}")
    mdi(f"G90 G53 G1 Z{z * K:.6f} F{2000 * K:.4f}")


def setup(case):
    """Phase 0: machine state for a case."""
    setp("world.phase", 0)
    setp("world.obstacles", case.get("obstacles", 0))
    setp("world.length-error", case.get("length_error", 0.0) * K)
    setp("world.reset", 1)
    time.sleep(0.01)
    setp("world.reset", 0)
    ensure_ready()
    c.feedrate(1.0)
    mdi("G40")  # first: with cutter comp on, G53 and most else fail
    unit = UNIT
    mdi(f"{unit} G90 G53 G0 Z{C.TRAVEL_Z * K:.6f}")
    mdi(f"{unit} G90 G94 G17 G40 G80 G91.1 G54 M48")
    mdi("G92.1")
    mdi("G10 L2 P2 X0 Y0 Z0 R0")
    # The probe's tool table entry back to its real diameter (cases change it).
    mdi(f"G10 L1 P99 Z{C.PROBE_L * K:.6f} R{C.PROBE_D / 2 * K:.6f}")
    tool = case.get("tool", 99)
    s.poll()
    if s.tool_in_spindle != tool:
        mdi(f"M6 T{tool}")
    mdi(f"G43 H{case.get('tlo_tool', tool)}")
    params = dict(C.params(UNITS))
    params.update({n: v for n, v in case.get("params", {}).items()})
    line = ""
    for n, v in sorted(params.items()):
        line += f"#{n}={v:.6f} "
        if len(line) > 180:
            mdi(line)
            line = ""
    if line:
        mdi(line)
    wx, wy, wz = case.get("wcs0", C.WCS0)
    mdi(f"G10 L2 P1 X{wx * K:.6f} Y{wy * K:.6f} Z{wz * K:.6f} R0")
    for line in case.get("tool_table", []):
        mdi(line)


def check_value(name, got, want, tol):
    if got is None:
        return f"{name}: no value (want {want:.4f})"
    if abs(got - want) > tol:
        return f"{name}: {got:.4f}, want {want:.4f} +- {tol:g}"
    return None


def run_case(case):
    t0 = time.time()
    res = {"name": case["name"], "expect": case["expect"], "notes": [], "fail": []}
    try:
        setup(case)
        steps = case["steps"]
        ev_off = events_end()
        crash = []
        last_msgs = []
        stopped = None
        failed_step = None
        for i, st in enumerate(steps):
            setp("world.phase", 0)
            if "start" in st:
                place(st["start"], st.get("travel", C.TRAVEL_Z))
            for line in st.get("pre", []):
                mdi(line)
            if st.get("override"):
                c.feedrate(st["override"])
            ev, ev_off = events_since(ev_off)
            if ev:
                res["fail"].append("BENCH SETUP CRASHED: " + "; ".join(x["what"] for x in ev))
                break
            before = snap()
            setp("world.phase", 1)
            messages.clear()
            call = re.sub(r"\{L:([-0-9.]+)\}", lambda m: f"{float(m.group(1)) * K:.6f}", st["call"])
            done, stopped = mdi(call, timeout=st.get("timeout", 600.0), stop_at=st.get("stop_at"))
            last_msgs = list(messages)
            after = snap()
            ev, ev_off = events_since(ev_off)
            crash += ev
            if not done:
                res["fail"].append(f"step {i + 1} did not finish")
            errs = [t for k, t in last_msgs if k == "error"]
            res.setdefault("errors", []).extend(errs)
            res.setdefault("modal", []).append(modal_diff(before, after))
            res["moved"] = math.dist(before["pos"], after["pos"]) > 1e-6 * K
            if errs or stopped:
                failed_step = i
                break
        setp("world.phase", 2)
        after = snap()
        vals = read_results() if failed_step is None else {}
        w = widgets(last_msgs)
        res["max_pen"] = float(hal.get_value("world.max-pen"))
        ok = failed_step is None and not res["fail"]
        res["ran"] = ok
        exp = case["expect"]
        if exp == "crash":
            res["crashes_seen"] = [f"{x['kind']}: {x['what']}" for x in crash]
            if not crash:
                res["fail"].append("the bench saw no crash")
            res["notes"] += [f"saw {x}" for x in res["crashes_seen"]]
            crash = []
            ok = False
        for x in [x for x in crash if x["kind"] == "late"]:
            res["notes"].append(f"bench stalled: {x['what']} (not counted)")
        crash = [x for x in crash if x["kind"] != "late"]
        for x in crash:
            res["fail"].append(f"CRASH ({x['kind']}): {x['what']} at {x['pos']} (motion type {x['motion_type']})")
        if exp == "ok" and not ok:
            res["fail"].append("should have run, failed: " + "; ".join(res.get("errors", []) or [str(stopped)]))
        if exp == "refuse":
            if ok:
                res["fail"].append("should have been refused, ran")
            elif case.get("refuse_text") and not any(case["refuse_text"].lower() in t.lower() for t in res.get("errors", [])):
                res["fail"].append(f"refused with {res.get('errors')}, want '{case['refuse_text']}'")
            if res.get("moved") and not case.get("refuse_may_move"):
                res["fail"].append("refused only after moving")
        if ok and not case.get("results_unchecked"):
            tol = case.get("tol", C.GEOM_TOL) * K
            for check in case.get("checks", []):
                kind, key, want = check[0], check[1], check[2]
                t = (check[3] * K) if len(check) > 3 else tol
                if kind == "g5x":
                    got = after["g5x"]["XYZ".index(key)]
                    want_u = want * K
                elif kind == "param":
                    got, want_u = vals.get(key), want * K
                elif kind == "angle":
                    got, want_u, t = vals.get(key), want, C.ANGLE_TOL
                elif kind == "rot":
                    got, want_u, t = after["rot"], want, C.ANGLE_TOL
                elif kind == "pos":
                    got = after["pos"]["XYZ".index(key)]
                    want_u = want * K
                elif kind == "widget":
                    got, want_u = w.get(key), want * K
                else:
                    raise ValueError(kind)
                msg = check_value(f"{kind} {key}", got, want_u, t)
                if msg:
                    res["fail"].append(("WRONG RESULT: " if exp != "ok" else "") + msg)
        if ok:
            for i, d in enumerate(res.get("modal", [])):
                if d:
                    res["fail"].append(f"step {i + 1} left modes changed: {', '.join(d)}")
            if after["contact"]:
                res["fail"].append("ended with the probe touching something")
        elif res.get("modal") and res["modal"][-1]:
            res["notes"].append("after the failure modes are " + ", ".join(res["modal"][-1]) + " (Ferrum restores them)")
        if stopped:
            res["notes"].append(f"stopped by {stopped}")
        res["msg"] = res.get("errors", [])[:2]
    except Exception:
        res["fail"].append("bench error: " + traceback.format_exc(limit=3))
    finally:
        # Out of the way, whatever happened: straight up is always clear.
        try:
            setp("world.phase", 3)
            if float(hal.get_value("world.length-error")) != 0:
                setp("world.length-error", 0)
            ensure_ready()
            c.feedrate(1.0)
            mdi("G40")
            mdi(f"{UNIT} G90 G53 G0 Z{C.TRAVEL_Z * K:.6f}")
        except Exception:
            res["fail"].append("bench error while clearing: " + traceback.format_exc(limit=2))
    res["secs"] = round(time.time() - t0, 1)
    return res


def report(results):
    with open(os.path.join(OUT, "results.json"), "w") as f:
        json.dump(results, f, indent=1)
    bad = [r for r in results if r["fail"]]
    lines = [f"# Probe bench ({UNITS}): {len(results) - len(bad)}/{len(results)} cases pass", ""]
    crashes = [r for r in results if any("CRASH" in x for x in r["fail"])]
    wrong = [r for r in results if any("WRONG RESULT" in x for x in r["fail"])]
    lines.append(f"Crashes: {len(crashes)}. Silent wrong results: {len(wrong)}.")
    lines.append(f"Deepest probing overtravel: {max([r.get('max_pen', 0) for r in results] or [0]):.4f} {UNITS}.")
    lines.append("")
    for r in results:
        mark = "FAIL" if r["fail"] else "ok"
        extra = ""
        if not r.get("ran") and r.get("msg"):
            extra = f" (refused: {r['msg'][0]})"
        lines.append(f"- **{mark}** `{r['name']}` [{r['expect']}] {r['secs']} s{extra}")
        for x in r["fail"]:
            lines.append(f"  - {x}")
        for x in r["notes"]:
            lines.append(f"  - note: {x}")
    with open(os.path.join(OUT, "report.md"), "w") as f:
        f.write("\n".join(lines) + "\n")
    return len(bad)


def main():
    only = os.environ.get("BENCH_ONLY", "")
    seed = int(os.environ.get("BENCH_SEED", "1"))
    fuzz = int(os.environ.get("BENCH_FUZZ", "0"))
    todo = C.all_cases(UNITS, random.Random(seed), fuzz)
    if only:
        pats = only.split(",")
        todo = [x for x in todo if any(re.search(p, x["name"]) for p in pats)]
    log(f"{len(todo)} cases ({UNITS})")
    results = []
    try:
        ensure_ready()
        for i, case in enumerate(todo):
            r = run_case(case)
            results.append(r)
            log(f"{i + 1}/{len(todo)} {'FAIL' if r['fail'] else 'ok  '} {case['name']} ({r['secs']} s)"
                + ("".join("\n      " + x for x in r["fail"])))
            report(results)
    finally:
        n = report(results)
        with open(os.path.join(OUT, "done"), "w") as f:
            f.write(f"{n}\n")
        # End LinuxCNC: the display exiting shuts it down.
        try:
            c.state(linuxcnc.STATE_ESTOP)
        except Exception:
            pass


if __name__ == "__main__":
    main()
