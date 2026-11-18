# millgui in the LinuxCNC simulator

Runs millgui against a real LinuxCNC 2.9.10 (task, interpreter, motion,
halui, the M6 remap, Probe Basic's macros) with the mill's own `mymill.ini`,
`mymill.hal` and subroutines. Only the 7i76E is faked (`sim_card.py`), so
the drawbar and spindle interlocks, HLFB arming, probe manager and touch-off
latches all run as written. It never talks to the mill.

The pretend stock is a block at machine X150..280, Y-140..-40 whose top the
tool tip reaches at machine Z -250 (the tip is the spindle nose minus the
tool table length of the tool in the spindle). The tool setter sits at the
G30 spot with its top where `tool_touch_off.ngc` expects it (#3010).
Homing runs the INI's real search and latch moves: each home switch closes
40 mm (`SIM_HOME_TRAVEL`) from where its joint started, in the search
direction.

## One-time setup (Debian/Ubuntu dev box or cloud container)

Use LinuxCNC **v2.9.10**, the version on the Pi. The 2.9 branch head adds a
spindle at-speed wait after M5 that stalls G38 probing with mymill.hal's
at-speed wiring.

```sh
git clone --branch v2.9.10 --depth 1 https://github.com/LinuxCNC/linuxcnc.git ~/lcnc/src
cd ~/lcnc/src/src
./autogen.sh
./configure --with-realtime=uspace --disable-gtk --disable-gtk2 PYTHON=/usr/bin/python3.12
make -j"$(nproc)"
sudo make setuid      # or run as a normal user; LinuxCNC refuses root
```

Headers and libraries for millgui's NML backend, laid out as a sysroot:

```sh
mkdir -p ~/lcnc/sysroot/usr/include
ln -s ~/lcnc/src/include ~/lcnc/sysroot/usr/include/linuxcnc
ln -s ~/lcnc/src/lib ~/lcnc/sysroot/usr/lib
cd millgui
LCNC_SYSROOT=~/lcnc/sysroot CARGO_TARGET_DIR=target-nml cargo build -p millgui
```

Build the probe manager comp once (`halcompile --install` on
`hal_components/probe_manager.comp` with rip-environment sourced).

## Each run

```sh
millgui/sim/make-config ~/simcfg --probe-basic vendor/probe_basic
source ~/lcnc/src/scripts/rip-environment
cd ~/simcfg
MILLGUI_BIN=$PWD/../mymill/millgui/target-nml/debug/millgui MILLGUI_RENDERER=software \
  linuxcnc millgui.ini
```

`MILLGUI_LOG_MESSAGES=1` prints every LinuxCNC message to stderr. Without a
window manager the window opens at 800x600; run one (openbox) on the X
display for a full-size window.

Handy pins: `halcmd setp hm2_7i76e.0.sim.force-probe 1` holds the probe
tripped, `sim.force-setter` the tool setter.

The pendant is a stand-in (`sim_pendant.py`, component `whb`) with the
nets of `xhc-whb04b-6.hal` that millgui cares about. Press START/PAUSE with
`halcmd setp whb.sim.press-start 1` then `0`; turn the wheel (the driver
asks for MANUAL) with `whb.sim.wheel 1` then `0`; unplug it with
`whb.sim.connected 0`.

## Stock machines (not the PM-728VT)

`make-stock-config` builds a simulator from one of LinuxCNC's own sample
configs, as a stranger would have it: no mymill HAL, no Probe Basic, no
`millgui.toml`. Use it to check that millgui works on machines that are not
Gardner's:

```sh
L=~/lcnc/src/configs/sim/axis
millgui/sim/make-stock-config ~/sims/inch   $L/axis.ini --remote 8088      # inch, XYZ
millgui/sim/make-stock-config ~/sims/mm     $L/axis_mm.ini --remote 8088   # mm, XYZ
millgui/sim/make-stock-config ~/sims/gantry $L/gantry/gantry.ini --remote 8088              # inch, XYZY gantry
millgui/sim/make-stock-config ~/sims/xyza   $L/wrapped_rotary/wrapped_rotary.ini --remote 8088  # mm, XYZA, no home switches
cd ~/sims/inch && MILLGUI_BIN=... linuxcnc axis.ini
```

The sample configs answer M6 with `hal_manualtoolchange` and have no probe
input. This is for trying millgui by hand on LinuxCNC's own samples (home
switches, XYZY joint order); the automated checks on stock machines are in
`millgui/harness` (`sim-mm`, `sim-inch`, `sim-xyza`, `sim-gantry`).
