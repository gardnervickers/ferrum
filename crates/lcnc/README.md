# lcnc

Typed Rust bindings for [LinuxCNC](https://linuxcnc.org) 2.9 (tested with
2.9.10, uspace): what LinuxCNC's Python module offers as `linuxcnc.stat`,
`linuxcnc.command` and `linuxcnc.error_channel`, plus userspace HAL
components, without Python.

```rust
use lcnc::{Axis, Command, Connection, TaskMode};
use std::time::Duration;

let mut lc = Connection::open(None)?;          // default linuxcnc.nml
let s = lc.poll()?;                            // &Status, typed
println!("{:?} {:?} X={:.3}", s.task_state, s.interp, s.work_position()[Axis::X]);

let serial = lc.send(&Command::SetMode(TaskMode::Mdi))?;
lc.wait_finished(serial, Duration::from_secs(1))?;
lc.send(&Command::Mdi("G0 X10".into()))?;

while let Some(m) = lc.next_message() {
    eprintln!("{:?}: {}", m.kind, m.text);
}
```

HAL pins are typed by value and direction; an input pin has no `set`:

```rust
use lcnc::hal::{Component, In, Out};

let comp = Component::new("mygui")?;
let request: In<bool> = comp.pin("tool-change")?;
let done: Out<bool> = comp.pin("tool-changed")?;
comp.ready()?;
```

## What is typed

- States and modes are enums (`TaskState`, `TaskMode`, `InterpState`,
  `ExecState`, `TrajMode`, `RcsStatus`, `MotionType`, `SpindleDirection`,
  `Wcs`). A value this crate doesn't know decodes to `Unknown(raw)`
  rather than being guessed.
- Positions are `Pose` (index by `Axis::X` or by number), modal codes are
  `ActiveGCodes`/`GCode` (`GCode(641)` prints as `G64.1`), command serials
  are `Serial`.
- `Command` takes `JogTarget::Axis(Axis::X)` or `JogTarget::Joint(0)`,
  `JointSel::All` or `JointSel::Joint(n)`, `Rotation::Forward`/`Reverse`,
  paths as `PathBuf`.
- Errors are one `Error` enum.

## Cost

`Connection::poll` copies `EMC_STAT` into a flat C struct (the shim) and
decodes it into a `Status` kept inside the connection, updated in place:
no allocation unless a string (the file name, the MDI command) changed, and
no decode at all when no new status was published since the last poll.
Run `cargo run --release --example poll_cost` against a running LinuxCNC to
measure it on your machine.

## Building

The NML side is a small C++ shim (`cpp/lcnc_shim.*`) compiled against
LinuxCNC's headers and linked to `libnml`, `liblinuxcncini` and
`liblinuxcnc.a`. `build.rs` looks for them in, first match wins:

1. `LCNC_INCLUDE_DIR` (and `LCNC_LIB_DIR`);
2. `LCNC_SYSROOT`: `$LCNC_SYSROOT/usr/include/linuxcnc` and
   `$LCNC_SYSROOT/usr/lib`, for cross builds against the target's own glibc
   and libstdc++;
3. `/usr/include/linuxcnc` (Debian's `linuxcnc-uspace-dev`).

Without any of them, or on a non-Linux host, the crate still builds:
`Connection::open` returns `Error::NoBackend` and `lcnc::HAS_NML` is false.
Code that only uses the types (a simulator, tests, a remote client) builds
anywhere.

The HAL side needs nothing at build time: `liblinuxcnchal.so.0` is loaded
with `dlopen` when the first component is made.

Features: `nml` and `hal`, both on by default.

## Threads

`Connection` is `Send`, not `Sync`. Poll and send from one thread (LinuxCNC's
own GUIs poll every 50 to 100 ms) and pass `Status` snapshots by value.
`send` returns once task has taken the command off the channel (the command
buffer holds one message); it doesn't wait for the command to finish.

## Scope

Spindle 0 only. No tool table or parameter file access (they are files;
read them directly). No `linuxcnc.ini` parsing.

## License

MIT, like the rest of Ferrum (`ferrum/LICENSE`). The `nml` build
compiles against LinuxCNC's headers and links its `libnml` (GPL-2.0), so a
binary built with it is distributed under the GPL's terms as well; MIT
source is compatible with that.
