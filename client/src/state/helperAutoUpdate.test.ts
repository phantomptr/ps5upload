import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const api = vi.hoisted(() => ({
  payloadCheck: vi.fn(),
  portCheck: vi.fn(),
  savesOpen: vi.fn(),
  cheatsStatus: vi.fn(),
  fetchConsoleTasks: vi.fn(),
  ensurePayloadCurrent: vi.fn(),
  pollMesh: vi.fn(),
  enabled: { value: true },
}));

vi.mock("../api/ps5", async (orig) => ({
  ...(await orig<typeof import("../api/ps5")>()),
  payloadCheck: api.payloadCheck,
  portCheck: api.portCheck,
  savesOpen: api.savesOpen,
  cheatsStatus: api.cheatsStatus,
}));
vi.mock("../api/consoleTasks", async (orig) => ({
  ...(await orig<typeof import("../api/consoleTasks")>()),
  fetchConsoleTasks: api.fetchConsoleTasks,
}));
vi.mock("../lib/ensurePayloadCurrent", () => ({ ensurePayloadCurrent: api.ensurePayloadCurrent }));
vi.mock("../lib/appVersion", () => ({ getAppVersion: async () => "12.6.15" }));
vi.mock("./mesh", async (orig) => ({
  ...(await orig<typeof import("./mesh")>()),
  pollMesh: api.pollMesh,
}));
vi.mock("./consoleSettings", async (orig) => ({
  ...(await orig<typeof import("./consoleSettings")>()),
  consoleSettingFor: (_h: string, k: string) => (k === "auto_update_helper" ? api.enabled.value : true),
}));

import { EMPTY_HOST_RUNTIME, useConnectionStore } from "./connection";
import { evaluateAutoUpdate, noteHelperStatus, resetHelperAutoUpdate, useHelperAutoUpdateStore } from "./helperAutoUpdate";
import { useMeshStore } from "./mesh";
import { useTaskStore } from "./tasks";
import { useToastStore } from "./toasts";
import { useRemotePlayStore } from "./remotePlay";
import { resetHelperSendGate } from "../lib/helperSendGate";
import { helperAutoUpdateGate } from "../lib/helperAutoUpdate";

const HOST = "192.168.1.20";

function helperUp(version: string) {
  useConnectionStore.setState({
    runtimeByHost: { [HOST]: { ...EMPTY_HOST_RUNTIME, payloadStatus: "up", payloadVersion: version, power: "awake" } },
  });
  // Up for a minute already.
  noteHelperStatus(Date.now() - 60_000);
}

beforeEach(() => {
  // These tests exercise the rules, so the feature is switched on here; see the last test for
  // the shipped default.
  helperAutoUpdateGate.available = true;
  resetHelperAutoUpdate();
  resetHelperSendGate();
  api.enabled.value = true;
  api.payloadCheck.mockResolvedValue({ reachable: true, payloadVersion: "12.6.13", engineReachable: true });
  api.portCheck.mockResolvedValue(true);
  api.savesOpen.mockResolvedValue({ mounted: false, image: "", mount_point: "", kind: "" });
  api.cheatsStatus.mockResolvedValue({ enabled: true, game_running: false });
  api.fetchConsoleTasks.mockResolvedValue({ supported: true, tasks: [] });
  api.ensurePayloadCurrent.mockResolvedValue("pushed");
  // A helper and engine without Mesh: nobody else can be there.
  api.pollMesh.mockImplementation(async (h: string) => {
    useMeshStore.getState().put(h, { supported: false, state: null, error: null, at: Date.now() });
  });
  useToastStore.setState({ toasts: [] });
  useTaskStore.setState({ tasks: [] });
  useRemotePlayStore.setState({ stream: null, target: null });
  helperUp("12.6.13");
});

afterEach(() => {
  helperAutoUpdateGate.available = false;
  vi.clearAllMocks();
});

describe("automatic helper updates in this build (#903)", () => {
  it("are off: an older helper is left alone and nothing is sent", async () => {
    helperAutoUpdateGate.available = false;
    await evaluateAutoUpdate(HOST);
    expect(api.payloadCheck).not.toHaveBeenCalled();
    expect(api.ensurePayloadCurrent).not.toHaveBeenCalled();
    // Nor is a line shown on the console card for a setting that can't be turned on.
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toBeUndefined();
  });
});

describe("the automatic update of an older helper", () => {
  it("sends this app's helper when nothing runs, and says so once", async () => {
    await evaluateAutoUpdate(HOST);
    expect(api.ensurePayloadCurrent).toHaveBeenCalledOnce();
    // Unattended: "auto", so a "stopped by you" mark and every hold in ensurePayloadCurrent apply.
    expect(api.ensurePayloadCurrent.mock.calls[0][3]).toBe("auto");
    expect(useToastStore.getState().toasts.map((t) => t.message)).toEqual(["PS5 helper updated to v12.6.15"]);
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toBeUndefined();
  });

  it("never asks the console, nor sends, while this app has work there", async () => {
    useRemotePlayStore.setState({
      stream: {} as never,
      target: { host: HOST, name: "PS5", registKey: "", rpKey: "" },
    });
    await evaluateAutoUpdate(HOST);
    expect(api.payloadCheck).not.toHaveBeenCalled();
    expect(api.ensurePayloadCurrent).not.toHaveBeenCalled();
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toMatchObject({ state: "waiting", why: "busy-here" });
  });

  it("waits for an open save on the console", async () => {
    api.savesOpen.mockResolvedValue({ mounted: true, image: "/user/home/x/sdimg", mount_point: "/mnt/sandbox", kind: "ps5" });
    await evaluateAutoUpdate(HOST);
    expect(api.ensurePayloadCurrent).not.toHaveBeenCalled();
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toMatchObject({ state: "waiting", why: "console-work" });
  });

  it("an older helper that has no save feature is not held for it", async () => {
    api.savesOpen.mockRejectedValue(new Error("save_mount: unknown method"));
    await evaluateAutoUpdate(HOST);
    expect(api.ensurePayloadCurrent).toHaveBeenCalledOnce();
  });

  it("an older helper with no task list is waited on, not replaced on a guess", async () => {
    api.fetchConsoleTasks.mockResolvedValue({ supported: false, tasks: [] });
    await evaluateAutoUpdate(HOST);
    expect(api.ensurePayloadCurrent).not.toHaveBeenCalled();
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toMatchObject({ why: "console-unknown" });
  });

  it("a probe that fails right before is no reason to send", async () => {
    api.payloadCheck.mockResolvedValue({ reachable: false, payloadVersion: null, engineReachable: true });
    await evaluateAutoUpdate(HOST);
    expect(api.ensurePayloadCurrent).not.toHaveBeenCalled();
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toMatchObject({ why: "not-answering" });
  });

  it("with the loader closed, waits and says why", async () => {
    api.portCheck.mockResolvedValue(false);
    await evaluateAutoUpdate(HOST);
    expect(api.ensurePayloadCurrent).not.toHaveBeenCalled();
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toMatchObject({ why: "loader-closed" });
  });

  it("never touches a newer or matching helper", async () => {
    helperUp("12.6.16");
    await evaluateAutoUpdate(HOST);
    helperUp("12.6.15");
    await evaluateAutoUpdate(HOST);
    expect(api.payloadCheck).not.toHaveBeenCalled();
    expect(api.ensurePayloadCurrent).not.toHaveBeenCalled();
  });

  it("does nothing with the setting off, and the card says so", async () => {
    api.enabled.value = false;
    await evaluateAutoUpdate(HOST);
    expect(api.payloadCheck).not.toHaveBeenCalled();
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toMatchObject({ state: "never", why: "off" });
  });

  it("two evaluations at once send one helper", async () => {
    await Promise.all([evaluateAutoUpdate(HOST), evaluateAutoUpdate(HOST)]);
    expect(api.ensurePayloadCurrent).toHaveBeenCalledOnce();
  });

  it("after a try that did not work, waits minutes before the next, quietly", async () => {
    api.ensurePayloadCurrent.mockResolvedValue("no-push");
    await evaluateAutoUpdate(HOST);
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toMatchObject({ state: "failed" });
    await evaluateAutoUpdate(HOST);
    expect(api.ensurePayloadCurrent).toHaveBeenCalledOnce();
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toMatchObject({ why: "retry-later" });
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it("another device updated it meanwhile: nothing to say", async () => {
    api.ensurePayloadCurrent.mockResolvedValue("current");
    await evaluateAutoUpdate(HOST);
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toBeUndefined();
  });

  it("waits for other devices' work and a Mesh Drive pass", async () => {
    api.pollMesh.mockImplementation(async (h: string) => {
      useMeshStore.getState().put(h, {
        supported: true,
        error: null,
        at: Date.now(),
        state: {
          rev: 1,
          now: 1_000_000,
          you: "me",
          helper: "12.6.13",
          settings: {} as never,
          devices: [],
          locks: [{ name: "drive:abc", label: "Mesh Drive", by: "other", until: 1_030_000 }],
          events: [],
          ev: 0,
        },
      });
    });
    await evaluateAutoUpdate(HOST);
    expect(api.ensurePayloadCurrent).not.toHaveBeenCalled();
    expect(useHelperAutoUpdateStore.getState().byHost[HOST]).toMatchObject({ why: "busy-others" });
  });
});
