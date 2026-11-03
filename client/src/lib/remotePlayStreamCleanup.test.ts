import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RemotePlayStream, type StreamOptions } from "./remotePlayStream";

// What a stream lets go of when it ends: a page that played many sessions in one run kept each
// one's WebGL context, worklet and stream alive (an AudioWorkletNode outlives its closed context
// while its port listens, and its port held the stream).

class FakePort {
  onmessage: ((e: MessageEvent) => void) | null = null;
  sent: unknown[] = [];
  closed = false;
  postMessage(m: unknown) {
    this.sent.push(m);
  }
  close() {
    this.closed = true;
  }
}

let ctxs: FakeAudioContext[] = [];
let nodes: FakeWorkletNode[] = [];
let workers: FakeWorker[] = [];
let sockets: FakeSocket[] = [];

class FakeAudioContext {
  state = "running";
  sampleRate = 48000;
  currentTime = 0;
  destination = {};
  closed = 0;
  modules: string[] = [];
  audioWorklet = {
    addModule: async (url: string) => {
      this.modules.push(url);
    },
  };
  constructor() {
    ctxs.push(this);
  }
  resumes = 0;
  /** As a browser that holds sound back until a click: resume() only works when allowed. */
  allowResume = true;
  private listeners: Array<() => void> = [];
  addEventListener(_t: string, f: () => void) {
    this.listeners.push(f);
  }
  setState(st: string) {
    this.state = st;
    for (const f of this.listeners) f();
  }
  resume() {
    this.resumes++;
    if (this.allowResume && this.state !== "closed") this.setState("running");
    return Promise.resolve();
  }
  createGain() {
    return { gain: { value: 1 }, connect: vi.fn(), disconnect: vi.fn() };
  }
  close() {
    this.closed++;
    this.state = "closed";
    return Promise.resolve();
  }
}

class FakeWorkletNode {
  port = new FakePort();
  connect = vi.fn();
  disconnect = vi.fn();
  constructor() {
    nodes.push(this);
  }
}

class FakeWorker {
  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: ((e: ErrorEvent) => void) | null = null;
  sent: unknown[] = [];
  terminated = false;
  constructor(readonly url: URL | string) {
    workers.push(this);
  }
  postMessage(m: unknown) {
    this.sent.push(m);
    if (m && typeof m === "object" && "probe" in m) setTimeout(() => this.onmessage?.({ data: { ready: true } } as MessageEvent), 0);
  }
  terminate() {
    this.terminated = true;
  }
}

class FakeSocket {
  static OPEN = 1;
  readyState = 0;
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: ((m: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(readonly url: string) {
    sockets.push(this);
  }
  send() {}
  close() {
    this.readyState = 3;
    setTimeout(() => this.onclose?.(), 0);
  }
}

/** Just what a stream touches of the page (vitest runs in node here). */
function fakeCanvas() {
  return {
    dataset: {} as Record<string, string>,
    className: "",
    width: 0,
    height: 0,
    style: { setProperty() {}, imageRendering: "", cssText: "" },
    addEventListener() {},
    getContext: () => null,
    ownerDocument: { defaultView: null },
  };
}

beforeEach(() => {
  // Events too: held-back sound waits for a click or key on the window.
  const events = new EventTarget();
  vi.stubGlobal("window", {
    addEventListener: events.addEventListener.bind(events),
    removeEventListener: events.removeEventListener.bind(events),
    dispatchEvent: events.dispatchEvent.bind(events),
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
    requestAnimationFrame: (cb: () => void) => setTimeout(cb, 16) as unknown as number,
    cancelAnimationFrame: (id: number) => clearTimeout(id),
  });
  vi.stubGlobal("document", { createElement: () => fakeCanvas() });
  ctxs = [];
  nodes = [];
  workers = [];
  sockets = [];
  vi.stubGlobal("AudioContext", FakeAudioContext);
  vi.stubGlobal("AudioWorkletNode", FakeWorkletNode);
  vi.stubGlobal("Worker", FakeWorker);
  vi.stubGlobal("WebSocket", FakeSocket);
  vi.stubGlobal("VideoDecoder", { isConfigSupported: async () => ({ supported: true }) });
});
afterEach(() => {
  vi.unstubAllGlobals();
});

function options(): StreamOptions & { phases: string[] } {
  const phases: string[] = [];
  return {
    phases,
    host: "192.168.1.100",
    registKey: "00",
    rpKey: "00",
    width: 1920,
    height: 1080,
    codec: "h264",
    onPhase: (p) => phases.push(p),
    onStats: () => {},
    onError: () => {},
    readPad: () => ({ buttons: 0, l2: 0, r2: 0, lx: 0, ly: 0, rx: 0, ry: 0 }),
  };
}

const flush = () => new Promise((r) => setTimeout(r, 5));

describe("RemotePlayStream at its end", () => {
  it("gives back the WebGL context and lets go of the worklet, worker and AudioContext", async () => {
    const o = options();
    const s = new RemotePlayStream(o);
    await s.start();
    // The decoding worker gets ready in the background (the session does not wait for it).
    await flush();
    expect(ctxs).toHaveLength(1);
    // The worklet once per context, from the app's own file (not a blob: URL).
    expect(ctxs[0].modules).toHaveLength(1);
    expect(ctxs[0].modules[0]).not.toMatch(/^blob:/);
    expect(nodes).toHaveLength(1);
    // Decoding in the worker, which feeds the worklet over a port of its own.
    expect(workers).toHaveLength(1);
    expect(workers[0].sent.some((m) => m && typeof m === "object" && "port" in m)).toBe(true);
    expect(nodes[0].port.sent.some((m) => m && typeof m === "object" && "feed" in m)).toBe(true);

    // As if a frame had been drawn with WebGL.
    const pass = { release: vi.fn(), dispose: vi.fn(), lost: false };
    (s as unknown as { drawer: unknown }).drawer = { kind: "gl", pass };

    s.stop();
    await flush();
    expect(o.phases[o.phases.length - 1]).toBe("ended");
    expect(pass.release).toHaveBeenCalledTimes(1);
    expect((s as unknown as { drawer: unknown }).drawer).toBeNull();
    expect(ctxs[0].closed).toBe(1);
    expect(nodes[0].port.onmessage).toBeNull();
    expect(nodes[0].port.closed).toBe(true);
    expect(nodes[0].disconnect).toHaveBeenCalled();
    expect(workers[0].terminated).toBe(true);
    expect(workers[0].onmessage).toBeNull();
  });

  it("keeps decoding on the page where a worker cannot decode Opus", async () => {
    vi.stubGlobal(
      "Worker",
      class extends FakeWorker {
        postMessage(m: unknown) {
          this.sent.push(m);
          setTimeout(() => this.onmessage?.({ data: { ready: false } } as MessageEvent), 0);
        }
      },
    );
    const s = new RemotePlayStream(options());
    await s.start();
    await flush();
    expect(workers[0].terminated).toBe(true);
    expect(nodes[0].port.sent.some((m) => m && typeof m === "object" && "feed" in m)).toBe(false);
    expect((s as unknown as { audioWorker: unknown }).audioWorker).toBeNull();
    s.stop();
    await flush();
  });

  it("schedules the sound itself where no worklet loads, and still closes the context", async () => {
    vi.stubGlobal(
      "AudioContext",
      class extends FakeAudioContext {
        audioWorklet = { addModule: async () => Promise.reject(new Error("refused")) };
      },
    );
    const s = new RemotePlayStream(options());
    await s.start();
    expect(nodes).toHaveLength(0);
    expect(workers).toHaveLength(0);
    s.stop();
    await flush();
    expect(ctxs[0].closed).toBe(1);
  });

  it("starts held-back sound on the next click and stops listening at the end", async () => {
    vi.stubGlobal(
      "AudioContext",
      class extends FakeAudioContext {
        state = "suspended";
        allowResume = false;
      },
    );
    const s = new RemotePlayStream(options());
    await s.start();
    await flush();
    const ctx = ctxs[0];
    expect(s.soundBlocked()).toBe(true);
    // A click anywhere in the app: the browser now allows it.
    ctx.allowResume = true;
    window.dispatchEvent(new Event("pointerdown"));
    expect(ctx.state).toBe("running");
    expect(s.soundBlocked()).toBe(false);
    // Stopped later (another app took the sound device): a key starts it again.
    ctx.setState("suspended");
    expect(s.soundBlocked()).toBe(true);
    window.dispatchEvent(new Event("keydown"));
    expect(s.soundBlocked()).toBe(false);
    // Ended: a click does nothing to the closed context.
    s.stop();
    await flush();
    const before = ctx.resumes;
    window.dispatchEvent(new Event("pointerdown"));
    expect(ctx.resumes).toBe(before);
  });
});
