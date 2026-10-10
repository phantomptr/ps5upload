import { beforeEach, describe, expect, it, vi } from "vitest";

// The helper goes through the one shared send (sendHelperAndWait → sendHelperTo), never a raw
// sendPayload: that path joins a send under way and opens pairing for an unpaired console.
const pending: Array<(r: string | null) => void> = [];
const releaseSend = (r: string | null = null) => pending.splice(0).forEach((f) => f(r));
const sendHelperAndWait = vi.fn(
  (_host: string, _tr: unknown) => new Promise<string | null>((r) => void pending.push(r)),
);
vi.mock("./helperSendRuntime", () => ({
  sendHelperAndWait: (host: string, tr: unknown) => sendHelperAndWait(host, tr),
}));
const { sendPayload } = vi.hoisted(() => ({ sendPayload: vi.fn() }));
vi.mock("../api/ps5", () => ({ sendPayload }));

import { bringUpStatusFor, useBringUpStore } from "./bringUp";
import type { Translator } from "./lang";

const tr = ((key: string) => key) as Translator;

describe("bring-up status is per console", () => {
  beforeEach(() => {
    useBringUpStore.setState({ byHost: {} });
    sendHelperAndWait.mockClear();
  });

  it("a run on A leaves B idle and free to run", async () => {
    const a = useBringUpStore.getState().run("192.168.0.5", tr);
    await Promise.resolve();
    const s = useBringUpStore.getState();
    expect(bringUpStatusFor(s, "192.168.0.5").kind).toBe("running");
    expect(bringUpStatusFor(s, "192.168.0.6").kind).toBe("idle");

    const b = useBringUpStore.getState().run("192.168.0.6", tr);
    await Promise.resolve();
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.6").kind).toBe("running");
    releaseSend();
    await Promise.all([a, b]);
  });

  it("a second run on the same console while one runs is ignored", async () => {
    const first = useBringUpStore.getState().run("192.168.0.5:9021", tr);
    await Promise.resolve();
    await useBringUpStore.getState().run("192.168.0.5", tr);
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.5").kind).toBe("running");
    releaseSend();
    await first;
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.5").kind).toBe("done");
  });
});

describe("bring-up sends the helper through the shared send", () => {
  beforeEach(() => {
    useBringUpStore.setState({ byHost: {} });
    sendHelperAndWait.mockClear();
  });

  it("uses sendHelperAndWait with the user's translator, not a raw sendPayload", async () => {
    const run = useBringUpStore.getState().run("192.168.0.7", tr);
    await Promise.resolve();
    releaseSend();
    await run;
    expect(sendHelperAndWait).toHaveBeenCalledWith("192.168.0.7", tr);
    expect(sendPayload).not.toHaveBeenCalled();
  });

  it("fails at the helper step with what the send said (e.g. pairing needed)", async () => {
    const run = useBringUpStore.getState().run("192.168.0.8", tr);
    await Promise.resolve();
    releaseSend("Pair with your PS5");
    await run;
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.8")).toEqual({
      kind: "failed",
      host: "192.168.0.8",
      phase: "helper",
      error: "Pair with your PS5",
    });
  });
});
