# Installing and updating Ferrum

`scripts/ferrum-update` installs, updates and rolls back Ferrum. Each release on GitHub carries it as `install.sh`, so the first install and every later update use the same script.

```sh
curl -fsSL https://github.com/gardnervickers/ferrum/releases/latest/download/install.sh | bash
curl -fsSL .../install.sh | bash -s -- --prefix ~/linuxcnc/configs/mymill/ferrum --edge
```

## What you need

- Debian 12 (bookworm) or newer, arm64 (Raspberry Pi 5) or x86_64.
- LinuxCNC 2.9 installed. The builds link against LinuxCNC 2.9.10's `libnml`, so a LinuxCNC that is not 2.9 is refused by the self-test, and nothing changes.
- `curl` or `wget`, `tar`, `sha256sum`. No root.

## Commands

| Command | What it does | While LinuxCNC runs |
| --- | --- | --- |
| `ferrum-update` | install the newest release on your channel | refused |
| `ferrum-update --version 0.2.0` | install that release | refused |
| `ferrum-update --from F.tar.gz` | install a package you have (checksum from `SHA256SUMS` beside it, or `--sha256`) | refused |
| `ferrum-update rollback` | go back to the version before | refused |
| `ferrum-update use V` | switch to any installed version | refused |
| `ferrum-update apply` | put a staged update into use now | refused |
| `ferrum-update stage` | download, verify and test the newest; it goes into use at the next start | yes |
| `ferrum-update unstage` | forget a staged update | yes |
| `ferrum-update use V --on-restart`, `rollback --on-restart` | switch at the next start instead | yes |
| `ferrum-update check` | is there a newer version? (changes nothing) | yes |
| `ferrum-update status [--json]`, `list` | what is installed | yes |
| `ferrum-update channel stable\|edge`, `--edge`, `--stable` | which builds to follow (remembered) | yes |
| `ferrum-update auto on\|off` | automatic updates | yes |

Other options: `--prefix DIR`, `--ini FILE` (the INI the self-test checks; remembered), `--keep N` (old versions kept, default 3), `--force` (install although `--check` got worse), `--reinstall`.

"While LinuxCNC runs" means any of `linuxcnc`, `linuxcncsvr`, `milltask`, `rtapi_app`, `io`, `iocontrol`, `halui` or `ferrum` is running.

## Channels

- **stable** (default) follows tagged releases (`vX.Y.Z`).
- **edge** follows every commit to main that passes CI, including the LinuxCNC simulator harness. Versions look like `0.1.0-edge.202611191500.71d7ce6`.

## The install folder

```
PREFIX/
  versions/0.2.0/        one folder per version: bin/ferrum, macros/, hal/, web/dist, mdi.toml, run, boot, ferrum-update
  versions/0.1.0/
  current -> versions/0.2.0
  previous -> versions/0.1.0
  bin, macros, hal, web, mdi.toml -> current/...
  run, boot, ferrum-update      copies from the version in use (they must outlive a version)
  machine.toml                 installed once, never overwritten
  update.log                   every switch, staged update and fallback
  .ferrum-install              channel, INI, auto, keep
```

Ferrum finds its macros beside its binary, and a config's `ferrum/hal/<module>.hal` through the `hal` link, so an install in `<config>/ferrum` (the mymill layout) works without changing the INI.

The first install into a folder that already has a hand-copied Ferrum (from the old `scripts/deploy`) keeps that build as version `pre-installer`, so `ferrum-update rollback` reaches it. It also moves the replaced files to `backup-<time>/`; nothing is deleted.

## What makes an update safe

1. **Nothing changes while LinuxCNC runs.** A switch happens only with LinuxCNC stopped, or in `run` before the screen opens (staged updates). An update never lands under a running program.
2. **The download is verified** against the release's `SHA256SUMS`. A damaged or partial download is refused.
3. **The new build is tested before the switch.** `ferrum --version` must start and print the expected version, which catches missing libraries, the wrong architecture and the wrong LinuxCNC. `ferrum --check` on your INI must not crash, and must not report a problem the version in use does not.
4. **The switch is atomic.** `current` is replaced with one `rename()`, so a power cut leaves the old version or the new one, never a mix.
5. **The first start is a trial.** If the new version crashes (segfault, abort, panic) within 60 seconds of its first start, `run` puts the previous version back, starts it, and shows *"Ferrum X crashed on its first start, so Y is back in use. Restart LinuxCNC before using the machine."* The crashed build's HAL component outlives it, so the screen that comes back has no HAL pins, and the screen watchdog keeps E-stop latched until LinuxCNC restarts. A version that crashed, or one you rolled back from, is not staged again automatically.
6. **Rollback is instant and offline:** the previous version is still on disk.

A screen that *freezes* rather than crashes is caught by the screen watchdog (`hal/watchdog.hal`), which latches E-stop. It is not caught by the launcher.

## Automatic updates

`ferrum-update auto on` makes `run` start `ferrum-update stage --if-due` in the background, at the lowest priority and at most once every 20 hours, each time the screen starts. Staging downloads, verifies and self-tests the new version into `versions/` and changes nothing in use. The next time `run` starts the screen, before the screen opens, the staged version goes into use (on trial), and the screen says *"Updated Ferrum X to Y. ferrum-update rollback goes back."*

`run` does not switch while another `ferrum` process is alive, such as the `--boot` screen starting LinuxCNC's own DISPLAY copy. The staged version then waits for the next start.

## For the SETUP screen

`ferrum-update status --json` prints one object with no network access. Every key is always present: `installed_in`, `current`, `previous`, `installed` (newest first), `channel`, `auto`, `available`, `staged`, `on_trial`, `fell_back`, `skipped`, `last_check`, `last_error` and `running`. The installer lives at `PREFIX/ferrum-update`, where PREFIX is three folders above the canonical path of the running binary (`PREFIX/versions/<v>/bin/ferrum`).

`run` exports `FERRUM_LAUNCHER` (its own path). To restart through it, so that a staged version goes into use, exec `$FERRUM_LAUNCHER` with Ferrum's own arguments.

## Releasing

- **Stable:** on GitHub, Actions > **Cut a release** > Run workflow, pick patch, minor or major (or type a version). From a terminal: `gh workflow run cut-release.yml -R gardnervickers/ferrum -f bump=minor`. It refuses unless CI passed on main's newest commit, then bumps `app/Cargo.toml`, commits "Release vX.Y.Z", tags it and starts the release build. (Pushing a `v<version>` tag by hand, matching `app/Cargo.toml`, does the same.) `.github/workflows/release.yml` builds on Debian 12 against LinuxCNC 2.9.10 for aarch64 and x86_64, runs the installer tests, installs the package and checks that it loads, and publishes release `v<version>` with `ferrum-linux-<arch>.tar.gz`, `install.sh`, `VERSION` and `SHA256SUMS`.
- **Edge:** every main commit whose CI passes refreshes the `edge` prerelease. Nothing to do by hand.
- `scripts/package BINARY VERSION ARCH` packs any build, for example a local one for `ferrum-update --from`. `scripts/deploy` uses it to install on the author's Pi.

## Tests

`scripts/test-installer` runs the updater and launcher against stand-in builds served from a local folder: install, update, rollback, refusal while LinuxCNC runs, damaged downloads, builds that do not start, `--check` regressions, crash fallback, trial expiry, shutdown signals, adopting a hand-copied install, pruning, channels, staging and the JSON status. It needs no LinuxCNC, network or display, and CI runs it on both architectures.
