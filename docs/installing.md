# Installing and updating Ferrum

`scripts/ferrum-update` installs, updates and rolls back Ferrum. Each release on GitHub carries it as `install.sh`, so the first install and every later update use the same script.

```sh
curl -fsSL https://github.com/gardnervickers/ferrum/releases/latest/download/install.sh | bash
curl -fsSL .../install.sh | bash -s -- --prefix ~/linuxcnc/configs/mymill/ferrum --beta
```

## What you need

- A 64-bit system: **arm64** (Raspberry Pi 4 and 5 with the 64-bit Raspberry Pi OS or LinuxCNC image) or **amd64**. The installer asks `dpkg` which architecture the system is, not the kernel, so a 32-bit Raspberry Pi OS (whose kernel is 64-bit) gets a clear refusal rather than a build it cannot start.
- **glibc 2.35 or newer**: Debian 12 (bookworm, the LinuxCNC 2.9 ISO) and 13 (trixie), Ubuntu 22.04 and newer. Debian 11 and older cannot start the build; the self-test says so and nothing changes.
- **LinuxCNC 2.9**, any 2.9.x. Ferrum reads LinuxCNC's status in 2.9's layout (`emc.hh` and `emc_nml.hh` are identical from 2.9.0 to 2.9.10), so Debian's own 2.9.4 package works as well as linuxcnc.org's 2.9.10. Each build says which series it is for (its `LINUXCNC` file); the installer asks `linuxcnc_var` (or the `linuxcnc-uspace` package) and refuses another series, and Ferrum itself refuses to start as the display of one. A LinuxCNC 2.10 has the same `libnml.so.0` but may not have the same layout, so it would load and then show wrong values: that is what the check is for. `--force` installs anyway.
- `curl` or `wget`, `tar`, `sha256sum`, and `gpgv` (on every Debian and Ubuntu: apt needs it). No root.
- The screen is an X11 window. On a Wayland desktop it runs through XWayland, as LinuxCNC's own screens do. It draws on the GPU when the driver is a real one, and in software otherwise (a Raspberry Pi 5 on Debian 12's stock Mesa 22: Mesa from bookworm-backports gives it the GPU).

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
| `ferrum-update channel stable\|beta`, `--beta`, `--stable` | which builds to follow (remembered; `edge` is beta's old name) | yes |
| `ferrum-update auto on\|off` | automatic updates on that channel | yes |

Other options: `--prefix DIR`, `--ini FILE` (the INI the self-test checks; remembered), `--keep N` (old versions kept, default 3), `--force` (install although `--check` got worse, or for another LinuxCNC series), `--allow-unsigned` (install a release from before signing, by hand only), `--reinstall`.

"While LinuxCNC runs" means any of `linuxcnc`, `linuxcncsvr`, `milltask`, `rtapi_app`, `io`, `iocontrol`, `halui` or `ferrum` is running.

## Channels

- **stable** (default) follows tagged releases (`vX.Y.Z`).
- **beta** (opt in: `ferrum-update channel beta`, or SETUP › SOFTWARE › UPDATES FROM › BETA) also gets the builds between releases: every commit to main that passes CI, including the LinuxCNC simulator harness. They are published as the rolling `edge` prerelease (its old name, so older installers still find it). Versions look like `0.1.1-beta.202611221000.71d7ce6`: the last release's number, then the build time.

Versions order by their three numbers, then a build from main after that release (`-beta.`, and the older `-edge.` and `-dev.` from `scripts/deploy`) comes after the plain release, and those order by build time. So `0.1.1 < 0.1.1-beta.202611221000.… < 0.1.2`. A plain `ferrum-update`, `stage` and automatic updates never go back to an older version: after switching from beta to stable you stay on the beta build until a release newer than it comes out. `--version V`, `rollback` and naming the channel (`--stable`) do go back.

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
2. **The download is verified.** `SHA256SUMS` must carry a good signature (`SHA256SUMS.sig`) from Ferrum's release key, which is built into the installer, and the tarball must match it. A damaged, partial or tampered download is refused, and so is an unsigned release (unless `--allow-unsigned`, which automatic updates never use). Checksums alone would only catch damage, since they come from the same place as the download. See [Release signing](#release-signing).
3. **The new build is tested before the switch.** `ferrum --version` must start and print the expected version, which catches missing libraries, the wrong architecture and the wrong LinuxCNC. `ferrum --check` on your INI must not crash, and must not report a problem the version in use does not.
4. **The switch is atomic.** `current` is replaced with one `rename()`, so a power cut leaves the old version or the new one, never a mix.
5. **The first start is a trial.** If the new version crashes (segfault, abort, panic) within 60 seconds of its first start, `run` puts the previous version back, starts it, and shows *"Ferrum X crashed on its first start, so Y is back in use. Restart LinuxCNC before using the machine."* The crashed build's HAL component outlives it, so the screen that comes back has no HAL pins, and the screen watchdog keeps E-stop latched until LinuxCNC restarts. A version that crashed, or one you rolled back from, is not staged again automatically.
6. **Rollback is instant and offline:** the previous version is still on disk.

A screen that *freezes* rather than crashes is caught by the screen watchdog (`hal/watchdog.hal`), which latches E-stop. It is not caught by the launcher.

## Automatic updates

Automatic updates follow the channel: tagged releases on **stable**, and on **beta** the newest beta build, at most one a day (so a machine on beta is at most a day behind main, and a burst of commits gives one update, not many). Every safety step below applies the same to beta builds: verified download, self-test, trial start with crash fallback, and a build you rolled back from is not staged again.

`ferrum-update auto on` makes `run` start `ferrum-update stage --if-due` in the background, at the lowest priority and at most once every 20 hours, each time the screen starts. Staging downloads, verifies and self-tests the new version into `versions/` and changes nothing in use. The next time `run` starts the screen, before the screen opens, the staged version goes into use (on trial), and the screen says *"Updated Ferrum X to Y. ferrum-update rollback goes back."*

`run` does not switch while another `ferrum` process is alive, such as the `--boot` screen starting LinuxCNC's own DISPLAY copy. The staged version then waits for the next start.

## For the SETUP screen

`ferrum-update status --json` prints one object with no network access. Every key is always present: `installed_in`, `current`, `previous`, `installed` (newest first), `channel`, `auto`, `available`, `staged`, `on_trial`, `fell_back`, `skipped`, `last_check`, `last_error` and `running`. The installer lives at `PREFIX/ferrum-update`, where PREFIX is three folders above the canonical path of the running binary (`PREFIX/versions/<v>/bin/ferrum`).

`run` exports `FERRUM_LAUNCHER` (its own path). To restart through it, so that a staged version goes into use, exec `$FERRUM_LAUNCHER` with Ferrum's own arguments.

## Releasing

- **Stable:** on GitHub, Actions > **Cut a release** > Run workflow, pick patch, minor or major (or type a version). From a terminal: `gh workflow run cut-release.yml -R gardnervickers/ferrum -f bump=minor`. It refuses unless CI passed on main's newest commit, then bumps `app/Cargo.toml`, commits "Release vX.Y.Z", tags it and starts the release build. (Pushing a `v<version>` tag by hand, matching `app/Cargo.toml`, does the same.) `.github/workflows/release.yml` builds on Debian 12 against LinuxCNC 2.9.10 for aarch64 and x86_64, runs the installer tests, installs the package and checks that it loads, and publishes release `v<version>` with `ferrum-linux-<arch>.tar.gz`, `install.sh`, `VERSION` and `SHA256SUMS`.
- **Beta:** every main commit whose CI passes refreshes the `edge` prerelease (the beta channel). Nothing to do by hand.
- `scripts/package BINARY VERSION ARCH` packs any build, for example a local one for `ferrum-update --from`. `scripts/deploy` uses it to install on the author's Pi.

## Release signing

The release workflow signs `SHA256SUMS` with an ed25519 OpenPGP key held in the repository's `FERRUM_SIGNING_KEY` Actions secret, and `ferrum-update` checks it with `gpgv` against the public keys in its `RELEASE_KEYS` block (also in `docs/release-key.asc`). The installed `ferrum-update` is what checks the next update, so a release can only be replaced by one signed with a key the machine already trusts.

- **Setting it up (once):** on your own computer, with `gpg` and a signed-in `gh`, in a checkout of this repository: `scripts/release-key`. It makes the key, stores it as the secret, saves a backup at `~/ferrum-release-key.asc` (keep it offline: GitHub never gives a secret back), and writes the public key into `scripts/ferrum-update` for you to commit and push. Releases built after that push are signed; installers from before it check checksums only.
- **Until then** releases are unsigned and the installer, having no key, checks checksums only. Once the installer trusts a key, a release built without the secret fails rather than publishing something installs would refuse.
- **Replacing the key** (lost or leaked): add the new public key beside the old one in `RELEASE_KEYS`, publish a release signed with the old key, and only then put the new private key in the secret and remove the old public key. A machine that never installed that middle release cannot check releases signed with the new key and needs a fresh `install.sh`, so treat the backup as precious.
- A signature proves a build came from this repository's release workflow. It does not stop someone who controls the download from serving an older signed release.

## Tests

`scripts/test-installer` runs the updater and launcher against stand-in builds served from a local folder: install, update, rollback, refusal while LinuxCNC runs, damaged downloads, builds that do not start, `--check` regressions, crash fallback, trial expiry, shutdown signals, adopting a hand-copied install, pruning, channels, staging and the JSON status. It needs no LinuxCNC, network or display, and CI runs it on both architectures.
