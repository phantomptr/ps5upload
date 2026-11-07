// Settings backups (8.0.1). A bundle is every `ps5upload.` key of this app's storage (the same JSON
// Settings › Export bundle saves). Copies are kept in two places so either can bring a device back:
//  * the engine (`/api/settings-backup`, its data folder; Docker's volume for the web UI): the
//    newest 14, one a day plus one before a sync or a restore changes several settings at once;
//  * the console: each app's latest, trimmed to fit, so a device whose disk was wiped gets its
//    set-up back from any console it used. An index document ("backups") hands out a fixed set of
//    slots (`backup-01`...), keyed by each app's own sync id: browsers sharing one Docker engine
//    are separate apps with separate copies, and the console never holds more than SLOTS of them.

import { meshDocGet, meshDocPut } from "../api/mesh";
import { getEngineUrl } from "../state/engine";
import { hostOf } from "./addr";
import { safeGetItem, safeRemoveItem, safeSetItem } from "./safeStorage";

export const NS_PREFIX = "ps5upload.";
/** When the last automatic backup was taken (never itself backed up or restored). */
const LAST_AUTO_KEY = "ps5upload.settings_backup.last_auto.v1";
/** Per console: when this device's copy there was last written ({host: ms}). Not backed up either. */
const LAST_CONSOLE_KEY = "ps5upload.settings_backup.last_console.v1";
/** Keys that describe this app rather than its set-up: never backed up, never restored (restoring
 *  another device's sync id would make two devices one in the settings sync). */
const OWN_KEYS = new Set([
  LAST_AUTO_KEY,
  LAST_CONSOLE_KEY,
  "ps5upload.mesh_state.v2",
  "ps5upload.shared_prefs.meta.v1",
  "ps5upload.mesh_prefs.meta.v1",
  "ps5upload.mesh.client",
  "ps5upload.roster.share_unsaved.v1",
  "ps5upload.browser_staged.v1",
  "ps5upload.resume_txids.v1",
]);
/** The keys that sign in to a console's user. A saved file can go anywhere, so it never carries them
 *  (`buildBundle`). The engine's copy keeps them sealed (settings_backup.rs) and a console's own copy
 *  holds only that console's (`forConsole`), so a wiped device can still wake and sign in
 *  (`buildBackupBundle`). A restore keeps the ones this app holds. */
const ROSTER_KEY = "ps5upload.roster.v1";
const PAIRING_FIELDS = ["wake_credential", "wake_regist_key", "wake_rp_key", "rp_users"] as const;
const DAY_MS = 24 * 60 * 60 * 1000;
/** Before-sync backups at most this often (a busy sync must not rotate the daily ones out). */
const BEFORE_SYNC_EVERY_MS = 60 * 60 * 1000;
/** A console document holds at most 160 KiB; the console copy stays under this. */
const CONSOLE_BUDGET = 140 * 1024;

export interface Bundle {
  meta: {
    generated_at_ms: number;
    schema: 1;
    reason?: string;
    device?: string;
    dropped?: string[];
    /** The engine kept the console keys back: this caller is not on the engine's computer. */
    secrets_withheld?: boolean;
  };
  local_storage: Record<string, string | null>;
}

type RosterProfile = Record<string, unknown> & { id?: unknown; host?: unknown };

/** The profiles of a stored roster, or null when the value is not one. */
function parseProfiles(text: string | null | undefined): { doc: Record<string, unknown>; profiles: RosterProfile[] } | null {
  if (!text) return null;
  try {
    const doc = JSON.parse(text) as Record<string, unknown> | null;
    if (!doc || !Array.isArray(doc.profiles)) return null;
    return { doc, profiles: doc.profiles as RosterProfile[] };
  } catch {
    return null;
  }
}

function sameConsole(a: unknown, b: unknown): boolean {
  if (typeof a !== "string" || typeof b !== "string") return false;
  return hostOf(a.trim()).toLowerCase() === hostOf(b.trim()).toLowerCase();
}

function withoutPairing(roster: string): string | null {
  const r = parseProfiles(roster);
  if (!r) return null;
  for (const p of r.profiles) for (const f of PAIRING_FIELDS) delete p[f];
  return JSON.stringify(r.doc);
}

/** The restored roster, with the keys this device already holds for any console the backup has
 *  none for (one the engine kept them back from, a file, or a console's own copy). A restore must
 *  not cost the ability to wake a console. Keys a backup does hold stand. */
export function keepLiveWakeKeys(restored: string, live: string | null): string {
  const from = parseProfiles(live);
  const to = parseProfiles(restored);
  if (!from || !to) return restored;
  for (const p of to.profiles) {
    const mine =
      (p.id === undefined ? undefined : from.profiles.find((l) => l.id === p.id)) ??
      from.profiles.find((l) => sameConsole(l.host, p.host));
    if (!mine) continue;
    for (const f of PAIRING_FIELDS) if (!p[f] && mine[f]) p[f] = mine[f];
  }
  return JSON.stringify(to.doc);
}

/** The bundle for a console's own copy: only that console's keys. Who can read its documents
 *  already controls that console, but not the others on the roster. */
export function forConsole(b: Bundle, host: string): Bundle {
  const text = b.local_storage[ROSTER_KEY];
  if (typeof text !== "string") return b;
  const roster = parseProfiles(text);
  const local_storage = { ...b.local_storage };
  if (!roster) {
    delete local_storage[ROSTER_KEY];
    return { ...b, local_storage };
  }
  for (const p of roster.profiles) {
    if (!sameConsole(p.host, host)) for (const f of PAIRING_FIELDS) delete p[f];
  }
  local_storage[ROSTER_KEY] = JSON.stringify(roster.doc);
  return { ...b, local_storage };
}

function collectBundle(reason: string, device: string, roster: (text: string) => string | null): Bundle {
  const local_storage: Record<string, string | null> = {};
  try {
    for (let i = 0; i < window.localStorage.length; i++) {
      const k = window.localStorage.key(i);
      if (!k || !k.startsWith(NS_PREFIX) || OWN_KEYS.has(k)) continue;
      const v = safeGetItem(k);
      if (k === ROSTER_KEY && v !== null) {
        // A roster it cannot read stays out: never a risk of copying keys along.
        const kept = roster(v);
        if (kept !== null) local_storage[k] = kept;
      } else local_storage[k] = v;
    }
  } catch {
    // Storage blocked: an empty bundle, which nothing restores from.
  }
  return {
    meta: { generated_at_ms: Date.now(), schema: 1, reason, device },
    local_storage,
  };
}

/** The bundle for a file: no pairing keys. */
export function buildBundle(reason = "manual", device = ""): Bundle {
  return collectBundle(reason, device, withoutPairing);
}

/** The bundle for the backup copies: with the pairing keys, which the engine seals and
 *  `saveConsoleBackup` narrows to the console it writes to. */
export function buildBackupBundle(reason = "manual", device = ""): Bundle {
  return collectBundle(reason, device, (text) => (parseProfiles(text) ? text : null));
}

/** Writes a bundle's keys back (only this app's namespace). Returns how many. Throws on a file
 *  that is not a bundle, before anything is written. */
export function applyBundle(parsed: unknown): number {
  if (!parsed || typeof parsed !== "object" || typeof (parsed as Bundle).local_storage !== "object") {
    throw new Error("file is not a ps5upload settings bundle");
  }
  const ls = (parsed as Bundle).local_storage as Record<string, unknown>;
  const actions: Array<{ key: string; value: string | null }> = [];
  for (const k of Object.keys(ls)) {
    // Only our own namespace: a hand-edited bundle can't write arbitrary keys.
    if (!k.startsWith(NS_PREFIX) || OWN_KEYS.has(k)) continue;
    const v = ls[k];
    if (typeof v === "string") actions.push({ key: k, value: k === ROSTER_KEY ? keepLiveWakeKeys(v, safeGetItem(k)) : v });
    else if (v === null) actions.push({ key: k, value: null });
    else if (v !== undefined) throw new Error(`invalid value for ${k}`);
  }
  for (const { key, value } of actions) {
    if (value === null) safeRemoveItem(key);
    else safeSetItem(key, value);
  }
  // The self-hosted web UI: the engine's list would replace the restored one at the next load
  // (state/rosterShare.ts). This list is the one asked for, so it goes back to the engine.
  if (typeof ls[ROSTER_KEY] === "string") {
    safeSetItem("ps5upload.roster.share_unsaved.v1", JSON.stringify({ base: null }));
  }
  return actions.length;
}

/** The bundle cut to `budget` bytes for the console: the largest values (histories, caches) go
 *  first, and are listed in `meta.dropped`. */
export function trimBundle(b: Bundle, budget = CONSOLE_BUDGET): Bundle {
  const entries = Object.entries(b.local_storage).sort(
    (x, y) => (x[1]?.length ?? 0) + x[0].length - ((y[1]?.length ?? 0) + y[0].length),
  );
  const kept: Record<string, string | null> = {};
  const dropped: string[] = [];
  let size = 512;
  for (const [k, v] of entries) {
    const add = k.length + (v?.length ?? 4) + 8;
    if (size + add > budget) dropped.push(k);
    else {
      kept[k] = v;
      size += add;
    }
  }
  return { meta: { ...b.meta, dropped }, local_storage: kept };
}

// ---- the engine's copies ----

export interface BackupEntry {
  id: string;
  at: number;
  reason: string;
  device: string;
  keys: number;
  bytes: number;
}

async function engine(route: string, init?: RequestInit): Promise<unknown> {
  const res = await fetch(`${getEngineUrl()}${route}`, { ...init, signal: AbortSignal.timeout(20_000) });
  const body = (await res.json().catch(() => ({}))) as { error?: string };
  if (res.status === 404) throw new Error("This app's engine is older than 8.0.1: restart or update it.");
  if (!res.ok) throw new Error(body.error || `request failed (${res.status})`);
  return body;
}

export async function listEngineBackups(): Promise<BackupEntry[]> {
  const b = (await engine("/api/settings-backup")) as { backups?: BackupEntry[] };
  return Array.isArray(b.backups) ? b.backups : [];
}

export async function getEngineBackup(id: string): Promise<Bundle> {
  return (await engine(`/api/settings-backup/get?id=${encodeURIComponent(id)}`)) as Bundle;
}

export async function saveEngineBackup(bundle: Bundle): Promise<void> {
  await engine("/api/settings-backup", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ reason: bundle.meta.reason ?? "manual", bundle }),
  });
}

// ---- the console's copies ----

const INDEX_DOC = "backups";
const SLOTS = 16;

interface IndexEntry {
  slot: number;
  /** The app's sync id (lib/meshShare's device). */
  app: string;
  name: string;
  at: number;
  keys: number;
}

export interface ConsoleBackup extends IndexEntry {
  bundle: Bundle;
}

const slotDoc = (slot: number) => `backup-${String(slot).padStart(2, "0")}`;

function readIndex(doc: Record<string, unknown> | null): IndexEntry[] {
  const list = doc && Array.isArray(doc.list) ? (doc.list as Partial<IndexEntry>[]) : [];
  return list.filter(
    (e): e is IndexEntry =>
      typeof e?.slot === "number" &&
      e.slot >= 1 &&
      e.slot <= SLOTS &&
      typeof e.app === "string" &&
      typeof e.at === "number",
  );
}

/** Which slot `app` writes: its own, a free one, or the one written longest ago. */
export function pickSlot(list: readonly IndexEntry[], app: string): number {
  const mine = list.find((e) => e.app === app);
  if (mine) return mine.slot;
  for (let s = 1; s <= SLOTS; s++) if (!list.some((e) => e.slot === s)) return s;
  return [...list].sort((a, b) => a.at - b.at)[0].slot;
}

/** Writes this app's copy to the console. False when the console keeps no documents. */
export async function saveConsoleBackup(host: string, app: string, name: string, bundle: Bundle): Promise<boolean> {
  const doc = trimBundle(forConsole(bundle, host));
  // Claim the slot first (the index's revision makes two apps' claims one after the other), then
  // fill it.
  let slot = 0;
  for (let i = 0; i < 4 && !slot; i++) {
    const cur = await meshDocGet(host, INDEX_DOC);
    if (!cur.supported) return false;
    const list = readIndex(cur.doc);
    const s = pickSlot(list, app);
    const entry: IndexEntry = {
      slot: s,
      app,
      name,
      at: doc.meta.generated_at_ms,
      keys: Object.keys(doc.local_storage).length,
    };
    const next = [...list.filter((e) => e.slot !== s && e.app !== app), entry];
    const put = await meshDocPut(host, INDEX_DOC, cur.rev, { v: 1, list: next });
    if (!put.supported) return false;
    if (put.saved) slot = s;
  }
  if (!slot) return false;
  for (let i = 0; i < 3; i++) {
    const cur = await meshDocGet(host, slotDoc(slot));
    if (!cur.supported) return false;
    const put = await meshDocPut(host, slotDoc(slot), cur.rev, { app, ...doc } as unknown as Record<string, unknown>);
    if (!put.supported) return false;
    if (put.saved) return true;
  }
  return false;
}

/** Every app's copy on this console, newest first. A slot whose document belongs to another app
 *  (it was handed on) is left out. */
export async function listConsoleBackups(host: string): Promise<ConsoleBackup[]> {
  const idx = await meshDocGet(host, INDEX_DOC);
  if (!idx.supported) return [];
  const out = await Promise.all(
    readIndex(idx.doc).map(async (e) => {
      const got = await meshDocGet(host, slotDoc(e.slot)).catch(() => null);
      const b = got && got.supported ? (got.doc as (Bundle & { app?: string }) | null) : null;
      if (!b || b.app !== e.app || !b.local_storage || typeof b.local_storage !== "object") return null;
      return { ...e, bundle: { meta: b.meta, local_storage: b.local_storage } };
    }),
  );
  return out.filter((x): x is ConsoleBackup => !!x).sort((a, b) => b.at - a.at);
}

// ---- automatic ----

let lastBeforeSync = 0;

/** A copy taken now, before a sync or a restore writes several settings: kept by the engine.
 *  Before a sync it never throws (the sync goes on); before a restore it does, so the restore can
 *  ask whether to go on without one. */
export function backupBeforeChange(reason: "before-sync" | "before-restore", device = ""): Promise<void> {
  if (reason === "before-sync") {
    if (Date.now() - lastBeforeSync < BEFORE_SYNC_EVERY_MS) return Promise.resolve();
    lastBeforeSync = Date.now();
  }
  // Built now (synchronously), so it holds the settings from before the change.
  const b = buildBackupBundle(reason, device);
  const saved = saveEngineBackup(b);
  return reason === "before-restore"
    ? saved
    : saved.catch((e) => console.warn("[settings-backup]", e instanceof Error ? e.message : e));
}

/** The daily copy: to the engine, and to the console when one that keeps documents is connected.
 *  Never throws. */
export async function dailyBackup(host: string | null, app: string | null, device = ""): Promise<void> {
  const now = Date.now();
  const due = (at: unknown) => !(typeof at === "number" && now - at < DAY_MS && at <= now);
  let consoles: Record<string, number>;
  try {
    consoles = (JSON.parse(safeGetItem(LAST_CONSOLE_KEY) ?? "{}") as Record<string, number> | null) ?? {};
  } catch {
    consoles = {};
  }
  const engineDue = due(Number(safeGetItem(LAST_AUTO_KEY) ?? 0));
  const consoleDue = !!host && !!app && due(consoles[host]);
  if (!engineDue && !consoleDue) return;
  const b = buildBackupBundle("daily", device);
  if (!Object.keys(b.local_storage).length) return;
  if (engineDue) {
    try {
      await saveEngineBackup(b);
      safeSetItem(LAST_AUTO_KEY, String(now));
    } catch (e) {
      console.warn("[settings-backup]", e instanceof Error ? e.message : e);
    }
  }
  if (consoleDue && host && app && (await saveConsoleBackup(host, app, device, b).catch(() => false))) {
    safeSetItem(LAST_CONSOLE_KEY, JSON.stringify({ ...consoles, [host]: now }));
  }
}
