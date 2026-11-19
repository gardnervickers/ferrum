# Developing Ferrum on mymill (the author's PM-728VT)

The Rust/Slint replacement for Probe Basic. Design: `ARCHITECTURE.md`;
what is left: `REQUIREMENTS.md`; mockup: `docs/mockup-v16.html`.

```
app/              Slint UI, feature modules, INI config
crates/lcnc/      standalone LinuxCNC bindings: typed status, commands, messages (NML shim), HAL pins
crates/machine/   Machine trait (LinuxCNC or the simulator) and the worker with Ferrum's mode rules
hal/<module>.hal  postgui nets, run only for enabled modules
run               LinuxCNC DISPLAY entry on the Pi
```

## Build and deploy (from the Mac)

```bash
ferrum/scripts/deploy
```

- `scripts/build` cross-compiles for `aarch64-unknown-linux-gnu` with clang/lld
  from nixpkgs and the Pi's own glibc and libstdc++, so the C++ shim matches
  LinuxCNC's `libnml` ABI. Rust and the target come from rustup.
- `scripts/fetch-sysroot` copies those headers and libraries from the Pi into
  `sysroot/` (gitignored, ~330 MB). Rerun it after upgrading the Pi.
- `scripts/deploy` packs the build (`scripts/package`, version
  `<crate>-dev.<time>.<sha>`) and installs it in `mymill/ferrum` with
  `scripts/ferrum-update --from`, the installer users get: it refuses while
  LinuxCNC runs, tests the build, switches atomically and keeps the old one
  (`ferrum/ferrum-update rollback`). It also copies `ferrum.ini`. See
  `docs/installing.md`.
- Browser screen (off by default; it has no access control): with `[FERRUM]REMOTE_PORT = 8088`, open `http://<pi>:8088/` (`?perf` logs
  frame timings to the console). It mirrors the touchscreen; navigation is per screen.
- Override the target with `FERRUM_PI=user@host` and `FERRUM_DEST=path`.

## Develop on the Mac

```bash
ferrum/scripts/run-local --ini ferrum.ini       # window, simulated machine
```

```bash
cd ferrum && cargo run -- --ini ../ferrum.ini --screenshot /tmp/frame.png --screen run
```

The second renders one frame headless to a PNG. Mac builds have no NML
backend; they always use the simulated machine (`--sim` forces it on the Pi).

## Browser demo

```bash
scripts/build-demo                                   # into web/demo-dist
python3 -m http.server -d web/demo-dist 8765         # then open http://localhost:8765/
```

The demo on GitHub Pages (`.github/workflows/demo.yml`, every push to main)
is Ferrum built for WASI (`wasm32-wasip1`) with its simulated machine, run
in a Web Worker (`web/static/demo-worker.js`, files in memory through the
vendored browser_wasi_shim). The page's screen is the ordinary browser build;
`web/static/demo.js` stands in for its websocket. `app/src/demo.rs` drives
Slint's timers and the machine loop from the worker, since WASI has no
threads (`util::background` and `machine::pump` cover the rest). The machine
config is `web/demo-config/`. The build has no LinuxCNC, HAL or network, so it
cannot reach a machine.

## Run on the Pi

```bash
~/linuxcnc/configs/mymill/ferrum/boot            # splash while LinuxCNC starts
linuxcnc ~/linuxcnc/configs/mymill/ferrum.ini    # as before
```

`ferrum/boot` (`ferrum --boot ferrum.ini`) opens the screen at once with
the splash and starts LinuxCNC underneath, showing the steps it reports
(realtime, each HAL file, motion, screen). If LinuxCNC fails to start, the
splash says why, with the lines LinuxCNC printed, TRY AGAIN and EXIT; the full
output is in `~/.cache/ferrum/linuxcnc-start.log`. A stale
`/tmp/linuxcnc.lock` gets a RESTART LINUXCNC (hold) button instead of
LinuxCNC's own question. EXIT stops LinuxCNC in order behind a STOPPING
splash. Started the old way, LinuxCNC opens the display last, so the splash
only covers Ferrum connecting.

The product name (on the splash, window title and UTIL) lives in one place:
`Brand` in `app/ui/brand.slint`.

`ferrum.ini` sets its own `[DISPLAY]` and `[FERRUM]` sections, then
`#INCLUDE`s `mymill.ini`, so the machine config stays single-sourced and Probe
Basic (`mymill.ini`) is unchanged. LinuxCNC returns the first match per
section, so `[DISPLAY]` keys from `mymill.ini` are not visible; the few that
milltask, halui and Ferrum read are repeated in `ferrum.ini`.

`[FERRUM]MODULES` lists the enabled feature modules. After its HAL pins exist,
Ferrum runs `ferrum/hal/<id>.hal` for each enabled module (with `halcmd -k`,
so one missing pin does not drop the rest), then `[FERRUM]POSTGUI_HALFILE` if set.
