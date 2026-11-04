/**
 * The self-hosted web UI's settings live in the engine too, so every browser that opens the page
 * gets the same set-up (pinned tabs set on one computer were missing on another). The browser
 * keeps its copy in localStorage as before; this keeps a fixed list of those keys the same as the
 * engine's document `/api/shared/prefs` (engine shared_state.rs).
 *
 * What is shared is what describes how the household uses PS5Upload: the sidebar's pinned, hidden
 * and folded screens, upload / install / convert / link settings, the save path, beta features,
 * and a few facts about each console (its helper stopped on purpose, Minion turned off, its
 * console-to-console routes). What stays in each browser: theme, language, text size and other
 * accessibility settings, which console is on screen, the last page, dismissed tips, the
 * notification inbox and task history.
 *
 * Each key carries the time it was last changed; two browsers saving at once keep, per key, the
 * newer change. The shared values are read before the app starts (see `preloadSharedPrefs`), so
 * every store reads them as its own; while a page is open, its changes are saved as they happen and
 * other browsers' changes are fetched on focus and every minute, and every store reads them at
 * once (lib/liveSettings).
 *
 * Desktop and mobile apps keep their settings on the device and mirror the same keys to their own
 * engine, so `GET|PATCH /api/settings` (engine settings_api.rs) reads and changes what they use.
 * An app pointed at another computer's engine does not mirror.
 */
import { getEngineUrl, isLoopbackUrl } from "../state/engine";
import { pageBasePath } from "./basePath";
import { onStorageWrite, safeGetItem, safeRemoveItem, safeSetItem } from "./safeStorage";
import { reloadSettings } from "./liveSettings";
import { isTauriEnv } from "./tauriEnv";

/** The keys every browser shares. A key not here never leaves this browser. */
export const SHARED_KEYS: readonly string[] = [
  // The sidebar: pinned, hidden and folded screens.
  "ps5upload.desktop-sidebar.pinned.v1",
  "ps5upload.desktop-sidebar.hidden.v1",
  "ps5upload.desktop-sidebar.closed-sections.v1",
  // Uploads.
  "ps5upload.always_overwrite",
  "ps5upload.show_transfer_files",
  "ps5upload.bandwidth_cap_mbps",
  "ps5upload.auto_resume",
  "ps5upload.queue_auto_start",
  "ps5upload.wake_for_uploads",
  "ps5upload.keep_ps5_awake_mode",
  "ps5upload.keep_ps5_awake",
  "ps5upload.auto_bring_up",
  "ps5upload.system_file_read",
  "ps5upload.rest_after_upload",
  "ps5upload.register_after_upload",
  // Installs and links.
  "ps5upload.auto_install_after_upload",
  "ps5upload.auto_remove_after_install",
  "ps5upload.auto_scan_external",
  "ps5upload.install_path_fallback",
  "ps5upload.link_install_mode",
  "ps5upload.link_install_insecure",
  // Convert (the output folder is a path in the container, the same for every browser).
  "ps5upload.convert.compression",
  "ps5upload.convert.full_verify",
  "ps5upload.convert.image_compress",
  "ps5upload.convert.image_format",
  "ps5upload.convert.image_level",
  "ps5upload.convert.make",
  "ps5upload.convert.output_dir",
  "ps5upload.convert.verify_chunk_kib",
  // Everything else that is set-up rather than taste.
  "ps5upload.save_path",
  "ps5upload.auto_save_backup",
  "ps5upload.beta_features",
  "ps5upload.log_level",
  "ps5upload.notif_prune_days.v1",
  "ps5upload.screenshots.png_mode",
  "ps5upload.saved_searches.v1",
  // Facts about each console.
  "ps5upload.helperStopped",
  "ps5upload.minionOff",
  "ps5upload.c2c.routes.v1",
];

const SHARED = new Set(SHARED_KEYS);

/** The sidebar keys: applied to an open page as they arrive (the rest at the next load). */
export const LIVE_KEYS: ReadonlySet<string> = new Set(SHARED_KEYS.slice(0, 3));

/**
 * The settings every device that uses a console shares through it (8.0.0, the console's "prefs"
 * document). The Docker list minus what belongs to one device: its bandwidth (each device has its
 * own network), where it saves and converts to (a path on that device), and its log level.
 */
export const MESH_KEYS: readonly string[] = SHARED_KEYS.filter(
  (k) =>
    k !== "ps5upload.bandwidth_cap_mbps" &&
    k !== "ps5upload.convert.output_dir" &&
    k !== "ps5upload.save_path" &&
    k !== "ps5upload.log_level" &&
    // How THIS device reaches each console (a phone on mobile data goes direct), keyed by the
    // address it uses: another device's routes mean nothing here.
    k !== "ps5upload.c2c.routes.v1" &&
    // "This computer turned Minion off" (kept off by this device): per device by design.
    k !== "ps5upload.minionOff",
);

/** Which keys a sync covers and where it keeps its own bookkeeping. */

/** The Docker engine's shared settings (`/api/shared/prefs`). */
const SHARED_KEY_SET: ReadonlySet<string> = new Set(SHARED_KEYS);
const META_KEY = "ps5upload.shared_prefs.meta.v1";

/** One key's shared value: the raw localStorage string (null: removed) and when it changed. */
export interface Entry {
  v: string | null;
  at: number;
}
export type Entries = Record<string, Entry>;

export interface Doc {
  rev: number;
  value: { keys?: Entries } | null;
}

/** This browser's sync state: the last revision it agreed on, and when each key last changed
 *  here or was taken from the engine. Device-only, never shared. */
interface Meta {
  rev: number;
  at: Record<string, number>;
}

function loadMeta(): Meta {
  try {
    const m = JSON.parse(safeGetItem(META_KEY) ?? "null") as Partial<Meta> | null;
    if (m && typeof m.rev === "number" && m.at && typeof m.at === "object") return { rev: m.rev, at: { ...m.at } };
  } catch {
    // A bad meta starts over: the engine's values win, this browser's unshared ones are offered.
  }
  return { rev: 0, at: {} };
}
const saveMeta = (m: Meta) => safeSetItem(META_KEY, JSON.stringify(m));

/** The engine's answer, checked: anything else is an error, so a misread answer can never
 *  overwrite this browser's settings. Entries that are not well formed are dropped. */
export function asDoc(raw: unknown): Doc {
  const d = raw as Partial<Doc> | null;
  if (!d || typeof d.rev !== "number" || !Number.isInteger(d.rev) || d.rev < 0) {
    throw new Error("the engine does not keep shared settings");
  }
  const keys: Entries = {};
  const inKeys = (d.value as { keys?: unknown } | null)?.keys;
  if (inKeys && typeof inKeys === "object") {
    for (const [k, e] of Object.entries(inKeys as Record<string, unknown>)) {
      const en = e as Partial<Entry> | null;
      if (!en || typeof en.at !== "number" || !Number.isFinite(en.at)) continue;
      if (en.v !== null && typeof en.v !== "string") continue;
      keys[k] = { v: en.v, at: en.at };
    }
  }
  return { rev: d.rev, value: d.value === null && d.rev === 0 ? null : { keys } };
}

/** Two sets of entries as one: per key, the newer change. Keys this build does not know are kept
 *  (another browser may run a newer one). */
export function mergeEntries(a: Entries, b: Entries): Entries {
  const out: Entries = { ...a };
  for (const [k, e] of Object.entries(b)) {
    const cur = out[k];
    if (!cur || e.at > cur.at) out[k] = e;
  }
  return out;
}

/** What this browser has for the shared keys, with when each changed (0: before sharing). */
export function localEntries(
  read: (k: string) => string | null,
  at: Record<string, number>,
  keys: Iterable<string> = SHARED_KEYS,
): Entries {
  const out: Entries = {};
  for (const k of keys) {
    const v = read(k);
    const when = at[k] ?? 0;
    if (v === null && when === 0) continue; // never set here: nothing to offer
    out[k] = { v, at: when };
  }
  return out;
}

/**
 * Reconcile this browser with the engine's entries. The engine wins for a key unless this browser
 * changed it later; a key the engine does not have yet is offered from here (the first browser to
 * open a new version hands its settings over). Returns what to write here and what to send.
 */
export function plan(
  local: Entries,
  remote: Entries,
  now: number,
  shared: ReadonlySet<string> = SHARED,
): { apply: Entries; send: Entries | null } {
  const apply: Entries = {};
  let sendNeeded = false;
  const mine: Entries = {};
  for (const [k, e] of Object.entries(local)) {
    const r = remote[k];
    if (!r) {
      mine[k] = { v: e.v, at: e.at || now };
      sendNeeded = true;
    } else if (e.at > r.at) {
      mine[k] = e;
      sendNeeded = true;
    }
  }
  for (const [k, r] of Object.entries(remote)) {
    if (!shared.has(k)) continue;
    const e = local[k];
    if (!e || r.at > e.at) apply[k] = r;
  }
  return { apply, send: sendNeeded ? mergeEntries(remote, mine) : null };
}

/* ---- the engine ---- */

export interface PrefsApi {
  get(signal?: AbortSignal): Promise<Doc>;
  put(baseRev: number, keys: Entries): Promise<{ rev: number } | { conflict: Doc }>;
}

/** The engine is the server the page came from (under its base path, behind a proxy); for an app,
 *  its own engine on this device. */
function engineBase(): string {
  if (isTauriEnv()) {
    const url = getEngineUrl();
    if (!isLoopbackUrl(url)) throw new Error("the app is using another computer's engine");
    return url;
  }
  return window.location.origin + pageBasePath();
}

export const enginePrefsApi: PrefsApi = {
  async get(signal) {
    const r = await fetch(`${engineBase()}/api/shared/prefs`, { signal });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return asDoc(await r.json());
  },
  async put(baseRev, keys) {
    const r = await fetch(`${engineBase()}/api/shared/prefs`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ base_rev: baseRev, value: { keys } }),
    });
    if (r.status === 409) return { conflict: asDoc(await r.json()) };
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return (await r.json()) as { rev: number };
  },
};

/* ---- the sync ---- */

/** Set while engine values are written here, so those writes are not sent back. */
let applying = false;

function writeLocal(entries: Entries, meta: Meta): string[] {
  const changed: string[] = [];
  applying = true;
  try {
    for (const [k, e] of Object.entries(entries)) {
      if (safeGetItem(k) !== e.v) {
        if (e.v === null) safeRemoveItem(k);
        else safeSetItem(k, e.v);
        changed.push(k);
      }
      meta.at[k] = e.at;
    }
  } finally {
    applying = false;
  }
  return changed;
}

/** One sync round: take what is newer on the engine, send what is newer here. Retries a few times
 *  when another browser saves in between. Returns the keys written here. */
export async function syncOnce(api: PrefsApi, signal?: AbortSignal): Promise<string[]> {
  const meta = loadMeta();
  let doc = await api.get(signal);
  const written: string[] = [];
  for (let attempt = 0; attempt < 4; attempt++) {
    const remote = doc.value?.keys ?? {};
    const { apply, send } = plan(localEntries(safeGetItem, meta.at, SHARED_KEY_SET), remote, Date.now(), SHARED_KEY_SET);
    written.push(...writeLocal(apply, meta));
    meta.rev = doc.rev;
    if (!send) {
      saveMeta(meta);
      return written;
    }
    const r = await api.put(doc.rev, send);
    if ("rev" in r) {
      for (const [k, e] of Object.entries(send)) if (SHARED_KEY_SET.has(k)) meta.at[k] = e.at;
      meta.rev = r.rev;
      saveMeta(meta);
      return written;
    }
    doc = r.conflict;
  }
  saveMeta(meta);
  return written;
}

/**
 * Before the app starts (browser build only): take the engine's settings into localStorage, so
 * every store reads them as its own on its first load. Bounded: a slow or older engine leaves this
 * browser's settings as they are.
 */
export async function preloadSharedPrefs(api: PrefsApi = enginePrefsApi, timeoutMs = 2000): Promise<void> {
  const ctl = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  // The whole sync, not only its GET: the app's import waits on this, and a PUT that stalled left
  // the page blank. What finishes after the bound still lands for the next load.
  const bound = new Promise<void>((resolve) => {
    timer = setTimeout(() => {
      ctl.abort();
      resolve();
    }, timeoutMs);
  });
  try {
    await Promise.race([
      bound,
      syncOnce(api, ctl.signal).catch((e) => {
        console.warn("[shared-prefs]", e instanceof Error ? e.message : String(e));
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** How often an open page looks for changes made in another browser (also on focus). */
export const PULL_MS = 60_000;
/** Changes here are sent this long after the last one (a burst of toggles is one save). */
const PUSH_DEBOUNCE_MS = 500;

/**
 * The time for a change made now: later than anything this browser has seen, as well as being the
 * time. A browser whose clock runs ahead would otherwise win every key until real time caught up,
 * and another's change made after it would lose (mesh uses the same kind of clock, for the same reason).
 */
export function nextStamp(seen: Record<string, number>, now: number): number {
  let newest = 0;
  for (const at of Object.values(seen)) if (at > newest) newest = at;
  return Math.max(now, newest + 1);
}

/**
 * While the page is open: stamp and send this browser's changes to shared keys, and fetch other
 * browsers' changes. `onLive` gets the sidebar keys that changed so the open page can show them.
 * Returns a stop.
 */
export function startSharedPrefs(
  onLive: (keys: string[]) => void,
  api: PrefsApi = enginePrefsApi,
  syncNow = false,
): () => void {
  let timer: number | undefined;
  let running: Promise<void> = Promise.resolve();
  const warn = (e: unknown) => console.warn("[shared-prefs]", e instanceof Error ? e.message : String(e));
  const run = () => {
    running = running
      .then(() => syncOnce(api))
      .then((written) => {
        // Every store re-reads what changed (lib/liveSettings): no reload needed.
        reloadSettings(written);
        const live = written.filter((k) => LIVE_KEYS.has(k));
        if (live.length) onLive(live);
      })
      .catch(warn);
  };
  const stopWrites = onStorageWrite((key) => {
    if (applying || !SHARED_KEY_SET.has(key)) return;
    const meta = loadMeta();
    meta.at[key] = nextStamp(meta.at, Date.now());
    saveMeta(meta);
    window.clearTimeout(timer);
    timer = window.setTimeout(run, PUSH_DEBOUNCE_MS);
  });
  const pull = () => {
    if (document.visibilityState === "visible") run();
  };
  // An app has no preload before it starts (its engine may still be starting): first look now.
  if (syncNow) run();
  const interval = window.setInterval(pull, PULL_MS);
  window.addEventListener("focus", pull);
  document.addEventListener("visibilitychange", pull);
  return () => {
    stopWrites();
    window.clearTimeout(timer);
    window.clearInterval(interval);
    window.removeEventListener("focus", pull);
    document.removeEventListener("visibilitychange", pull);
  };
}
