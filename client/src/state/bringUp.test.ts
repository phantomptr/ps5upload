import { beforeEach, describe, expect, it, vi } from "vitest";

// Bring-up on console A must not show on, or block, console B's Connection screen.
const pending: Array<() => void> = [];
const releaseSend = () => pending.splice(0).forEach((r) => r());
vi.mock("../api/ps5", () => ({
  bundledPayloadPath: vi.fn(async () => "/tmp/ps5upload.elf"),
  sendPayload: vi.fn(() => new Promise<void>((r) => void pending.push(r))),
  payloadCheck: vi.fn(async () => ({ reachable: true })),
}));

import { bringUpStatusFor, useBringUpStore } from "./bringUp";

describe("bring-up status is per console", () => {
  beforeEach(() => useBringUpStore.setState({ byHost: {} }));

  it("a run on A leaves B idle and free to run", async () => {
    const a = useBringUpStore.getState().run("192.168.0.5");
    await Promise.resolve();
    const s = useBringUpStore.getState();
    expect(bringUpStatusFor(s, "192.168.0.5").kind).toBe("running");
    expect(bringUpStatusFor(s, "192.168.0.6").kind).toBe("idle");

    const b = useBringUpStore.getState().run("192.168.0.6");
    await Promise.resolve();
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.6").kind).toBe("running");
    releaseSend();
    await Promise.all([a, b]);
  });

  it("a second run on the same console while one runs is ignored", async () => {
    const first = useBringUpStore.getState().run("192.168.0.5:9021");
    await Promise.resolve();
    await useBringUpStore.getState().run("192.168.0.5");
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.5").kind).toBe("running");
    releaseSend();
    await first;
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.5").kind).toBe("done");
  });
});
