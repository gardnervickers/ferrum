#!/bin/sh
# Start LinuxCNC with millgui's splash covering the whole start:
#   ~/linuxcnc/configs/mymill/millgui/boot [millgui.ini]
# millgui opens at once and runs `linuxcnc` itself (see app/src/boot.rs);
# `linuxcnc millgui.ini` still works, with the splash only while millgui connects.
here="$(cd "$(dirname "$0")" && pwd)"
exec "$here/run" --boot "${1:-$here/../millgui.ini}"
