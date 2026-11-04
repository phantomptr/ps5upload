/**
 * Settings and the console list, the same on every device that uses a console (8.0.0; rebuilt
 * on a CRDT in 8.0.1).
 *
 * Each console keeps one document, "state": a last-writer-wins map stamped with hybrid logical
 * clocks (lib/meshCrdt.ts). Keys:
 *   p:<setting key>          a shared setting's raw value (null: removed);
 *   c:<address>              whether that console is in the list (1, or null once removed);
 *   c:<address>:<field>      one field of it (its name, notes, wake keys...).
 * A console is known by its address on every device (each device names it with an id of its own),
 * and each field is its own register, so a rename on one device and a wake key set on another
 * both survive. A removal is a stamped register, so a console deleted on one device stays deleted
 * whichever console another device syncs through next.
 *
 * Every device keeps its own copy of the map (with its stamps). A change made here is stamped when
 * it happens (or found and stamped before the next sync); a sync merges this copy with the
 * console's, applies what changed to this device, and writes the merge back when the console's
 * copy lacked something. Merging is order-independent, so the copies on every device and console
 * converge.
 *
 * A console with a helper from before 8.0.0 has no documents: nothing is shared through it.
 */
import { meshDocGet, meshDocPut } from "../api/mesh";
import { useConnectionStore } from "../state/connection";
import { meshFor, useMeshStore } from "../state/mesh";
import { useRosterStore, type PS5Profile } from "../state/roster";
import { hostOf } from "./addr";
import { asMap, Clock, maxHlc, merge, pruneTombstones, sameMap, type Hlc, type LwwMap } from "./meshCrdt";
import { onStorageWrite, safeGetItem, safeRemoveItem, safeSetItem } from "./safeStorage";
import { reloadSettings } from "./liveSettings";
import { backupBeforeChange } from "./settingsBackup";
import { MESH_KEYS } from "./sharedPrefs";

const STATE_KEY = "ps5upload.mesh_state.v2";
const DOC = "state";

/** Fields of a console that are this device's own, never shared. */
const LOCAL_FIELDS = new Set(["id", "last_seen_at", "last_seen_kernel", "last_seen_payload"]);

interface Persisted {
  device: string;
  clock: { t: number; c: number };
  m: LwwMap;
}

function randomId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return "d_" + Math.random().toString(36).slice(2) + Date.now().toString(36);
}

export function loadState(): Persisted {
  try {
    const p = JSON.parse(safeGetItem(STATE_KEY) ?? "null") as Partial<Persisted> | null;
    if (p && typeof p.device === "string" && p.device && p.clock && typeof p.clock.t === "number") {
      return { device: p.device, clock: { t: p.clock.t, c: p.clock.c ?? 0 }, m: asMap(p.m) };
    }
  } catch {
    // A bad copy starts over: the consoles' copies bring everything back.
  }
  return { device: randomId(), clock: { t: 0, c: 0 }, m: {} };
}

function saveState(s: Persisted) {
  safeSetItem(STATE_KEY, JSON.stringify(s));
}

/** This app's sync id, kept from the first time it is asked for (also names its settings backup
 *  on the consoles, lib/settingsBackup). */
export function syncId(): string {
  const st = loadState();
  if (!safeGetItem(STATE_KEY)) saveState(st);
  return st.device;
}

// ---- this device's values, as map keys ----

const P = "p:";
const C = "c:";

export function settingKey(k: string) {
  return P + k;
}
export function consoleKey(host: string) {
  return C + hostOf(host);
}
export function fieldKey(host: string, field: string) {
  return `${C}${hostOf(host)}:${field}`;
}
const isConsoleKey = (k: string) => k.startsWith(C) && !k.slice(C.length).includes(":");

/** What this device has now, for every shared key it holds. */
export function localView(profiles: readonly PS5Profile[], read: (k: string) => string | null): Map<string, unknown> {
  const out = new Map<string, unknown>();
  for (const k of MESH_KEYS) out.set(settingKey(k), read(k));
  for (const p of profiles) {
    if (!hostOf(p.host)) continue;
    out.set(consoleKey(p.host), 1);
    for (const [f, v] of Object.entries(p)) {
      if (LOCAL_FIELDS.has(f)) continue;
      out.set(fieldKey(p.host, f), v ?? null);
    }
  }
  return out;
}

const same = (a: unknown, b: unknown) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

/**
 * Stamps what changed here since the map last saw it; a console that left this device's list
 * gets a removal. `baseline` is the look taken when sharing starts: a value the map has never had
 * then is the set-up from before sharing and gets the lowest stamp (it loses to any change made
 * anywhere since, and to what the consoles already hold, see adoptBaseline). Any other new value
 * is a change made now.
 */
export function stampLocal(m: LwwMap, view: Map<string, unknown>, clock: Clock, baseline = false): LwwMap {
  const out: LwwMap = { ...m };
  // Consoles added on this device since the last look: adding one is a change (it must beat an
  // old removal), but its fields are only this device's defaults ("PS5 (192.168.1.5)"); the name
  // and settings the other devices already gave that console must win over them.
  const added = new Set<string>();
  for (const k of view.keys()) if (isConsoleKey(k) && !(m[k] && m[k].v !== null)) added.add(k);
  const ofAdded = (k: string) => {
    const i = k.indexOf(":", C.length);
    return i > 0 && added.has(k.slice(0, i));
  };
  for (const [k, v] of view) {
    const cur = out[k];
    if (!cur) {
      if (v === null) continue; // nothing here and nothing known: nothing to say
      out[k] = { v, h: baseline || ofAdded(k) ? { t: 0, c: 0, d: clock.device } : clock.now() };
    } else if (!same(cur.v, v)) {
      out[k] = { v, h: clock.now() };
    }
  }
  // Consoles this map lists that are no longer here: removed on this device.
  for (const [k, r] of Object.entries(m)) {
    if (isConsoleKey(k) && r.v !== null && !view.has(k)) out[k] = { v: null, h: clock.now() };
  }
  return out;
}

/**
 * The console list this map describes, starting from this device's (order and ids kept): consoles
 * removed elsewhere go, fields changed elsewhere are taken, consoles added elsewhere are appended.
 */
export function rosterFrom(
  m: LwwMap,
  local: readonly PS5Profile[],
  newId: () => string = randomId,
): PS5Profile[] {
  const fieldsOf = (hk: string): Record<string, unknown> => {
    const out: Record<string, unknown> = {};
    const prefix = `${C}${hk}:`;
    for (const [k, r] of Object.entries(m)) if (k.startsWith(prefix)) out[k.slice(prefix.length)] = r.v;
    return out;
  };
  const next: PS5Profile[] = [];
  const seen = new Set<string>();
  for (const p of local) {
    const hk = hostOf(p.host);
    if (!hk) {
      next.push(p);
      continue;
    }
    if (seen.has(hk)) continue; // one entry per address
    const exists = m[C + hk];
    if (exists && exists.v === null) continue; // removed on some device, after it was added here
    seen.add(hk);
    next.push({ ...p, ...(fieldsOf(hk) as Partial<PS5Profile>), id: p.id });
  }
  for (const [k, r] of Object.entries(m)) {
    if (!isConsoleKey(k)) continue;
    const hk = k.slice(C.length);
    if (r.v === null || seen.has(hk)) continue;
    const f = fieldsOf(hk);
    if (typeof f.host !== "string" || !f.host) continue;
    seen.add(hk);
    next.push({ ...(f as unknown as PS5Profile), id: newId(), name: String(f.name ?? f.host) });
  }
  return next;
}

/** What this device must change to match the map: settings to write, and the new console list
 *  (null when it is the same). */
export function plan(
  m: LwwMap,
  view: Map<string, unknown>,
  profiles: readonly PS5Profile[],
  newId?: () => string,
): { settings: [string, string | null][]; roster: PS5Profile[] | null } {
  const settings: [string, string | null][] = [];
  for (const k of MESH_KEYS) {
    const r = m[settingKey(k)];
    if (!r) continue;
    if (!same(r.v, view.get(settingKey(k)))) settings.push([k, typeof r.v === "string" ? r.v : null]);
  }
  const roster = rosterFrom(m, profiles, newId);
  const strip = (l: readonly PS5Profile[]) =>
    l.map((p) => Object.fromEntries(Object.entries(p).filter(([f]) => !LOCAL_FIELDS.has(f) || f === "id")));
  return { settings, roster: same(strip(roster), strip(profiles)) ? null : roster };
}

// ---- the console's copy ----

/** The console on screen, when its helper keeps documents; else null. */
export function meshHost(): string | null {
  const c = useConnectionStore.getState();
  const host = hostOf(c.host);
  if (!host || c.payloadStatus !== "up") return null;
  return meshFor(useMeshStore.getState(), host).supported === true ? host : null;
}

const LEGACY: Hlc = { t: 0, c: 0, d: "legacy" };

/** The 8.0.0 documents ("prefs" with per-key times, "consoles" as a list), as a map: read once,
 *  when a console has no "state" document yet. */
async function legacy(host: string): Promise<LwwMap> {
  const out: LwwMap = {};
  const prefs = await meshDocGet(host, "prefs").catch(() => null);
  const keys =
    prefs && prefs.supported && prefs.doc ? (prefs.doc.keys as Record<string, { v?: unknown; at?: unknown }>) : null;
  if (keys && typeof keys === "object") {
    for (const k of MESH_KEYS) {
      const e = keys[k];
      if (!e || typeof e.at !== "number") continue;
      out[settingKey(k)] = { v: typeof e.v === "string" ? e.v : null, h: { ...LEGACY, t: e.at } };
    }
  }
  const consoles = await meshDocGet(host, "consoles").catch(() => null);
  const list = consoles && consoles.supported && consoles.doc ? consoles.doc.profiles : null;
  if (Array.isArray(list)) {
    for (const p of list as Record<string, unknown>[]) {
      if (typeof p?.host !== "string" || !hostOf(p.host)) continue;
      out[consoleKey(p.host)] = { v: 1, h: LEGACY };
      for (const [f, v] of Object.entries(p)) if (!LOCAL_FIELDS.has(f)) out[fieldKey(p.host, f)] = { v: v ?? null, h: LEGACY };
    }
  }
  return out;
}

// ---- running it ----

/** Set while this module writes settings or the list, so those writes are not stamped as local
 *  changes. */
let applying = false;

/** Stamps this device's changes into its copy. Returns the copy, and whether anything changed
 *  (a status poll touches the console list on every round without changing anything shared). */
function stampNow(baseline = false): { st: Persisted; changed: boolean } {
  const st = loadState();
  const clock = new Clock(st.device, st.clock);
  const view = localView(useRosterStore.getState().profiles, safeGetItem);
  const m = stampLocal(st.m, view, clock, baseline);
  const changed = !sameMap(m, st.m);
  if (changed) saveState({ device: st.device, clock: clock.last, m });
  return { st: { device: st.device, clock: clock.last, m }, changed };
}

/**
 * A device's set-up from before sharing (stamp time 0) does not compete with what the consoles
 * already hold: it takes theirs. Which of two untouched set-ups "wins" would otherwise come down
 * to comparing random device ids, and a new phone's default console name could replace the one
 * named on the Mac. The first device to share a value sets it; real changes still win by time.
 */
export function adoptBaseline(local: LwwMap, remote: LwwMap): LwwMap {
  const out: LwwMap = { ...local };
  for (const [k, r] of Object.entries(local)) {
    const theirs = remote[k];
    if (r.h.t === 0 && theirs && theirs.h.d !== r.h.d) delete out[k];
  }
  return out;
}

/** One sync with `host`. Returns the settings it changed here. */
export async function syncWith(host: string): Promise<string[]> {
  const got = await meshDocGet(host, DOC);
  if (!got.supported) return [];
  let rev = got.rev;
  let remote = asMap((got.doc as { m?: unknown } | null)?.m);
  if (rev === 0) remote = merge(await legacy(host), remote);
  const written: string[] = [];
  for (let attempt = 0; attempt < 4; attempt++) {
    // Stamped after every wait, never before: a change made while a request was out is part of
    // this round instead of being undone by it.
    const { st } = stampNow();
    const clock = new Clock(st.device, st.clock);
    clock.observe(maxHlc(remote));
    const merged = pruneTombstones(merge(adoptBaseline(st.m, remote), remote), Date.now());
    // Apply to this device what the merge changed.
    const profiles = useRosterStore.getState().profiles;
    const todo = plan(merged, localView(profiles, safeGetItem), profiles);
    // Several settings about to change at once, or consoles about to go: keep a copy of how they
    // were first.
    if (
      todo.settings.length >= 3 ||
      (todo.roster && (todo.settings.length > 0 || todo.roster.length < profiles.length))
    ) {
      void backupBeforeChange("before-sync", useMeshStore.getState().me?.name ?? "");
    }
    applying = true;
    try {
      for (const [k, v] of todo.settings) {
        if (v === null) safeRemoveItem(k);
        else safeSetItem(k, v);
        written.push(k);
      }
      if (todo.roster) useRosterStore.getState().replaceShared(todo.roster);
    } finally {
      applying = false;
    }
    saveState({ device: st.device, clock: clock.last, m: merged });
    if (sameMap(merged, remote)) return written;
    const put = await meshDocPut(host, DOC, rev, { v: 2, m: merged });
    if (!put.supported || put.saved) return written;
    // Another device wrote in between: merge theirs in too and try again.
    rev = put.rev;
    remote = asMap((put.doc as { m?: unknown } | null)?.m);
  }
  return written;
}

const PULL_MS = 60_000;
const PUSH_DEBOUNCE_MS = 800;

/** Starts sharing; returns a stop. `onLive` gets the sidebar keys another device changed (every
 *  other setting is re-read by its store as it arrives: lib/liveSettings). */
export function startMeshShare(
  onLive: (keys: string[]) => void,
): () => void {
  const warn = (e: unknown) => console.warn("[mesh-share]", e instanceof Error ? e.message : String(e));
  // Duplicates left by 8.0.0 go before anything is shared.
  useRosterStore.getState().dedupeByHost();
  // The baseline: what this device has now is "from before" (lowest stamp, the first time a key is
  // seen), so a change made from here on is stamped as a change.
  stampNow(true);
  let running: Promise<void> = Promise.resolve();
  const run = () => {
    running = running
      .then(async () => {
        const host = meshHost();
        if (!host) {
          stampNow(); // keep the change's real time for when a console is reachable
          return;
        }
        const written = await syncWith(host);
        if (!written.length) return;
        // Every loaded store re-reads what changed, so the app follows at once with nothing to
        // reload; a store not loaded yet reads the new value when it is.
        reloadSettings(written);
        const live = written.filter((k) => k.startsWith("ps5upload.desktop-sidebar."));
        if (live.length) onLive(live);
      })
      .catch(warn);
  };
  let timer: number | undefined;
  const soon = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(run, PUSH_DEBOUNCE_MS);
  };
  const shared = new Set(MESH_KEYS);
  const stopWrites = onStorageWrite((key) => {
    if (applying || !shared.has(key)) return;
    if (stampNow().changed) soon();
  });
  const unsubRoster = useRosterStore.subscribe((s, prev) => {
    if (applying || s.profiles === prev.profiles) return;
    if (stampNow().changed) soon();
  });
  // A console that keeps documents came on screen: sync now.
  let lastHost: string | null = null;
  const onMesh = () => {
    const host = meshHost();
    if (host === lastHost) return;
    lastHost = host;
    if (host) run();
  };
  const unsubMesh = useMeshStore.subscribe(onMesh);
  const unsubConn = useConnectionStore.subscribe(onMesh);
  onMesh();
  const pull = () => {
    if (document.visibilityState === "visible") run();
  };
  const interval = window.setInterval(pull, PULL_MS);
  window.addEventListener("focus", pull);
  return () => {
    stopWrites();
    unsubRoster();
    unsubMesh();
    unsubConn();
    window.clearTimeout(timer);
    window.clearInterval(interval);
    window.removeEventListener("focus", pull);
  };
}
