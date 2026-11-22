import { beforeEach, describe, expect, it, vi } from "vitest";

const { sendPayloadChecked, sendConsolePayload } = vi.hoisted(() => ({
  sendPayloadChecked: vi.fn(async () => ({ viaPayloadManager: false })),
  sendConsolePayload: vi.fn(async () => ({ viaPayloadManager: false })),
}));
vi.mock("../api/ps5", () => ({
  sendPayloadChecked,
  sendConsolePayload,
  PayloadSendRefused: class extends Error {},
  payloadPlaylistsLoad: vi.fn(async () => ({})),
  payloadPlaylistsSave: vi.fn(async () => {}),
  payloadsLocalInventory: vi.fn(async () => []),
}));
const store = vi.hoisted(() => new Map<string, string>());
vi.mock("../lib/safeStorage", () => ({
  safeGetItem: (k: string) => store.get(k) ?? null,
  safeSetItem: (k: string, v: string) => void store.set(k, v),
}));
vi.mock("../lib/materialize", () => ({
  withLocalCopy: (path: string, work: (local: string) => Promise<unknown>) => work(path),
}));
vi.mock("./connections", () => {
  const state = {
    loaded: true,
    connections: [{ id: "nas-1a2b", name: "NAS" }],
    load: async () => {},
  };
  return { useConnectionsStore: { getState: () => state } };
});

import { loadKernelSends, rememberKernelSend, type AutoRunContext } from "../lib/autoLoaderGuard";
import type { Playlist, PlaylistStep } from "../lib/playlistOps";
import { runStatusForHost, usePayloadPlaylistsStore } from "./payloadPlaylists";

const HOST = "10.0.0.7";
const playlist = (steps: PlaylistStep[], continueOnFailure = true): Playlist => ({
  id: "p1",
  name: "boot",
  steps,
  continueOnFailure,
  createdAt: 0,
  updatedAt: 0,
});
const step = (path: string): PlaylistStep => ({ path, sleepMs: 0 });
const auto = (c: Partial<AutoRunContext> = {}): AutoRunContext => ({
  running: [],
  bootMs: Date.now() - 3_600_000,
  stayedSince: () => null,
  ...c,
});

describe("the playlist runner", () => {
  beforeEach(() => {
    sendPayloadChecked.mockClear();
    sendConsolePayload.mockClear();
    store.clear();
    usePayloadPlaylistsStore.setState({ runStatusByHost: {} });
  });

  it("sends a step stored on the PS5 through the engine, and others as files (#900)", async () => {
    usePayloadPlaylistsStore.setState({
      playlists: [playlist([step("console:/data/elf/ftpsrv.elf"), step("remote://nas-1a2b/elf/hush.elf")])],
    });
    await usePayloadPlaylistsStore.getState().run("p1", HOST, 9021);
    expect(sendConsolePayload).toHaveBeenCalledWith(HOST, "/data/elf/ftpsrv.elf", 9021);
    expect(sendPayloadChecked).toHaveBeenCalledWith(HOST, "remote://nas-1a2b/elf/hush.elf", 9021);
    expect(runStatusForHost(usePayloadPlaylistsStore.getState(), HOST)).toMatchObject({
      kind: "done",
      successCount: 2,
      failureCount: 0,
    });
  });

  it("fails a step on a share this device doesn't have, and goes on per the playlist", async () => {
    usePayloadPlaylistsStore.setState({
      playlists: [playlist([step("remote://other-9f9f/elf/hush.elf"), step("/elf/ftpsrv.elf")])],
    });
    await usePayloadPlaylistsStore.getState().run("p1", HOST, 9021);
    const s = runStatusForHost(usePayloadPlaylistsStore.getState(), HOST);
    expect(s).toMatchObject({ kind: "done", successCount: 1, failureCount: 1 });
    expect(s.kind === "done" && s.failures[0].error).toMatch(/hush\.elf isn't saved on this device/);
    expect(sendPayloadChecked).toHaveBeenCalledTimes(1);
  });

  it("an automatic run sends kstuff once per console boot, and a person's Run still sends it", async () => {
    usePayloadPlaylistsStore.setState({
      playlists: [playlist([step("/elf/kstuff.elf"), step("/elf/ftpsrv.elf")])],
    });
    const store = usePayloadPlaylistsStore.getState();
    // The first automatic run: kstuff goes and is remembered.
    await store.run("p1", HOST, 9021, { auto: auto({ running: ["payload.elf"] }) });
    expect(sendPayloadChecked).toHaveBeenCalledTimes(2);
    expect(loadKernelSends(HOST).kstuff).toBeTypeOf("number");

    // The helper comes back: kstuff runs as `payload.elf`, so the list can't show it. Not again.
    sendPayloadChecked.mockClear();
    await store.run("p1", HOST, 9021, { auto: auto({ running: ["payload.elf"] }) });
    expect(sendPayloadChecked).toHaveBeenCalledTimes(1);
    expect(sendPayloadChecked).toHaveBeenCalledWith(HOST, "/elf/ftpsrv.elf", 9021);

    // Run, by a person: everything goes.
    sendPayloadChecked.mockClear();
    await store.run("p1", HOST, 9021);
    expect(sendPayloadChecked).toHaveBeenCalledTimes(2);
  });

  it("an automatic run sends kstuff again after the console restarted", async () => {
    rememberKernelSend(HOST, "kstuff", Date.now() - 2 * 3_600_000);
    usePayloadPlaylistsStore.setState({ playlists: [playlist([step("/elf/kstuff.elf")])] });
    await usePayloadPlaylistsStore.getState().run("p1", HOST, 9021, { auto: auto() });
    expect(sendPayloadChecked).toHaveBeenCalledWith(HOST, "/elf/kstuff.elf", 9021);
  });

  it("an automatic run skips a payload the process list shows", async () => {
    usePayloadPlaylistsStore.setState({
      playlists: [playlist([step("console:/data/shadowmountplus.elf"), step("/elf/ftpsrv.elf")])],
    });
    await usePayloadPlaylistsStore
      .getState()
      .run("p1", HOST, 9021, { auto: auto({ running: ["shadowmountplus.elf", "ftpsrv.elf"] }) });
    expect(sendConsolePayload).not.toHaveBeenCalled();
    expect(sendPayloadChecked).not.toHaveBeenCalled();
  });

  it("a step sent to another console is not judged by this console's process list or boot", async () => {
    const OTHER = "10.0.0.8";
    // Sent to this console in its boot: it stays out here, but the other console gets its own.
    rememberKernelSend(HOST, "kstuff", Date.now() - 60_000);
    usePayloadPlaylistsStore.setState({
      playlists: [
        playlist([
          { path: "/elf/ftpsrv.elf", sleepMs: 0, ip: OTHER },
          { path: "/elf/kstuff.elf", sleepMs: 0, ip: OTHER },
          step("/elf/kstuff.elf"),
        ]),
      ],
    });
    await usePayloadPlaylistsStore
      .getState()
      .run("p1", HOST, 9021, { auto: auto({ running: ["ftpsrv.elf", "payload.elf"] }) });
    expect(sendPayloadChecked.mock.calls).toEqual([
      [OTHER, "/elf/ftpsrv.elf", 9021],
      [OTHER, "/elf/kstuff.elf", 9021],
    ]);
  });
});
