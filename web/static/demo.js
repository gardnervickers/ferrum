// The browser demo's link between the screen (ferrum_web.js, which expects a
// websocket to a mill) and the simulated Ferrum in demo-worker.js. It
// replaces the page's WebSocket before the screen starts, so the screen code
// is the same one a real mill serves.
const worker = new Worker(new URL("demo-worker.js", import.meta.url), { type: "module" });
let socket = null;

class DemoSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  constructor() {
    this.readyState = DemoSocket.CONNECTING;
    this.binaryType = "arraybuffer";
    this.onmessage = null;
    this.onclose = null;
    this.onopen = null;
    socket = this;
    // The screen sets its handlers right after this; then hand over what
    // the engine sent before the screen was up (its first full snapshot).
    setTimeout(() => {
      if (ready) this.open();
      for (const data of early.splice(0)) this.onmessage && this.onmessage({ data });
    }, 0);
  }
  open() {
    this.readyState = DemoSocket.OPEN;
    this.onopen && this.onopen({});
  }
  send(text) {
    worker.postMessage({ text });
  }
  close() {}
}

let ready = false;
const early = [];
const deliver = (data) => {
  if (socket && socket.onmessage) socket.onmessage({ data });
  else early.push(data);
};
const failed = (msg) => {
  console.error(msg);
  const el = document.getElementById("loading");
  if (el) el.textContent = "The demo stopped: " + msg.split("\n")[0];
};

worker.onmessage = (e) => {
  const d = e.data;
  if (d.open) {
    ready = true;
    socket && socket.open();
  } else if (d.text !== undefined) {
    deliver(d.text);
  } else if (d.png) {
    deliver(d.png);
  } else if (d.error) {
    failed(d.error);
  }
};
worker.onerror = (e) => failed(e.message || "the simulated mill failed to start");

window.WebSocket = DemoSocket;
window.ferrumDemo = true;
worker.postMessage({ start: new URL(".", import.meta.url).href });
