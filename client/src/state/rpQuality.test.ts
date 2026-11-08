import { beforeEach, describe, expect, it, vi } from "vitest";

import type { RemotePlayStream } from "../lib/remotePlayStream";
import { useRemotePlayStore } from "./remotePlay";
import { useRpQualityStore, wasOffered } from "./rpQuality";

const target = { host: "192.168.1.100", name: "Living room", registKey: "00", rpKey: "00" };
const fakeStream = () => ({ setVolume() {}, setAudioBuffer() {}, setFit() {}, setReplay() {}, setPicture() {} }) as unknown as RemotePlayStream;

describe("a restart with other settings", () => {
  beforeEach(() => {
    useRpQualityStore.setState({ offerFor: null, switching: null });
  });

  it("leaves Play Together open: friends keep their places through it", async () => {
    const userStop = vi.fn();
    const restartStop = vi.fn(() => useRemotePlayStore.setState({ stream: null, phase: "ended" }));
    const start = vi.fn(async () => {
      useRemotePlayStore.setState({ stream: fakeStream(), phase: "playing", error: null });
    });
    useRemotePlayStore.setState({
      stream: fakeStream(),
      target,
      phase: "playing",
      error: null,
      start,
      stop: userStop,
      stopForRestart: restartStop,
    });
    await useRpQualityStore.getState().restartWith({ codec: "h264" });
    expect(restartStop).toHaveBeenCalledTimes(1);
    expect(userStop).not.toHaveBeenCalled();
    expect(start).toHaveBeenCalledTimes(1);
  }, 10_000);

  it("a plain Start answered busy waits it out with a countdown, and starts", async () => {
    const start = vi.fn(async () => {
      useRemotePlayStore.setState({ stream: fakeStream(), phase: "playing", error: null });
    });
    useRemotePlayStore.setState({
      stream: null,
      target,
      phase: "ended",
      error: { code: "busy", retryS: 1, detail: "" },
      start,
    });
    const run = useRpQualityStore.getState().startWhenFree(target);
    // No error to read: the wait, with its countdown and Cancel.
    expect(useRemotePlayStore.getState().error).toBeNull();
    expect(useRpQualityStore.getState().switching?.kind).toBe("wait");
    expect(useRpQualityStore.getState().switching?.retryAt).toBeTruthy();
    await run;
    expect(start).toHaveBeenCalledTimes(1);
    expect(useRpQualityStore.getState().switching).toBeNull();
  }, 10_000);

  it("a PS5 holding its last session is waited out again and again, then said (#720)", async () => {
    let tries = 0;
    const start = vi.fn(async () => {
      tries++;
      // The engine: busy while it spreads its tries out, then held.
      const code = tries < 3 ? "busy" : "held";
      useRemotePlayStore.setState({ stream: null, phase: "ended", error: { code, retryS: code === "busy" ? 0 : undefined, detail: "" } });
    });
    useRemotePlayStore.setState({ stream: null, target, phase: "ended", error: { code: "busy", retryS: 0, detail: "" }, start });
    await useRpQualityStore.getState().startWhenFree(target);
    expect(start).toHaveBeenCalledTimes(3);
    // Held is shown (with Start), not waited out for ever.
    expect(useRemotePlayStore.getState().error?.code).toBe("held");
    expect(useRpQualityStore.getState().switching).toBeNull();
  }, 15_000);

  it("another device using the console is not waited out by a plain Start", async () => {
    const start = vi.fn(async () => {
      useRemotePlayStore.setState({ stream: null, phase: "ended", error: { code: "in_use", retryS: 30, detail: "" } });
    });
    useRemotePlayStore.setState({ stream: null, target, phase: "ended", error: { code: "busy", retryS: 0, detail: "" }, start });
    await useRpQualityStore.getState().startWhenFree(target);
    expect(start).toHaveBeenCalledTimes(1);
    expect(useRemotePlayStore.getState().error?.code).toBe("in_use");
    expect(useRpQualityStore.getState().switching).toBeNull();
  }, 10_000);
});

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
      stopForRestart: stop,
    });
    await useRpQualityStore.getState().switchTo720();
    expect(stop).toHaveBeenCalledTimes(1);
    expect(start).toHaveBeenCalledTimes(2);
    // For this session only: the setting stays what the user chose.
    expect(start).toHaveBeenLastCalledWith(target, { quality: { res: "1280x720" } });
    const rp = useRemotePlayStore.getState();
    expect(rp.prefs.res).toBe("1920x1080");
    expect(rp.phase).toBe("playing");
    // The busy answer was the plan, not an error to show.
    expect(rp.error).toBeNull();
    expect(useRpQualityStore.getState().switching).toBeNull();
  }, 10_000);

  it("adds to what this session already changed, and never saves it", async () => {
    const start = vi.fn(async () => {
      useRemotePlayStore.setState({ stream: fakeStream(), phase: "playing", error: null });
    });
    const stop = vi.fn(() => useRemotePlayStore.setState({ stream: null, phase: "ended" }));
    const prefs = { ...useRemotePlayStore.getState().prefs, res: "1920x1080" as const, codec: "auto" as const, bitrateKbps: 0 };
    useRemotePlayStore.setState({
      stream: fakeStream(),
      target,
      phase: "playing",
      error: null,
      prefs,
      sessionQuality: { res: "1280x720" },
      start,
      stop,
    });
    await useRpQualityStore.getState().restartWith({ codec: "h264", bitrateKbps: 6000 });
    expect(start).toHaveBeenCalledWith(target, { quality: { res: "1280x720", codec: "h264", bitrateKbps: 6000 } });
    expect(useRemotePlayStore.getState().prefs).toEqual(prefs);
  }, 10_000);

  it("a picture turned back on stays on through a later restart", async () => {
    const start = vi.fn(async () => {
      useRemotePlayStore.setState({ stream: fakeStream(), phase: "playing", error: null });
    });
    const prefs = { ...useRemotePlayStore.getState().prefs, pictureHere: true };
    useRemotePlayStore.setState({
      stream: fakeStream(),
      target,
      phase: "playing",
      error: null,
      prefs,
      pictureHere: true,
      sessionQuality: { codec: "h264", pictureHere: false },
      start,
      stopForRestart: () => useRemotePlayStore.setState({ stream: null, phase: "ended" }),
    });
    await useRpQualityStore.getState().restartWith({ res: "1280x720" });
    expect(start).toHaveBeenCalledWith(target, { quality: { codec: "h264", res: "1280x720" } });
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
      stopForRestart: () => useRemotePlayStore.setState({ stream: null, phase: "ended" }),
    });
    const run = useRpQualityStore.getState().switchTo720();
    await vi.waitFor(() => expect(useRpQualityStore.getState().switching?.retryAt).toBeTruthy());
    useRpQualityStore.getState().cancelSwitch();
    await run;
    expect(start).toHaveBeenCalledTimes(1);
  }, 10_000);
});
