import { beforeEach, describe, expect, it, vi } from "vitest";

import type { RemotePlayStream } from "../lib/remotePlayStream";
import { useRemotePlayStore } from "./remotePlay";
import { useRpQualityStore, wasOffered } from "./rpQuality";

const target = { host: "192.168.1.100", name: "Living room", registKey: "00", rpKey: "00" };
const fakeStream = () => ({ setVolume() {}, setAudioBuffer() {}, setFit() {}, setReplay() {} }) as unknown as RemotePlayStream;

describe("the 720p offer and switch", () => {
  beforeEach(() => {
    useRpQualityStore.setState({ offerFor: null, switching: null });
  });

  it("offers once per session", () => {
    const s = fakeStream();
    useRpQualityStore.getState().offer(s);
    expect(useRpQualityStore.getState().offerFor).toBe(s);
    useRpQualityStore.getState().dismiss();
    useRpQualityStore.getState().offer(s);
    expect(useRpQualityStore.getState().offerFor).toBeNull();
    expect(wasOffered(s)).toBe(true);
  });

  it("restarts at 720p, waiting out the PS5's busy answer", async () => {
    let tries = 0;
    const start = vi.fn(async () => {
      tries++;
      if (tries === 1) {
        // The console still holds its slot: the engine says so and the stream ends.
        useRemotePlayStore.setState({ stream: null, phase: "ended", error: { code: "busy", retryS: 1, detail: "" } });
      } else {
        useRemotePlayStore.setState({ stream: fakeStream(), phase: "playing", error: null });
      }
    });
    const stop = vi.fn(() => useRemotePlayStore.setState({ stream: null, phase: "ended" }));
    useRemotePlayStore.setState({
      stream: fakeStream(),
      target,
      phase: "playing",
      error: null,
      prefs: { ...useRemotePlayStore.getState().prefs, res: "1920x1080" },
      start,
      stop,
    });
    await useRpQualityStore.getState().switchTo720();
    expect(stop).toHaveBeenCalledTimes(1);
    expect(start).toHaveBeenCalledTimes(2);
    expect(start).toHaveBeenLastCalledWith(target);
    const rp = useRemotePlayStore.getState();
    expect(rp.prefs.res).toBe("1280x720");
    expect(rp.phase).toBe("playing");
    // The busy answer was the plan, not an error to show.
    expect(rp.error).toBeNull();
    expect(useRpQualityStore.getState().switching).toBeNull();
  }, 10_000);

  it("stops trying when cancelled", async () => {
    const start = vi.fn(async () => {
      useRemotePlayStore.setState({ stream: null, phase: "ended", error: { code: "busy", retryS: 3, detail: "" } });
    });
    useRemotePlayStore.setState({
      stream: fakeStream(),
      target,
      phase: "playing",
      start,
      stop: () => useRemotePlayStore.setState({ stream: null, phase: "ended" }),
    });
    const run = useRpQualityStore.getState().switchTo720();
    await vi.waitFor(() => expect(useRpQualityStore.getState().switching?.retryAt).toBeTruthy());
    useRpQualityStore.getState().cancelSwitch();
    await run;
    expect(start).toHaveBeenCalledTimes(1);
  }, 10_000);
});
