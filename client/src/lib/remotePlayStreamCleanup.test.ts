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
/** Every animation frame asked for, so none outlives its test. */
const frames = new Set<ReturnType<typeof setTimeout>>();
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
    requestAnimationFrame: (cb: () => void) => {
      const id = setTimeout(cb, 16);
      frames.add(id);
      return id as unknown as number;
    },
    cancelAnimationFrame: (id: number) => clearTimeout(id),
  });
  const docEvents = new EventTarget();
  vi.stubGlobal("document", {
    createElement: () => fakeCanvas(),
    addEventListener: docEvents.addEventListener.bind(docEvents),
    removeEventListener: docEvents.removeEventListener.bind(docEvents),
    dispatchEvent: docEvents.dispatchEvent.bind(docEvents),
    visibilityState: "visible",
  });
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
  // A stream a test left running must not tick once `window` is gone.
  for (const id of frames) clearTimeout(id);
  frames.clear();
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

  it("does not start behind a stop that came while it checked the codecs", async () => {
    const o = options();
    const s = new RemotePlayStream(o);
    const starting = s.start();
    s.stop();
    await starting;
    await flush();
    // The one socket is the stop's own word to the engine; the start never connected.
    expect(sockets).toHaveLength(1);
    expect(o.phases).not.toContain("connecting");
    expect(o.phases).toEqual(["ended"]);
  });

  const opened = async (o = options()) => {
    const errors: Array<{ code: string; detail: string }> = [];
    o.onError = (e) => errors.push(e);
    const s = new RemotePlayStream(o);
    await s.start();
    const ws = sockets[sockets.length - 1];
    ws.onopen?.();
    return { s, ws, errors, o };
  };

  it("says why the console ended the stream, and that nothing is running any more", async () => {
    const a = await opened();
    a.ws.onmessage?.({ data: JSON.stringify({ stage: "ended", reason: "Server shutting down" }) });
    expect(a.errors).toEqual([{ code: "console_ended", detail: "Server shutting down" }]);
    a.s.stop();
    await flush();

    const b = await opened();
    b.ws.onmessage?.({ data: JSON.stringify({ stage: "ended", reason: "no session" }) });
    expect(b.errors[0].code).toBe("no_session");
    b.s.stop();
    await flush();
  });

  it("says nothing about a stream the player stopped", async () => {
    const a = await opened();
    a.s.stop();
    a.ws.onmessage?.({ data: JSON.stringify({ stage: "ended", reason: "Client stopped" }) });
    expect(a.errors).toEqual([]);
    await flush();
  });

  it("sends what is held again on a new socket: the engine let go when the old one left", async () => {
    const sent: string[] = [];
    const o = options();
    o.readPad = () => ({ buttons: 1, l2: 0, r2: 0, lx: 0, ly: 0, rx: 0, ry: 0 });
    const s = new RemotePlayStream(o);
    await s.start();
    const first = sockets[0] as unknown as { readyState: number; send: (m: string) => void; onopen?: () => void };
    first.readyState = 1;
    first.send = (m) => sent.push(m);
    vi.stubGlobal("WebSocket", Object.assign(FakeSocket, { OPEN: 1 }));
    s.sendPad();
    s.sendPad();
    expect(sent.filter((m) => m.includes('"pad"'))).toHaveLength(1);
    (s as unknown as { connect: (a: boolean) => void }).connect(true);
    const second = sockets[1] as unknown as { readyState: number; send: (m: string) => void };
    second.readyState = 1;
    second.send = (m) => sent.push(m);
    s.sendPad();
    expect(sent.filter((m) => m.includes('"pad"'))).toHaveLength(2);
    s.stop();
    await flush();
  });

  it("lets go when the page goes to the background, where no frame will read the pad", async () => {
    const o = options();
    let hidden = 0;
    o.onHidden = () => hidden++;
    const s = new RemotePlayStream(o);
    await s.start();
    const send = vi.spyOn(s, "sendPad");
    (document as unknown as { visibilityState: string }).visibilityState = "hidden";
    document.dispatchEvent(new Event("visibilitychange"));
    expect(hidden).toBe(1);
    expect(send).toHaveBeenCalledTimes(1);
    s.stop();
    await flush();
    document.dispatchEvent(new Event("visibilitychange"));
    expect(hidden).toBe(1);
  });

  it("ends a session no decoder will ever take, rather than stream to nothing", async () => {
    const a = await opened();
    (a.s as unknown as { cannotDecode: (d: string) => void }).cannotDecode("no");
    expect(a.errors[0].code).toBe("decode");
    expect((a.s as unknown as { stopping: boolean }).stopping).toBe(true);
    await flush();
  });
});

describe("the picture on this device", () => {
  /** A socket that keeps what the page sends, opened. */
  class SentSocket extends FakeSocket {
    sent: string[] = [];
    send(m?: unknown) {
      this.sent.push(String(m));
    }
  }
  const started = async (o: StreamOptions) => {
    vi.stubGlobal("WebSocket", SentSocket);
    const s = new RemotePlayStream(o);
    await s.start();
    await flush();
    const ws = sockets[0] as unknown as SentSocket;
    ws.readyState = 1;
    ws.onopen?.();
    return { s, ws };
  };
  const msgs = (ws: SentSocket) => ws.sent.map((m) => JSON.parse(m) as Record<string, unknown>);
  const internals = (s: RemotePlayStream) => s as unknown as { video: unknown; drawer: unknown; ticking: boolean; header: unknown };

  it("off, keeps nothing running and sends no controller; on, comes back and asks for a keyframe", async () => {
    let press = 0;
    const heard: boolean[] = [];
    const o: StreamOptions = {
      ...options(),
      readPad: () => ({ buttons: ++press, l2: 0, r2: 0, lx: 0, ly: 0, rx: 0, ry: 0 }),
      onPictureHere: (on) => heard.push(on),
    };
    const { s, ws } = await started(o);
    expect(ctxs).toHaveLength(1);
    const first = s.canvas;
    const pass = { release: vi.fn(), lost: false };
    const decoder = { close: vi.fn(), state: "configured" };
    const raw = internals(s);
    raw.drawer = { kind: "gl", pass };
    raw.video = decoder;
    s.sendPad();
    expect(msgs(ws).some((m) => "pad" in m)).toBe(true);

    ws.sent.length = 0;
    s.setPictureHere(false);
    expect(heard).toEqual([false]);
    expect(msgs(ws)).toEqual([{ picture: false }]);
    expect(decoder.close).toHaveBeenCalled();
    expect(raw.video).toBeNull();
    expect(pass.release).toHaveBeenCalled();
    expect(raw.drawer).toBeNull();
    expect(ctxs[0].closed).toBe(1);
    expect(workers[0].terminated).toBe(true);
    expect(raw.ticking).toBe(false);
    // A new canvas: the old one's context is gone with it. And its pixels.
    expect(s.canvas).not.toBe(first);
    expect(first.width).toBe(0);
    // No controller, no recording, and media still on its way is dropped.
    s.sendPad();
    expect(msgs(ws).some((m) => "pad" in m)).toBe(false);
    expect(s.captureMedia(60)).toBeNull();
    await expect(s.snapshot()).resolves.toBeNull();
    const record = new Uint8Array([1, 0, 0, 1, 0x65]).buffer;
    ws.onmessage?.({ data: record });
    expect(raw.video).toBeNull();
    // The decoder's watchdog has nothing to judge.
    (s as unknown as { watchDecoder(): void }).watchDecoder();
    expect(raw.video).toBeNull();

    // The session goes on: the reconnect keeps asking for it off.
    ws.sent.length = 0;
    ws.onopen?.();
    expect(msgs(ws)[0].picture).toBe(false);

    ws.sent.length = 0;
    s.setPictureHere(true);
    expect(heard).toEqual([false, true]);
    expect(msgs(ws)).toEqual([{ picture: true }, { keyframe: true }]);
    expect(ctxs).toHaveLength(2);
    expect(raw.ticking).toBe(true);
    s.stop();
    ws.close();
    await flush();
  });

  it("starts off when asked, with no sound made, and says so in the start message", async () => {
    const { s, ws } = await started({ ...options(), pictureHere: false });
    expect(ctxs).toHaveLength(0);
    expect(s.pictureHere).toBe(false);
    expect(msgs(ws)[0]).toMatchObject({ host: "192.168.1.100", picture: false });
    s.stop();
    ws.close();
    await flush();
  });

  it("a page joining says nothing and follows what the engine tells it", async () => {
    const heard: boolean[] = [];
    const { s, ws } = await started({ ...options(), attach: true, onPictureHere: (on) => heard.push(on) });
    expect("picture" in msgs(ws)[0]).toBe(false);
    ws.onmessage?.({ data: JSON.stringify({ picture: false }) });
    expect(heard).toEqual([false]);
    expect(s.pictureHere).toBe(false);
    s.stop();
    ws.close();
    await flush();
  });
});

describe("RemotePlayStream with a decoder that never shows a picture (#683)", () => {
  /** Every decoder fails as soon as it is configured, and remembers how it was set up. */
  function failingDecoders() {
    const made: VideoDecoderConfig[] = [];
    class FailingDecoder {
      static isConfigSupported = async () => ({ supported: true });
      constructor(private readonly init: VideoDecoderInit) {}
      configure(c: VideoDecoderConfig) {
        made.push(c);
        queueMicrotask(() => this.init.error(new DOMException("Decoder failure", "EncodingError")));
      }
      decode() {}
      close() {}
      flush = async () => {};
    }
    vi.stubGlobal("VideoDecoder", FailingDecoder);
    return made;
  }
  // An H.264 header with a minimal SPS and PPS.
  const HEADER = Uint8Array.from([0, 0, 0, 1, 0x67, 0x4d, 0x40, 0x28, 0xe8, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80]);
  const settle = async () => {
    for (let i = 0; i < 40; i++) await flush();
  };
  const open = (s: RemotePlayStream, hevc: boolean) => {
    const p = s as unknown as { hevc: boolean; codecChoice: string; header: Uint8Array; openVideo(): void };
    p.hevc = hevc;
    p.codecChoice = hevc ? "h265" : "h264";
    p.header = HEADER;
    p.openVideo();
  };

  it("on H.264: hardware a few times, software a few times, then one error and the end", async () => {
    const made = failingDecoders();
    const errors: string[] = [];
    const o = { ...options(), onError: (e: { code: string }) => errors.push(e.code) };
    const s = new RemotePlayStream(o);
    await s.start();
    open(s, false);
    await settle();
    expect(made.map((c) => c.hardwareAcceleration)).toEqual([
      "prefer-hardware",
      "prefer-hardware",
      "prefer-hardware",
      "prefer-software",
      "prefer-software",
      "prefer-software",
    ]);
    expect(errors).toEqual(["decode"]);
    expect(o.phases[o.phases.length - 1]).toBe("ended");
  });

  it("on HEVC: ends the stream with no error and asks for H.264 next time", async () => {
    failingDecoders();
    const errors: string[] = [];
    const onHevcFailed = vi.fn();
    const o = { ...options(), onHevcFailed, onError: (e: { code: string }) => errors.push(e.code) };
    const s = new RemotePlayStream(o);
    await s.start();
    open(s, true);
    await settle();
    expect(onHevcFailed).toHaveBeenCalledTimes(1);
    expect(errors).toEqual([]);
    expect(o.phases[o.phases.length - 1]).toBe("ended");
  });
});
