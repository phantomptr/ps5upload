import { afterEach, describe, expect, it, vi } from "vitest";
import * as storage from "../lib/safeStorage";
import type { StreamError, StreamOptions } from "../lib/remotePlayStream";

// The Bitrate setting: what is kept, what a session asks for, and the one fall back to Auto
// when the PS5 did not start at a high one.

const made: StreamOptions[] = [];
vi.mock("../lib/remotePlayStream", async (orig) => {
  const real = await orig<typeof import("../lib/remotePlayStream")>();
  class FakeStream {
    currentPhase = "connecting";
    constructor(readonly o: StreamOptions) {
      made.push(o);
    }
    setVolume() {}
    setAudioBuffer() {}
    setFit() {}
    setReplay() {}
    setPicture() {}
    setPictureHere() {}
    async start() {}
    stop() {}
  }
  return { ...real, RemotePlayStream: FakeStream };
});
const startAgain = vi.fn(async () => {});
vi.mock("./rpQuality", () => ({ useRpQualityStore: { getState: () => ({ startAgain }) } }));
const notes: string[] = [];
vi.mock("./notifications", () => ({ pushNotification: (_k: string, m: string) => notes.push(m) }));

const target = { host: "192.168.1.5", name: "PS5", registKey: "", rpKey: "" };

afterEach(() => {
  vi.restoreAllMocks();
  made.length = 0;
  notes.length = 0;
  startAgain.mockClear();
});

describe("Bitrate in the saved settings", () => {
  it("keeps every offered bitrate, the new high ones and the old ones, and nothing else", async () => {
    const { loadRpPrefs, RP_BITRATES } = await import("./remotePlay");
    const stored = (v: unknown) => vi.spyOn(storage, "safeGetItem").mockReturnValue(JSON.stringify(v));
    for (const b of [0, 50000, 40000, 30000, 20000, 15000, 3000]) {
      expect(RP_BITRATES).toContain(b);
      stored({ bitrateKbps: b });
      expect(loadRpPrefs().bitrateKbps).toBe(b);
    }
    for (const b of [35000, 60000, -1, "30000"]) {
      stored({ bitrateKbps: b });
      expect(loadRpPrefs().bitrateKbps).toBe(0);
    }
    // A save from before Bitrate existed: Auto.
    stored({ res: "1280x720" });
    expect(loadRpPrefs().bitrateKbps).toBe(0);
  });
});

describe("a session's bitrate", () => {
  const begin = async (bitrateKbps: number) => {
    const { useRemotePlayStore } = await import("./remotePlay");
    vi.spyOn(storage, "safeSetItem").mockImplementation(() => true);
    useRemotePlayStore.setState({ stream: null, error: null, prefs: { ...useRemotePlayStore.getState().prefs, bitrateKbps, dualSense: false } });
    await useRemotePlayStore.getState().start(target);
    return { store: useRemotePlayStore, o: made[made.length - 1] };
  };
  const fail = (o: StreamOptions, code: string) => o.onError({ code, detail: "Nagare did not init" } as StreamError);

  it("asks for the chosen one, and for none on Auto", async () => {
    expect((await begin(40000)).o.bitrateKbps).toBe(40000);
    expect((await begin(0)).o.bitrateKbps).toBeUndefined();
  });

  it("falls back once to Auto when the PS5 did not start at a high one, and says so", async () => {
    const { store, o } = await begin(50000);
    fail(o, "video_refused");
    expect(store.getState().error).toBeNull();
    expect(store.getState().prefs.bitrateKbps).toBe(0);
    expect(notes[0]).toContain("50 Mbps");
    await vi.waitFor(() => expect(startAgain).toHaveBeenCalledWith(target, { bitrateKbps: 0 }));
    // The same session failing again is shown, not retried.
    fail(o, "video_refused");
    expect(store.getState().error?.code).toBe("video_refused");
    expect(startAgain).toHaveBeenCalledTimes(1);
  });

  it("shows the error as it is at Auto or up to 15 Mbps, for other errors, and once the picture came", async () => {
    for (const kbps of [0, 15000]) {
      const { store, o } = await begin(kbps);
      fail(o, "video_refused");
      expect(store.getState().error?.code).toBe("video_refused");
    }
    const busy = await begin(30000);
    fail(busy.o, "busy");
    expect(busy.store.getState().error?.code).toBe("busy");
    const played = await begin(30000);
    played.o.onPhase("playing");
    fail(played.o, "lost");
    expect(played.store.getState().error?.code).toBe("lost");
    expect(played.store.getState().prefs.bitrateKbps).toBe(30000);
    await new Promise((r) => setTimeout(r, 10));
    expect(startAgain).not.toHaveBeenCalled();
  });
});
