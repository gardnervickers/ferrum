#!/usr/bin/env python3
"""LinuxCNC's own view of the machine, for the harness (never for ferrum).

Reads one command per line on stdin, answers one JSON line on stdout:
  stat    -> every linuxcnc.stat() field
  errors  -> every error-channel message since start, as {"kind", "text"}
Runs with LinuxCNC's rip-environment sourced; the simulator only.
"""
import json
import sys

import linuxcnc

KINDS = {
    linuxcnc.NML_ERROR: "error",
    linuxcnc.OPERATOR_ERROR: "error",
    linuxcnc.NML_TEXT: "text",
    linuxcnc.OPERATOR_TEXT: "text",
    linuxcnc.NML_DISPLAY: "display",
    linuxcnc.OPERATOR_DISPLAY: "display",
}

stat = linuxcnc.stat()
err = linuxcnc.error_channel()
seen = []
# "errors-to-ferrum": LinuxCNC's error channel is a queue with one reader;
# the scenario lets ferrum read it, as on the machine.
draining = True


def plain(v):
    if isinstance(v, (bool, int, float, str)) or v is None:
        return v
    names = getattr(type(v), "__match_args__", None)
    if names and hasattr(v, "n_fields"):  # struct sequences (tool_result)
        return {n: plain(getattr(v, n)) for n in names}
    if isinstance(v, (list, tuple)):
        return [plain(x) for x in v]
    if isinstance(v, dict):
        return {str(k): plain(x) for k, x in v.items()}
    # tool_result and friends: their named fields
    fields = [f for f in dir(v) if not f.startswith("_") and not callable(getattr(v, f))]
    if fields:
        return {f: plain(getattr(v, f)) for f in fields}
    return str(v)


def drain():
    while True:
        e = err.poll()
        if not e:
            return
        kind, text = e
        seen.append({"kind": KINDS.get(kind, str(kind)), "text": text})


def snapshot():
    stat.poll()
    out = {}
    for name in dir(stat):
        if name.startswith("_") or name in ("poll",):
            continue
        try:
            v = getattr(stat, name)
        except Exception:
            continue
        if callable(v):
            continue
        out[name] = plain(v)
    # Short tool table: only real entries.
    out["tool_table"] = [t for t in out.get("tool_table", []) if isinstance(t, dict) and t.get("id", -1) != -1]
    return out


for line in sys.stdin:
    cmd = line.strip()
    try:
        if draining:
            drain()
        if cmd == "nodrain":
            draining = False
            ans = {}
        elif cmd == "stat":
            ans = snapshot()
        elif cmd == "errors":
            ans = seen
        else:
            ans = {"error": f"unknown command {cmd}"}
    except Exception as e:  # LinuxCNC gone: say so, keep reading
        ans = {"error": str(e)}
    sys.stdout.write(json.dumps(ans) + "\n")
    sys.stdout.flush()
