#!/bin/sh
# Start LinuxCNC with ferrum's splash covering the whole start:
#   ~/linuxcnc/configs/mymill/ferrum/boot [ferrum.ini]
# ferrum opens at once and runs `linuxcnc` itself (see app/src/boot.rs);
# `linuxcnc ferrum.ini` still works, with the splash only while ferrum connects.
here="$(cd "$(dirname "$0")" && pwd)"
exec "$here/run" --boot "${1:-$here/../ferrum.ini}"
