# millgui

Skeleton for the Rust replacement of Probe Basic (plan and mockup: `../rust_gui/`).
Today it is a Slint window that owns a `millgui` HAL component:

| Pin | Dir | Connected by `millgui_postgui.hal` |
| --- | --- | --- |
| `millgui.machine-is-on` | in | `machine-is-on` (halui.machine.is-on) |
| `millgui.estop-is-activated` | in | `halui.estop.is-activated` |
| `millgui.output` | out | unconnected; toggled by the on-screen button |
| `millgui.heartbeat` | out | unconnected; increments every 50 ms |

## Build and deploy (from the Mac)

```bash
millgui/scripts/deploy        # = scripts/build + copy to cnc@cncmill.localdomain
```

- `scripts/build` cross-compiles with `cargo zigbuild` for
  `aarch64-unknown-linux-gnu.2.36` (Pi: Debian 12, glibc 2.36). zig, cargo-zigbuild
  and pkg-config come from nixpkgs; Rust and the `aarch64-unknown-linux-gnu`
  target come from rustup.
- `scripts/fetch-sysroot` copies the Pi's fontconfig and LinuxCNC headers and
  libraries into `sysroot/` (gitignored, ~6 MB). Rerun it after upgrading the Pi.
  Only fontconfig is linked today; the LinuxCNC parts are there for the NML shim.
- HAL is loaded with dlopen (`liblinuxcnchal.so.0`), so the binary needs no
  LinuxCNC link step. `scripts/run-local` runs it natively on the Mac with
  "HAL unavailable" for UI work.
- Rendering uses Slint's software renderer: the Pi has no GPU GL here (llvmpipe).
- Override the target with `MILLGUI_PI=user@host` and `MILLGUI_DEST=path`.

## Run on the Pi

```bash
linuxcnc ~/linuxcnc/configs/mymill/millgui.ini
```

`millgui.ini` sets its own `[DISPLAY]` and `[MILLGUI]` sections, then
`#INCLUDE`s `mymill.ini`, so the machine config stays single-sourced and Probe
Basic (`mymill.ini`) is unchanged. LinuxCNC returns the first match per
section, so `[DISPLAY]` keys from `mymill.ini` are not visible; the few that
milltask and halui read are repeated in `millgui.ini`.

The GUI runs `[MILLGUI]POSTGUI_HALFILE` with `halcmd -i <ini> -f` after
`hal_ready`, like the stock GUIs do for `[HAL]POSTGUI_HALFILE` (that key belongs
to Probe Basic).

Not handled yet: the manual tool change handshake (M6 will wait), NML
status/command/error, preview.
