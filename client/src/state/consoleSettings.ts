/**
 * Settings that belong to one PS5 (Settings › This PS5).
 *
 * Before 12.x these were one value for every console: waking for uploads, keeping the PS5 awake,
 * resting after uploads, bringing it up by itself, reconnecting after rest mode, and the
 * install-by-path fallback. Two consoles on different firmware want different answers, so each
 * console keeps its own now, inside its roster profile (`PS5Profile.settings`). The profile is
 * keyed by its id (the console's stable identity on this device), and it is what the engine's
 * console list and every device that uses the console already share (rosterShare,
 * meshShare `c:<address>:settings`), so the settings travel with the console.
 *
 * The old app-wide values stay where they were (uploadSettings, restAfterUpload,
 * installPathFallback) as the DEFAULT: a console without its own value reads it. A one-time
 * migration copies the default into every console the roster holds then, so a later change of
 * the default (an older build on another device still edits it) only reaches consoles added after.
 *
 * Every reader asks for the console it acts on (`consoleSettingFor(host, key)`), never the one on
 * screen: the upload queue wakes and rests the console an item is for, keep-awake ticks each
 * console by its own mode, auto-redeploy and bring-up decide per roster profile, an install uses
 * its target's fallback switch.
 *
 * The bring-up playlist is per console too, but playlists live on this device (their ids are
 * local), so its per-console choice stays in the playlist document
 * (`AutoLoaderConfig.bringUpByConsole`, keyed by profile id), with the same migration.
 */
import { hostOf } from "../lib/addr";
import { redeployAction, type RedeployAction } from "../lib/autoRedeploy";
import { bringUpPlaylistIdFor } from "../lib/playlistOps";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useInstallPathFallbackStore } from "./installPathFallback";
import { usePayloadPlaylistsStore } from "./payloadPlaylists";
import { useRestAfterUploadStore } from "./restAfterUpload";
import { useRosterStore, type ConsoleSettings, type PS5Profile } from "./roster";
import { useUploadSettingsStore } from "./uploadSettings";

export type { ConsoleSettings } from "./roster";
export type ConsoleSettingKey = keyof ConsoleSettings;

export const CONSOLE_SETTING_KEYS: readonly ConsoleSettingKey[] = [
  "wake_for_uploads",
  "keep_awake_mode",
  "rest_after_upload",
  "auto_bring_up",
  "auto_redeploy_on_wake",
  "install_path_fallback",
];

/** Set once this device copied the app-wide values into each console. Device-only. */
export const MIGRATED_KEY = "ps5upload.console_settings.migrated.v1";

/** The app-wide values: what a console without its own value reads. */
export function defaultConsoleSettings(): ConsoleSettings {
  const u = useUploadSettingsStore.getState();
  return {
    wake_for_uploads: u.wakeForUploads,
    keep_awake_mode: u.keepPs5AwakeMode,
    rest_after_upload: useRestAfterUploadStore.getState().enabled,
    auto_bring_up: u.autoBringUp,
    auto_redeploy_on_wake: u.autoRedeployOnWake,
    install_path_fallback: useInstallPathFallbackStore.getState().enabled,
  };
}

const KEEP_AWAKE = new Set(["off", "transfers", "always"]);

/** One value as stored, if it is well formed (a synced profile may come from any build). */
function valid<K extends ConsoleSettingKey>(key: K, v: unknown): v is ConsoleSettings[K] {
  if (key === "keep_awake_mode") return typeof v === "string" && KEEP_AWAKE.has(v);
  return typeof v === "boolean";
}

/** Pure: a profile's settings, its own values over the defaults. */
export function settingsOfProfile(
  profile: Pick<PS5Profile, "settings"> | null | undefined,
  defaults: ConsoleSettings,
): ConsoleSettings {
  const own = profile?.settings;
  if (!own) return defaults;
  const out = { ...defaults } as Record<ConsoleSettingKey, unknown>;
  for (const k of CONSOLE_SETTING_KEYS) {
    const v = (own as Record<string, unknown>)[k];
    if (valid(k, v)) out[k] = v;
  }
  return out as unknown as ConsoleSettings;
}

/** Pure: the profile a console address belongs to (port-tolerant), if any. */
export function profileForHost(host: string | null | undefined, profiles: readonly PS5Profile[]): PS5Profile | undefined {
  const bare = hostOf((host ?? "").trim());
  if (!bare) return undefined;
  return profiles.find((p) => hostOf(p.host) === bare);
}

/** The settings of the console at `host` (bare ip or ip:port). A console not in the roster reads
 *  the defaults. */
export function consoleSettingsFor(host: string | null | undefined): ConsoleSettings {
  return settingsOfProfile(profileForHost(host, useRosterStore.getState().profiles), defaultConsoleSettings());
}

/** One setting of the console at `host`. */
export function consoleSettingFor<K extends ConsoleSettingKey>(host: string | null | undefined, key: K): ConsoleSettings[K] {
  return consoleSettingsFor(host)[key];
}

/** The settings of a roster profile, by id. */
export function consoleSettingsById(id: string | null | undefined): ConsoleSettings {
  const p = id ? useRosterStore.getState().profiles.find((x) => x.id === id) : undefined;
  return settingsOfProfile(p, defaultConsoleSettings());
}

/** Change one console's setting (by profile id). */
export function setConsoleSetting<K extends ConsoleSettingKey>(id: string, key: K, value: ConsoleSettings[K]): void {
  useRosterStore.getState().setConsoleSettings(id, { [key]: value } as Partial<ConsoleSettings>);
}

/** Reactive: the settings of a roster profile (by id), following the roster and the defaults. */
export function useConsoleSettings(id: string | null | undefined): ConsoleSettings {
  const profile = useRosterStore((s) => (id ? s.profiles.find((p) => p.id === id) : undefined));
  // Subscribing to each default keeps a console without its own value in step with them.
  const wake = useUploadSettingsStore((s) => s.wakeForUploads);
  const awake = useUploadSettingsStore((s) => s.keepPs5AwakeMode);
  const bringUp = useUploadSettingsStore((s) => s.autoBringUp);
  const redeploy = useUploadSettingsStore((s) => s.autoRedeployOnWake);
  const rest = useRestAfterUploadStore((s) => s.enabled);
  const fallback = useInstallPathFallbackStore((s) => s.enabled);
  return settingsOfProfile(profile, {
    wake_for_uploads: wake,
    keep_awake_mode: awake,
    rest_after_upload: rest,
    auto_bring_up: bringUp,
    auto_redeploy_on_wake: redeploy,
    install_path_fallback: fallback,
  });
}

/** Reactive: one setting of the console at `host` (the selected console, for status lines). */
export function useConsoleSettingForHost<K extends ConsoleSettingKey>(host: string | null | undefined, key: K): ConsoleSettings[K] {
  const id = useRosterStore((s) => profileForHost(host, s.profiles)?.id);
  return useConsoleSettings(id)[key];
}

/**
 * Pure: what the migration writes. Each console gets the app-wide value of every setting it has
 * no value for; a console that already has one (set on another device and synced here) keeps it.
 * Null when nothing changes.
 */
export function migrationFill(
  profiles: readonly PS5Profile[],
  defaults: ConsoleSettings,
): Record<string, Partial<ConsoleSettings>> | null {
  const out: Record<string, Partial<ConsoleSettings>> = {};
  let any = false;
  for (const p of profiles) {
    const own = (p.settings ?? {}) as Record<string, unknown>;
    const fill: Partial<ConsoleSettings> = {};
    let need = false;
    for (const k of CONSOLE_SETTING_KEYS) {
      if (valid(k, own[k])) continue;
      (fill as Record<string, unknown>)[k] = defaults[k];
      need = true;
    }
    if (need) {
      out[p.id] = fill;
      any = true;
    }
  }
  return any ? out : null;
}

/** Pure: the bring-up map the migration writes (every console gets the app-wide playlist), or
 *  null when the map exists already. */
export function bringUpMigration(
  autoLoader: { bringUpPlaylistId: string | null; bringUpByConsole?: Record<string, string | null> },
  profiles: readonly Pick<PS5Profile, "id">[],
): Record<string, string | null> | null {
  if (autoLoader.bringUpByConsole) return null;
  return Object.fromEntries(profiles.map((p) => [p.id, autoLoader.bringUpPlaylistId]));
}

function migrateBringUp() {
  const run = () => {
    const pl = usePayloadPlaylistsStore.getState();
    const map = bringUpMigration(pl.autoLoader, useRosterStore.getState().profiles);
    if (map) pl.setAutoLoader({ bringUpByConsole: map });
  };
  if (usePayloadPlaylistsStore.getState().loaded) {
    run();
    return;
  }
  // The playlist document loads later (a file on the desktop app): copy once it has.
  const unsub = usePayloadPlaylistsStore.subscribe((s) => {
    if (!s.loaded) return;
    unsub();
    run();
  });
}

/**
 * The one-time copy of the app-wide values into each console this device knows. Call after
 * `ensureRosterMigrated()` and before sharing starts, so the copy is this device's set-up from
 * before (mesh stamps it lowest: a console's value set on another device wins over it).
 * Idempotent.
 */
export function ensureConsoleSettingsMigrated(): void {
  migrateBringUp();
  if (safeGetItem(MIGRATED_KEY) === "1") return;
  const fill = migrationFill(useRosterStore.getState().profiles, defaultConsoleSettings());
  if (fill) useRosterStore.getState().fillConsoleSettings(fill);
  safeSetItem(MIGRATED_KEY, "1");
}

/** Pure: which keep-awake modes are in use at all, over every console and the default (a console
 *  not in the roster reads it). Drives whether the keep-awake tick runs. */
export function keepAwakeModesInUse(
  profiles: readonly Pick<PS5Profile, "settings">[],
  defaults: ConsoleSettings,
): { anyOn: boolean; anyAlways: boolean } {
  const modes = [defaults.keep_awake_mode, ...profiles.map((p) => settingsOfProfile(p, defaults).keep_awake_mode)];
  return { anyOn: modes.some((m) => m !== "off"), anyAlways: modes.includes("always") };
}

/**
 * Pure: the consoles one keep-awake tick resets, each by its own mode. `busy` are the consoles
 * with an upload, install or copy running ("During uploads and installs" and "Always" keep them
 * awake); `up` are the consoles whose helper answers ("Always" keeps those awake too).
 */
export function keepAwakeHosts(
  busy: Iterable<string>,
  up: Iterable<string>,
  modeFor: (host: string) => ConsoleSettings["keep_awake_mode"],
): Set<string> {
  const out = new Set<string>();
  for (const h of busy) if (modeFor(h) !== "off") out.add(h);
  for (const h of up) if (modeFor(h) === "always") out.add(h);
  return out;
}

/** What the auto-redeploy loop does for one roster console that went down, by ITS settings
 *  (Bring up automatically, Reconnect after rest mode). */
export function redeployActionForProfile(
  profile: Pick<PS5Profile, "settings">,
  defaults: ConsoleSettings,
  upThisBoot: boolean,
): RedeployAction {
  const s = settingsOfProfile(profile, defaults);
  return redeployAction({ autoBringUp: s.auto_bring_up, autoRedeploy: s.auto_redeploy_on_wake, upThisBoot });
}

/** The install options the console at `host` adds: its install-by-path switch, nothing while it
 *  is off (state/installPathFallback). */
export function installPathFallbackOptionsFor(host: string | null | undefined): { console_path_fallback?: true } {
  return consoleSettingFor(host, "install_path_fallback") ? { console_path_fallback: true } : {};
}

/** The bring-up playlist id for the console at `host`, or the default. */
export function bringUpPlaylistIdForHost(host: string | null | undefined): string | null {
  const id = profileForHost(host, useRosterStore.getState().profiles)?.id;
  return bringUpPlaylistIdFor(usePayloadPlaylistsStore.getState().autoLoader, id);
}

/** Choose a console's bring-up playlist (by profile id; null: none, send the helper only). */
export function setBringUpPlaylistFor(consoleId: string, playlistId: string | null): void {
  const pl = usePayloadPlaylistsStore.getState();
  pl.setAutoLoader({ bringUpByConsole: { ...(pl.autoLoader.bringUpByConsole ?? {}), [consoleId]: playlistId } });
}
