Error.stackTraceLimit = 64;
// The browser demo's engine: Ferrum built for WASI (scripts/build-demo) with
// its simulated machine, in a Web Worker so the screen never waits on it.
// Nothing here can reach a machine: the build has no LinuxCNC, HAL or
// network, and the file system is in memory.
import { WASI, File, PreopenDirectory, Directory, OpenFile, ConsoleStdout, wasi } from "./vendor/browser_wasi_shim/index.js";

// The shim writes a file's times at the wrong offsets and keeps none; FILES
// shows "modified", so keep one per file (the page's load time to start).
const loadedAt = BigInt(Date.now()) * 1000000n;
wasi.Filestat.prototype.write_bytes = function (view, ptr) {
  view.setBigUint64(ptr, this.dev, true);
  view.setBigUint64(ptr + 8, this.ino, true);
  view.setUint8(ptr + 16, this.filetype);
  view.setBigUint64(ptr + 24, this.nlink, true);
  view.setBigUint64(ptr + 32, this.size, true);
  view.setBigUint64(ptr + 40, this.atim, true);
  view.setBigUint64(ptr + 48, this.mtim, true);
  view.setBigUint64(ptr + 56, this.ctim, true);
};
for (const kind of [File, Directory]) {
  const stat = kind.prototype.stat;
  kind.prototype.stat = function () {
    const st = stat.call(this);
    st.atim = st.mtim = st.ctim = this.mtime ?? loadedAt;
    return st;
  };
}
const fdWrite = OpenFile.prototype.fd_write;
OpenFile.prototype.fd_write = function (data) {
  this.file.mtime = BigInt(Date.now()) * 1000000n;
  return fdWrite.call(this, data);
};

const dec = new TextDecoder();
let inst = null;
const inbox = [];

// A Directory tree from {"a/b.ngc": "text"}.
function tree(files) {
  const root = new Map();
  for (const [path, text] of Object.entries(files)) {
    const parts = path.split("/");
    let dir = root;
    for (const p of parts.slice(0, -1)) {
      if (!dir.has(p)) dir.set(p, new Directory(new Map()));
      dir = dir.get(p).contents;
    }
    dir.set(parts.at(-1), new File(new TextEncoder().encode(text)));
  }
  return root;
}

async function start(base) {
  const [files, wasm] = await Promise.all([
    fetch(new URL("demo-files.json", base)).then(r => r.json()),
    WebAssembly.compileStreaming(fetch(new URL("ferrum-engine.wasm", base))),
  ]);
  const root = new Map([
    ["demo", new Directory(tree(files))],
    ["home", new Directory(new Map([["demo", new Directory(new Map())]]))],
    ["tmp", new Directory(new Map())],
  ]);
  const log = (line) => console.log("ferrum:", line);
  const fds = [
    new OpenFile(new File([])),
    ConsoleStdout.lineBuffered(log),
    ConsoleStdout.lineBuffered(log),
    new PreopenDirectory("/", root),
  ];
  // Powered, homed and with a program loaded, ready for CYCLE START.
  const args = ["ferrum", "-ini", "/demo/config/ferrum-demo.ini", "--sim", "--open", "/demo/nc_files/bracket-sample.ngc"];
  const shim = new WASI(args, ["HOME=/home/demo", "FERRUM_MACROS=/demo/macros"], fds);
  const bytes = (ptr, len) => new Uint8Array(inst.exports.memory.buffer, ptr, len);
  inst = await WebAssembly.instantiate(wasm, {
    wasi_snapshot_preview1: shim.wasiImport,
    ferrum: {
      send_text: (ptr, len) => postMessage({ text: dec.decode(bytes(ptr, len)) }),
      send_png: (ptr, len) => {
        const png = bytes(ptr, len).slice().buffer;
        postMessage({ png }, [png]);
      },
    },
  });
  shim.start(inst);
  postMessage({ open: true });
  loop();
}

function deliver(text) {
  const b = new TextEncoder().encode(text);
  const ptr = inst.exports.ferrum_demo_alloc(b.length);
  new Uint8Array(inst.exports.memory.buffer, ptr, b.length).set(b);
  inst.exports.ferrum_demo_recv(ptr, b.length);
}

function loop() {
  try {
    while (inbox.length) deliver(inbox.shift());
    inst.exports.ferrum_demo_tick();
  } catch (e) {
    postMessage({ error: String(e && e.stack || e) });
    return;
  }
  setTimeout(loop, 8);
}

onmessage = (e) => {
  if (e.data.start) start(e.data.start).catch(err => postMessage({ error: String(err && err.stack || err) }));
  else if (typeof e.data.text === "string") inbox.push(e.data.text);
};
