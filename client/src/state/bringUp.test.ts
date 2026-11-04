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
vi.mock("../api/ps5", () => ({
  sendPayload,
  // A playlist step is a send the user set up: it goes through the checked one.
  sendPayloadChecked: sendPayload,
  PayloadSendRefused: class extends Error {},
  payloadPlaylistsSave: vi.fn(async () => {}),
}));
// A playlist step on a local path sends that path (no saved-server copy).
vi.mock("../lib/materialize", () => ({
  withLocalCopy: (path: string, work: (local: string) => Promise<unknown>) => work(path),
}));

import { bringUpStatusFor, useBringUpStore } from "./bringUp";
import type { Translator } from "./lang";
import { usePayloadPlaylistsStore } from "./payloadPlaylists";

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

describe("bring-up waits for its pre-helper playlist to really finish", () => {
  const playlist = {
    id: "pl-1",
    name: "kstuff + SMP",
    steps: [{ path: "/payloads/kstuff.elf", sleepMs: 0 }],
    continueOnFailure: false,
    createdAt: 1,
    updatedAt: 1,
  };
  beforeEach(() => {
    useBringUpStore.setState({ byHost: {} });
    sendHelperAndWait.mockClear();
    sendPayload.mockReset();
    usePayloadPlaylistsStore.setState({
      playlists: [playlist],
      autoLoader: { enabled: false, playlistId: null, bringUpPlaylistId: "pl-1" },
      runStatusByHost: {},
    });
  });

  it("sends the helper once the playlist is done", async () => {
    sendPayload.mockResolvedValue({ ok: true });
    const run = useBringUpStore.getState().run("192.168.0.9", tr);
    await vi.waitFor(() => expect(sendHelperAndWait).toHaveBeenCalled());
    releaseSend();
    await run;
    expect(sendPayload).toHaveBeenCalledTimes(1);
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.9").kind).toBe("done");
  });

  it("fails on a failed step and never sends the helper", async () => {
    sendPayload.mockRejectedValue(new Error("connect :9021: Connection refused"));
    await useBringUpStore.getState().run("192.168.0.9", tr);
    expect(sendHelperAndWait).not.toHaveBeenCalled();
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.9")).toMatchObject({
      kind: "failed",
      phase: "prehelper",
    });
  });

  it("a playlist stopped mid-run fails the bring-up instead of going on to the helper", async () => {
    // The step's send hangs; Stop on the playlist leaves the run status idle,
    // which used to read as "no failure" and the helper went out anyway.
    let finishSend: (v: unknown) => void = () => {};
    sendPayload.mockImplementation(() => new Promise((r) => (finishSend = r)));
    const run = useBringUpStore.getState().run("192.168.0.9", tr);
    await vi.waitFor(() => expect(sendPayload).toHaveBeenCalled());
    usePayloadPlaylistsStore.getState().stop("192.168.0.9");
    finishSend({ ok: true });
    await run;
    expect(sendHelperAndWait).not.toHaveBeenCalled();
    expect(bringUpStatusFor(useBringUpStore.getState(), "192.168.0.9")).toMatchObject({
      kind: "failed",
      phase: "prehelper",
    });
  });
});
