import { afterEach, describe, expect, it, vi } from "vitest";
import { cameraRefused, okToJoin } from "./JoinAFriend";
import { useRemotePlayStore } from "../../state/remotePlay";
import { useNotificationsStore } from "../../state/notifications";
import type { JoinLink } from "../../lib/joinProtocol";

describe("okToJoin with Remote Play running", () => {
  const tr = (_k: string, _v: unknown, fallback?: string) => fallback ?? _k;
  const link = { origin: "http://192.168.1.20:19113", name: "Sam" } as unknown as JoinLink;
  const before = useRemotePlayStore.getState();
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
    useRemotePlayStore.setState({ stream: before.stream, phase: before.phase, target: before.target, stop: before.stop });
  });
  const streaming = (stop: () => void) =>
    useRemotePlayStore.setState({
      stream: {} as never,
      phase: "streaming" as never,
      target: { name: "Pro", host: "10.0.0.2" } as never,
      stop,
    });

  it("goes ahead once the stream has stopped", async () => {
    streaming(() => useRemotePlayStore.setState({ stream: null, phase: "ended" as never }));
    await expect(okToJoin(tr as never, async () => true, link)).resolves.toBe(true);
  });

  it("does not join while the stream is still stopping, and says so", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("window", { setTimeout: (f: () => void, ms: number) => setTimeout(f, ms) });
    streaming(() => {});
    const n = useNotificationsStore.getState().entries.length;
    const answer = okToJoin(tr as never, async () => true, link);
    await vi.advanceTimersByTimeAsync(6000);
    await expect(answer).resolves.toBe(false);
    const entries = useNotificationsStore.getState().entries;
    expect(entries.length).toBe(n + 1);
    expect(entries.some((e) => e.title.startsWith("Remote Play is still stopping"))).toBe(true);
  });
});

describe("cameraRefused", () => {
  it("is a refused permission for getUserMedia's permission errors", () => {
    expect(cameraRefused(new DOMException("denied", "NotAllowedError"))).toBe(true);
    expect(cameraRefused(new DOMException("insecure", "SecurityError"))).toBe(true);
    expect(cameraRefused({ name: "PermissionDeniedError" })).toBe(true);
  });

  it("is not for a missing or busy camera, or anything else", () => {
    expect(cameraRefused(new DOMException("none", "NotFoundError"))).toBe(false);
    expect(cameraRefused(new DOMException("busy", "NotReadableError"))).toBe(false);
    expect(cameraRefused(new Error("boom"))).toBe(false);
    expect(cameraRefused(null)).toBe(false);
    expect(cameraRefused(undefined)).toBe(false);
  });
});
