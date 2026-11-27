import { afterEach, describe, expect, it, vi } from "vitest";
import * as storage from "../lib/safeStorage";
import type { StreamError, StreamOptions } from "../lib/remotePlayStream";

// A Play Together friend put this stream's PS5 user on their controller, so the PS5 ended the
// stream ("Server shutting down"): it starts again by itself, once in a while, and says why.

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

afterEach(() => {
  vi.restoreAllMocks();
  made.length = 0;
  notes.length = 0;
  startAgain.mockClear();
});

describe("restartsAfterGuestPick", () => {
  it("only for a stream that played, ended by a friend the engine named, not again too soon", async () => {
    const { restartsAfterGuestPick, GUEST_PICK_RESTART_GAP_MS } = await import("./remotePlay");
    const took = { code: "console_ended", guestPicked: 2 };
    expect(restartsAfterGuestPick(took, true, undefined, 1000)).toBe(true);
    expect(restartsAfterGuestPick(took, false, undefined, 1000)).toBe(false);
    expect(restartsAfterGuestPick({ code: "console_ended" }, true, undefined, 1000)).toBe(false);
    expect(restartsAfterGuestPick({ code: "lost", guestPicked: 2 }, true, undefined, 1000)).toBe(false);
    expect(restartsAfterGuestPick(took, true, 1000, 1000 + GUEST_PICK_RESTART_GAP_MS - 1)).toBe(false);
    expect(restartsAfterGuestPick(took, true, 1000, 1000 + GUEST_PICK_RESTART_GAP_MS)).toBe(true);
  });
});

describe("a stream a friend's controller took", () => {
  const begin = async (host: string) => {
    const { useRemotePlayStore } = await import("./remotePlay");
    vi.spyOn(storage, "safeSetItem").mockImplementation(() => true);
    useRemotePlayStore.setState({ stream: null, error: null, prefs: { ...useRemotePlayStore.getState().prefs, bitrateKbps: 0, dualSense: false } });
    const target = { host, name: "PS5", registKey: "", rpKey: "" };
    await useRemotePlayStore.getState().start(target);
    const o = made[made.length - 1];
    o.onPhase("playing");
    return { store: useRemotePlayStore, o, target };
  };
  const took = (o: StreamOptions) =>
    o.onError({ code: "console_ended", detail: "Server shutting down", guestPicked: 2 } as StreamError);

  it("starts again by itself and says which player, then shows the error the next time", async () => {
    const { store, o, target } = await begin("192.168.1.70");
    took(o);
    expect(store.getState().error).toBeNull();
    expect(notes[0]).toContain("Player 2 picked your PS5 user");
    await vi.waitFor(() => expect(startAgain).toHaveBeenCalledWith(target, {}));
    // Again within a few minutes: left to the player, with the reason.
    const again = await begin("192.168.1.70");
    took(again.o);
    expect(again.store.getState().error?.guestPicked).toBe(2);
    expect(startAgain).toHaveBeenCalledTimes(1);
  });

  it("does nothing new for a takeover nobody in Play Together caused", async () => {
    const { store, o } = await begin("192.168.1.71");
    o.onError({ code: "console_ended", detail: "Server shutting down" } as StreamError);
    expect(store.getState().error?.code).toBe("console_ended");
    await new Promise((r) => setTimeout(r, 10));
    expect(startAgain).not.toHaveBeenCalled();
  });
});
