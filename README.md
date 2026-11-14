# millgui

The Rust/Slint replacement for Probe Basic. Design: `ARCHITECTURE.md`;
what is left: `REQUIREMENTS.md`; mockup: `docs/mockup-v16.html`.

```
app/              Slint UI, feature modules, INI config
crates/linuxcnc/  Machine trait: NML backend (cpp/lcnc_shim.*) and a simulated machine
crates/hal/       the GUI's HAL component (dlopen of liblinuxcnchal)
hal/<module>.hal  postgui nets, run only for enabled modules
run               LinuxCNC DISPLAY entry on the Pi
```

## Build and deploy (from the Mac)

```bash
millgui/scripts/deploy
```

- `scripts/build` cross-compiles for `aarch64-unknown-linux-gnu` with clang/lld
  from nixpkgs and the Pi's own glibc and libstdc++, so the C++ shim matches
  LinuxCNC's `libnml` ABI. Rust and the target come from rustup.
- `scripts/fetch-sysroot` copies those headers and libraries from the Pi into
  `sysroot/` (gitignored, ~330 MB). Rerun it after upgrading the Pi.
- `scripts/deploy` copies only millgui's own files: `millgui/run`,
  `millgui/bin/millgui`, `millgui/mdi.toml`, `millgui/hal/*.hal`, `millgui/web/dist` (the
  browser screen, from `scripts/build-web`) and `millgui.ini`.
- Browser screen (off by default; it has no access control): with `[MILLGUI]REMOTE_PORT = 8088`, open `http://<pi>:8088/` (`?perf` logs
  frame timings to the console). It mirrors the touchscreen; navigation is per screen.
- Override the target with `MILLGUI_PI=user@host` and `MILLGUI_DEST=path`.

## Develop on the Mac

```bash
millgui/scripts/run-local --ini millgui.ini       # window, simulated machine
```

```bash
cd millgui && cargo run -- --ini ../millgui.ini --screenshot /tmp/frame.png --screen run
```

The second renders one frame headless to a PNG. Mac builds have no NML
backend; they always use the simulated machine (`--sim` forces it on the Pi).

## Run on the Pi

```bash
linuxcnc ~/linuxcnc/configs/mymill/millgui.ini
```

`millgui.ini` sets its own `[DISPLAY]` and `[MILLGUI]` sections, then
`#INCLUDE`s `mymill.ini`, so the machine config stays single-sourced and Probe
Basic (`mymill.ini`) is unchanged. LinuxCNC returns the first match per
section, so `[DISPLAY]` keys from `mymill.ini` are not visible; the few that
milltask, halui and millgui read are repeated in `millgui.ini`.

`[MILLGUI]MODULES` lists the enabled feature modules. After its HAL pins exist,
millgui runs `millgui/hal/<id>.hal` for each enabled module (with `halcmd -k`,
so one missing pin does not drop the rest), then `[MILLGUI]POSTGUI_HALFILE` if set.
