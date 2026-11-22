import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { StreamOptions } from "../lib/remotePlayStream";
import { memoryStorage } from "../test-utils/storage";

// Play's picture from a capture device (#893): the light Remote Play session it starts, the
// device opened while the stream plays, its sound in place of Remote Play's, and the way back
// to the Remote Play picture when the device goes. No sound is played: the player is a stub.

const made: Array<{ o: StreamOptions; volume: number[] }> = [];
vi.mock("../lib/remotePlayStream", async (orig) => {
  const real = await orig<typeof import("../lib/remotePlayStream")>();
  class FakeStream {
    currentPhase = "connecting";
    volume: number[] = [];
    constructor(readonly o: StreamOptions) {
      made.push(this as unknown as { o: StreamOptions; volume: number[] });
    }
    setVolume(v: number) {
      this.volume.push(v);
    }
    setAudioBuffer() {}
    setFit() {}
    setReplay() {}
    setPicture() {}
    setPictureHere() {}
    captureMedia() {
      return null;
    }
    async start() {}
    stop() {}
  }
  return { ...real, RemotePlayStream: FakeStream };
});
const notes: string[] = [];
vi.mock("./notifications", () => ({ pushNotification: (_k: string, m: string) => notes.push(m) }));

class FakeTrack {
  readyState = "live";
  onended: (() => void) | null = null;
  stopped = false;
  constructor(readonly kind: "video" | "audio") {}
  stop() {
    this.stopped = true;
  }
  clone() {
    return new FakeTrack(this.kind);
  }
  getSettings() {
    return this.kind === "video" ? { width: 1920, height: 1080, frameRate: 60 } : {};
  }
}
class FakeMediaStream {
  constructor(private tracks: FakeTrack[] = []) {}
  getTracks() {
    return this.tracks;
  }
  getVideoTracks() {
    return this.tracks.filter((t) => t.kind === "video");
  }
  getAudioTracks() {
    return this.tracks.filter((t) => t.kind === "audio");
  }
}
const players: FakeAudio[] = [];
const last = <T,>(a: readonly T[]): T | undefined => a[a.length - 1];
class FakeAudio {
  srcObject: unknown = null;
  volume = 1;
  muted = false;
  autoplay = false;
  play = vi.fn(async () => {});
  pause = vi.fn();
  constructor() {
    players.push(this);
  }
}

const devices = [
  { kind: "videoinput", deviceId: "cap", label: "USB Video", groupId: "g" },
  { kind: "audioinput", deviceId: "capsnd", label: "USB Video Audio", groupId: "g" },
];
let getUserMedia: ReturnType<typeof vi.fn>;
const target = { host: "10.0.0.7", name: "PS5", registKey: "", rpKey: "" };

beforeEach(() => {
  vi.resetModules();
  vi.stubGlobal("window", {
    isTauri: true,
    localStorage: memoryStorage(),
    addEventListener: () => {},
    removeEventListener: () => {},
    location: { origin: "http://127.0.0.1:19113", pathname: "/", href: "http://127.0.0.1:19113/" },
  });
  getUserMedia = vi.fn(async (c: MediaStreamConstraints) => {
    const tracks = [new FakeTrack("video")];
    if (c.audio) tracks.push(new FakeTrack("audio"));
    return new FakeMediaStream(tracks);
  });
  vi.stubGlobal("navigator", {
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)",
    mediaDevices: { getUserMedia, enumerateDevices: async () => devices },
  });
  vi.stubGlobal("MediaStream", FakeMediaStream);
  vi.stubGlobal("Audio", FakeAudio);
});

afterEach(() => {
  vi.unstubAllGlobals();
  made.length = 0;
  notes.length = 0;
  players.length = 0;
});

async function play(sound: "rp" | "device", picture: "rp" | "device" = "device") {
  const { saveCaptureChoice } = await import("../lib/capturePicture");
  saveCaptureChoice(target.host, { picture, deviceId: "cap", label: "USB Video", sound });
  const cap = await import("./capturePicture");
  const { useRemotePlayStore } = await import("./remotePlay");
  useRemotePlayStore.setState({ prefs: { ...useRemotePlayStore.getState().prefs, dualSense: false, volume: 0.6 } });
  await useRemotePlayStore.getState().start(target);
  const s = made[made.length - 1];
  s.o.onPhase("playing");
  return { cap, store: useRemotePlayStore, s };
}

describe("a capture device's picture", () => {
  it("asks the console for the light stream, and nothing else changes without one", async () => {
    const light = await play("rp");
    expect(light.s.o).toMatchObject({ width: 1280, height: 720, fps: 30, bitrateKbps: 3000 });
    light.s.o.onPhase("ended");
    made.length = 0;
    const plain = await play("rp", "rp");
    expect(plain.s.o).toMatchObject({ width: 1920, height: 1080, fps: 60 });
    expect(getUserMedia).not.toHaveBeenCalled();
  });

  it("opens the device while the stream plays, with Remote Play's sound kept", async () => {
    const { cap, s } = await play("rp");
    await vi.waitFor(() => expect(cap.useCaptureStore.getState().media).not.toBeNull());
    expect(cap.useCaptureStore.getState().host).toBe(target.host);
    expect(getUserMedia.mock.calls[0][0]).toMatchObject({ video: { deviceId: { exact: "cap" } }, audio: false });
    expect(players).toHaveLength(0);
    expect(last(s.volume)).toBe(0.6);
  });

  it("plays the device's sound in place of Remote Play's, at the same volume", async () => {
    const { cap, s, store } = await play("device");
    await vi.waitFor(() => expect(cap.useCaptureStore.getState().media).not.toBeNull());
    expect(getUserMedia.mock.calls[0][0]).toMatchObject({ audio: { deviceId: { exact: "capsnd" }, echoCancellation: false } });
    expect(players).toHaveLength(1);
    expect(players[0].volume).toBeCloseTo(0.6);
    expect(last(s.volume)).toBe(0); // Remote Play's sound is silent meanwhile
    store.getState().setPrefs({ volume: 0.3 });
    expect(players[0].volume).toBeCloseTo(0.3);
    expect(last(s.volume)).toBe(0);
  });

  it("goes back to the Remote Play picture and sound when the device is unplugged", async () => {
    const { cap, s } = await play("device");
    await vi.waitFor(() => expect(cap.useCaptureStore.getState().media).not.toBeNull());
    const video = cap.useCaptureStore.getState().media!.getVideoTracks()[0] as unknown as FakeTrack;
    video.onended?.();
    expect(cap.useCaptureStore.getState().media).toBeNull();
    expect(last(s.volume)).toBe(0.6);
    expect(last(notes)).toContain("stopped");
  });

  it("says so and keeps the Remote Play picture when the app may not use it", async () => {
    getUserMedia.mockRejectedValueOnce(Object.assign(new Error("no"), { name: "NotAllowedError" }));
    const { cap } = await play("rp");
    await vi.waitFor(() => expect(last(notes)).toContain("camera access"));
    expect(cap.useCaptureStore.getState().media).toBeNull();
  });

  it("lets go of the device when the stream ends", async () => {
    const { cap, s } = await play("device");
    await vi.waitFor(() => expect(cap.useCaptureStore.getState().media).not.toBeNull());
    const tracks = cap.useCaptureStore.getState().media!.getTracks() as unknown as FakeTrack[];
    s.o.onPhase("ended");
    expect(cap.useCaptureStore.getState().media).toBeNull();
    expect(tracks.every((t) => t.stopped)).toBe(true);
    expect(players[0].pause).toHaveBeenCalled();
  });
});
