import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { installWindowStub } from "../test-utils/storage";
import { bringUpPlaylistIdFor } from "../lib/playlistOps";
import {
  bringUpMigration,
  bringUpPlaylistIdForHost,
  consoleSettingFor,
  consoleSettingsById,
  defaultConsoleSettings,
  ensureConsoleSettingsMigrated,
  installPathFallbackOptionsFor,
  keepAwakeHosts,
  keepAwakeModesInUse,
  migrationFill,
  MIGRATED_KEY,
  redeployActionForProfile,
  setBringUpPlaylistFor,
  setConsoleSetting,
  settingsOfProfile,
  type ConsoleSettings,
} from "./consoleSettings";
import { useInstallPathFallbackStore } from "./installPathFallback";
import { usePayloadPlaylistsStore } from "./payloadPlaylists";
import { useRestAfterUploadStore } from "./restAfterUpload";
import { useRosterStore, type PS5Profile } from "./roster";
import { useUploadSettingsStore } from "./uploadSettings";

const DEFAULTS: ConsoleSettings = {
  wake_for_uploads: true,
  keep_awake_mode: "transfers",
  rest_after_upload: false,
  auto_bring_up: false,
  auto_redeploy_on_wake: true,
  install_path_fallback: false,
};

const profile = (id: string, host: string, settings?: PS5Profile["settings"]): PS5Profile => ({
  id,
  name: id,
  host,
  ...(settings ? { settings } : {}),
});

function setRoster(profiles: PS5Profile[], active: string | null = profiles[0]?.id ?? null) {
  useRosterStore.setState({ profiles, active_id: active });
}

/** Today's app-wide values (what a 12.x build reads as the default). */
function setDefaults(over: Partial<ConsoleSettings>) {
  const d = { ...DEFAULTS, ...over };
  useUploadSettingsStore.setState({
    wakeForUploads: d.wake_for_uploads,
    keepPs5AwakeMode: d.keep_awake_mode,
    autoBringUp: d.auto_bring_up,
    autoRedeployOnWake: d.auto_redeploy_on_wake,
  });
  useRestAfterUploadStore.setState({ enabled: d.rest_after_upload });
  useInstallPathFallbackStore.setState({ enabled: d.install_path_fallback });
}

let storage: Storage;
beforeEach(() => {
  storage = installWindowStub();
  setDefaults({});
  setRoster([]);
  usePayloadPlaylistsStore.setState({
    loaded: true,
    autoLoader: { enabled: false, playlistId: null, bringUpPlaylistId: null },
  });
});
afterEach(() => {
  vi.unstubAllGlobals();
});

describe("per-console settings: reading", () => {
  it("a console's own value wins over the default; what it lacks reads the default", () => {
    const s = settingsOfProfile({ settings: { rest_after_upload: true, keep_awake_mode: "always" } }, DEFAULTS);
    expect(s.rest_after_upload).toBe(true);
    expect(s.keep_awake_mode).toBe("always");
    expect(s.wake_for_uploads).toBe(DEFAULTS.wake_for_uploads);
  });

  it("ignores a value of the wrong shape (a synced profile may come from any build)", () => {
    const s = settingsOfProfile(
      { settings: { keep_awake_mode: "forever", rest_after_upload: "yes" } as unknown as PS5Profile["settings"] },
      DEFAULTS,
    );
    expect(s.keep_awake_mode).toBe("transfers");
    expect(s.rest_after_upload).toBe(false);
  });

  it("reads the console it acts on, by address (port-tolerant), not the selected one", () => {
    setRoster(
      [
        profile("a", "192.168.1.10", { rest_after_upload: false }),
        profile("b", "192.168.1.20", { rest_after_upload: true }),
      ],
      "a",
    );
    expect(consoleSettingFor("192.168.1.20:9113", "rest_after_upload")).toBe(true);
    expect(consoleSettingFor("192.168.1.10", "rest_after_upload")).toBe(false);
  });

  it("a console not in the roster reads the default", () => {
    setDefaults({ wake_for_uploads: false });
    expect(consoleSettingFor("10.0.0.9", "wake_for_uploads")).toBe(false);
  });

  it("each reader's console: install fallback options and keep-awake", () => {
    setRoster([
      profile("old", "192.168.1.10", { install_path_fallback: true, keep_awake_mode: "off" }),
      profile("new", "192.168.1.20", { install_path_fallback: false, keep_awake_mode: "always" }),
    ]);
    expect(installPathFallbackOptionsFor("192.168.1.10")).toEqual({ console_path_fallback: true });
    expect(installPathFallbackOptionsFor("192.168.1.20")).toEqual({});
    const mode = (h: string) => consoleSettingFor(h, "keep_awake_mode");
    // Busy on both, up on both: "off" is never ticked, "always" is ticked even when idle.
    expect([...keepAwakeHosts(["192.168.1.10", "192.168.1.20"], [], mode)]).toEqual(["192.168.1.20"]);
    expect([...keepAwakeHosts([], ["192.168.1.10", "192.168.1.20"], mode)]).toEqual(["192.168.1.20"]);
  });

  it("auto-redeploy and bring-up decide by each console's own switches", () => {
    const bringUpHere = profile("a", "192.168.1.10", { auto_bring_up: true, auto_redeploy_on_wake: true });
    const reconnectOnly = profile("b", "192.168.1.20", { auto_bring_up: false, auto_redeploy_on_wake: true });
    const neither = profile("c", "192.168.1.30", { auto_bring_up: false, auto_redeploy_on_wake: false });
    expect(redeployActionForProfile(bringUpHere, DEFAULTS, false)).toBe("bring-up");
    expect(redeployActionForProfile(bringUpHere, DEFAULTS, true)).toBe("helper");
    expect(redeployActionForProfile(reconnectOnly, DEFAULTS, false)).toBe("helper");
    expect(redeployActionForProfile(neither, DEFAULTS, false)).toBe("none");
    // No value of its own: the default (reconnect on, bring-up off).
    expect(redeployActionForProfile(profile("d", "h"), DEFAULTS, false)).toBe("helper");
  });

  it("keep-awake runs when any console wants it", () => {
    const off = { ...DEFAULTS, keep_awake_mode: "off" as const };
    expect(keepAwakeModesInUse([{ settings: { keep_awake_mode: "off" } }], off)).toEqual({ anyOn: false, anyAlways: false });
    expect(keepAwakeModesInUse([{ settings: { keep_awake_mode: "always" } }], off)).toEqual({ anyOn: true, anyAlways: true });
    expect(keepAwakeModesInUse([{ settings: {} }], DEFAULTS)).toEqual({ anyOn: true, anyAlways: false });
  });

  it("writes one console's value through the roster", () => {
    setRoster([profile("a", "192.168.1.10"), profile("b", "192.168.1.20")]);
    setConsoleSetting("b", "auto_bring_up", true);
    expect(consoleSettingsById("b").auto_bring_up).toBe(true);
    expect(consoleSettingsById("a").auto_bring_up).toBe(false);
    // Kept with the profile, so it is shared with the console list.
    expect(JSON.parse(storage.getItem("ps5upload.roster.v1")!).profiles[1].settings).toEqual({ auto_bring_up: true });
  });
});

describe("per-console settings: shared with the console", () => {
  it("travel with the profile: the engine's console list and the consoles' documents", async () => {
    const { sharedForm } = await import("./rosterShare");
    const { localView, rosterFrom, fieldKey } = await import("../lib/meshShare");
    const p = profile("mine", "192.168.1.10", { keep_awake_mode: "always" });
    expect(sharedForm([p])[0].settings).toEqual({ keep_awake_mode: "always" });
    // Mesh: one register per console (keyed by its address, as every console field is).
    const view = localView([p], () => null);
    expect(view.get(fieldKey("192.168.1.10", "settings"))).toEqual({ keep_awake_mode: "always" });
    // Another device's value for this console arrives on this device's profile (its id kept).
    const map = {
      [fieldKey("192.168.1.10", "settings")]: { v: { keep_awake_mode: "off" }, h: { t: 9, c: 0, d: "other" } },
    };
    const next = rosterFrom(map, [p]);
    expect(next[0].id).toBe("mine");
    expect(next[0].settings).toEqual({ keep_awake_mode: "off" });
  });
});

describe("per-console settings: the one-time migration", () => {
  it("copies today's app-wide values into every existing console", () => {
    setDefaults({ rest_after_upload: true, keep_awake_mode: "always", install_path_fallback: true });
    setRoster([profile("a", "192.168.1.10"), profile("b", "192.168.1.20")]);
    ensureConsoleSettingsMigrated();
    for (const id of ["a", "b"]) {
      const own = useRosterStore.getState().profiles.find((p) => p.id === id)!.settings;
      expect(own).toEqual({ ...DEFAULTS, rest_after_upload: true, keep_awake_mode: "always", install_path_fallback: true });
    }
    expect(storage.getItem(MIGRATED_KEY)).toBe("1");
  });

  it("keeps a value a console already has (set on another device and synced here)", () => {
    const fill = migrationFill([profile("a", "h", { wake_for_uploads: false })], DEFAULTS)!;
    expect(fill.a.wake_for_uploads).toBeUndefined();
    expect(fill.a.rest_after_upload).toBe(false);
    expect(migrationFill([profile("a", "h", DEFAULTS)], DEFAULTS)).toBeNull();
  });

  it("runs once: a later change of the default does not reach migrated consoles", () => {
    setRoster([profile("a", "192.168.1.10")]);
    ensureConsoleSettingsMigrated();
    setDefaults({ wake_for_uploads: false }); // an older build elsewhere edits the global key
    ensureConsoleSettingsMigrated();
    expect(consoleSettingFor("192.168.1.10", "wake_for_uploads")).toBe(true);
  });

  it("a console added later gets the default", () => {
    setDefaults({ rest_after_upload: true });
    setRoster([profile("a", "192.168.1.10")]);
    ensureConsoleSettingsMigrated();
    useRosterStore.getState().add({ name: "New", host: "192.168.1.30" });
    expect(consoleSettingFor("192.168.1.30", "rest_after_upload")).toBe(true);
    expect(defaultConsoleSettings().rest_after_upload).toBe(true);
  });

  it("gives every console the app-wide bring-up playlist, and new consoles fall back to it", () => {
    setRoster([profile("a", "192.168.1.10"), profile("b", "192.168.1.20")]);
    usePayloadPlaylistsStore.setState({
      loaded: true,
      autoLoader: { enabled: false, playlistId: null, bringUpPlaylistId: "kstuff" },
    });
    ensureConsoleSettingsMigrated();
    expect(usePayloadPlaylistsStore.getState().autoLoader.bringUpByConsole).toEqual({ a: "kstuff", b: "kstuff" });
    // Each console its own from now on (payloads depend on firmware).
    setBringUpPlaylistFor("b", "smp-1360");
    expect(bringUpPlaylistIdForHost("192.168.1.10")).toBe("kstuff");
    expect(bringUpPlaylistIdForHost("192.168.1.20:9021")).toBe("smp-1360");
    // A console not in the map (added later) reads the app-wide one.
    expect(bringUpPlaylistIdFor(usePayloadPlaylistsStore.getState().autoLoader, "later")).toBe("kstuff");
    // Run again: the map is kept.
    ensureConsoleSettingsMigrated();
    expect(bringUpPlaylistIdForHost("192.168.1.20")).toBe("smp-1360");
  });

  it("waits for the playlist document before copying the bring-up playlist", () => {
    setRoster([profile("a", "192.168.1.10")]);
    usePayloadPlaylistsStore.setState({
      loaded: false,
      autoLoader: { enabled: false, playlistId: null, bringUpPlaylistId: null },
    });
    ensureConsoleSettingsMigrated();
    expect(usePayloadPlaylistsStore.getState().autoLoader.bringUpByConsole).toBeUndefined();
    usePayloadPlaylistsStore.setState({
      loaded: true,
      autoLoader: { enabled: false, playlistId: null, bringUpPlaylistId: "pl" },
    });
    expect(usePayloadPlaylistsStore.getState().autoLoader.bringUpByConsole).toEqual({ a: "pl" });
  });

  it("an existing map is left alone", () => {
    expect(bringUpMigration({ bringUpPlaylistId: "x", bringUpByConsole: {} }, [{ id: "a" }])).toBeNull();
  });
});
