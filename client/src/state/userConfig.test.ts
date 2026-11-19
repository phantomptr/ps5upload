import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("../lib/invokeLogged", () => ({ invoke }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

import { useConnectionStore } from "./connection";
import { useRosterStore } from "./roster";
import { useUploadSettingsStore } from "./uploadSettings";
import { hydrateFromUserConfig, installUserConfigMirror } from "./userConfig";

const saves = () => invoke.mock.calls.filter(([c]) => c === "user_config_save");

describe("the file mirror", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invoke.mockReset();
    invoke.mockResolvedValue(undefined);
    installUserConfigMirror();
  });
  afterEach(() => vi.useRealTimers());

  it("writes when the host changes, not on every status change, and not the same file twice", async () => {
    useConnectionStore.setState({ host: "10.0.0.7" });
    await vi.advanceTimersByTimeAsync(400);
    expect(saves()).toHaveLength(1);

    // What a poll does: status moves, nothing the file holds does.
    useConnectionStore.setState({ status: "connected" } as never);
    await vi.advanceTimersByTimeAsync(400);
    expect(saves()).toHaveLength(1);

    // A store that changed without touching what is written.
    useConnectionStore.setState({ host: "10.0.0.8" });
    useConnectionStore.setState({ host: "10.0.0.7" });
    await vi.advanceTimersByTimeAsync(400);
    expect(saves()).toHaveLength(1);
  });
});

describe("hydrating the selected console", () => {
  beforeEach(() => {
    invoke.mockReset();
    useConnectionStore.setState({ host: "10.0.0.1" });
    useRosterStore.setState({ profiles: [], active_id: null });
  });

  const fileSays = (ps5_host: string) =>
    invoke.mockImplementation(async (cmd: string) => (cmd === "user_config_load" ? { ps5_host } : undefined));

  it("leaves the roster and the host alone when a roster exists", async () => {
    useRosterStore.setState({
      profiles: [{ id: "a", host: "10.0.0.1", name: "A" }] as never,
      active_id: "a",
    });
    fileSays("10.0.0.9");
    await hydrateFromUserConfig();
    expect(useConnectionStore.getState().host).toBe("10.0.0.1");
    expect(useRosterStore.getState().profiles).toHaveLength(1);
    expect(useRosterStore.getState().profiles[0].host).toBe("10.0.0.1");
  });

  it("makes the file's console the selected one when there is no roster yet", async () => {
    fileSays("10.0.0.9");
    await hydrateFromUserConfig();
    expect(useRosterStore.getState().profiles.map((p) => p.host)).toEqual(["10.0.0.9"]);
    expect(useConnectionStore.getState().host).toBe("10.0.0.9");
  });
});

describe("hydrating the upload speed limit", () => {
  let stored: Map<string, string>;
  beforeEach(() => {
    invoke.mockReset();
    stored = new Map();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (k: string) => stored.get(k) ?? null,
        setItem: (k: string, v: string) => void stored.set(k, v),
        removeItem: (k: string) => void stored.delete(k),
      },
    });
    useUploadSettingsStore.setState({ bandwidthCapMbps: 0 });
  });
  afterEach(() => vi.unstubAllGlobals());

  const fileSays = (bandwidth_cap_mbps: number) =>
    invoke.mockImplementation(async (cmd: string) =>
      cmd === "user_config_load" ? { upload: { bandwidth_cap_mbps } } : undefined,
    );

  it("keeps the limit the user chose when the file holds an older one", async () => {
    useUploadSettingsStore.getState().setBandwidthCapMbps(5);
    fileSays(20);
    await hydrateFromUserConfig();
    expect(useUploadSettingsStore.getState().bandwidthCapMbps).toBe(5);
  });

  it("keeps 'no limit' when the user cleared it and the file still holds the old one", async () => {
    useUploadSettingsStore.getState().setBandwidthCapMbps(5);
    useUploadSettingsStore.getState().setBandwidthCapMbps(0);
    fileSays(5);
    await hydrateFromUserConfig();
    expect(useUploadSettingsStore.getState().bandwidthCapMbps).toBe(0);
  });

  it("fills the limit from the file when this app has never stored one", async () => {
    fileSays(12);
    await hydrateFromUserConfig();
    expect(useUploadSettingsStore.getState().bandwidthCapMbps).toBe(12);
  });
});
