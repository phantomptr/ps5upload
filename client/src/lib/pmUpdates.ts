// Keeping what ps5upload stored in Payload Manager up to date (Reddit/Discord ask: "sync with
// the payloads on the device"). Payload Manager 0.5 reports no versions and many assets keep one
// file name across releases, so the app remembers, per console, which catalogue payload and
// release it stored under each file name. The Payload Manager tab compares that with the
// catalogue's current release and offers the update. Payloads Payload Manager installed from
// its own repository are its to update and are never touched.

import type { PayloadReleaseInfo } from "../api/ps5";

export interface PmStoreRecord {
  /** Catalogue payload id. */
  id: string;
  /** The release tag that was stored. */
  tag: string;
}

type Records = Record<string, Record<string, PmStoreRecord>>;

const KEY = "ps5upload.pmStored.v1";

function load(): Records {
  try {
    const v = JSON.parse(globalThis.localStorage?.getItem(KEY) ?? "{}");
    return v && typeof v === "object" ? (v as Records) : {};
  } catch {
    return {};
  }
}

function save(r: Records): void {
  try {
    globalThis.localStorage?.setItem(KEY, JSON.stringify(r));
  } catch {
    /* private window or blocked storage: updates are simply not offered */
  }
}

/** What this app stored in `host`'s Payload Manager, by file name. */
export function pmRecords(host: string): Record<string, PmStoreRecord> {
  return load()[host] ?? {};
}

/** Remember that `filename` on `host` is catalogue payload `id` at release `tag`. */
export function recordPmStore(host: string, filename: string, id: string, tag: string): void {
  const all = load();
  all[host] = { ...(all[host] ?? {}), [filename]: { id, tag } };
  save(all);
}

export function forgetPmStore(host: string, filename: string): void {
  const all = load();
  if (!all[host]?.[filename]) return;
  const { [filename]: _gone, ...rest } = all[host];
  void _gone;
  all[host] = rest;
  save(all);
}

/** Stored from Payload Manager's own repository: it updates those itself. */
export function isRepoManaged(p: { path: string; slot: string }): boolean {
  return p.slot.startsWith("repo/") || p.path.includes("/payloads/repo/");
}

type Token = number | string;

/** A tag split for ordering, as the engine's `is_newer` does: its leading numbers, then what
 *  follows them as words and numbers (`1.7beta11` → [1, 7], ["beta", 11]). */
function tagParts(tag: string): { nums: number[]; rest: Token[] } | null {
  const m = /\d[\d.]*/.exec(tag);
  if (!m) return null;
  const nums = m[0]
    .split(".")
    .filter((s) => s !== "")
    .map(Number);
  const rest = (tag.slice(m.index + m[0].length).toLowerCase().match(/[a-z]+|\d+/g) ?? []).map((t) =>
    /^\d/.test(t) ? Number(t) : t,
  );
  return { nums, rest };
}

const PRE_WORDS = new Set(["alpha", "beta", "rc", "pre", "preview", "test", "testing", "dev", "nightly", "snapshot", "canary"]);

/** 0: a pre-release (`beta3`, `rc1`, `-dr-test1`, `b2`), 1: the release, 2: a later revision of
 *  it (`2.5B`). */
function stage(rest: Token[]): number {
  if (rest.length === 0) return 1;
  const pre = rest.some(
    (t, i) => typeof t === "string" && (PRE_WORDS.has(t) || ((t === "a" || t === "b") && typeof rest[i + 1] === "number")),
  );
  return pre ? 0 : 2;
}

function cmpTokens(a: Token[], b: Token[]): number {
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const x = a[i];
    const y = b[i];
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    if (typeof x !== typeof y) return typeof x === "number" ? 1 : -1;
    if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}

/** Whether release `latest` is newer than `installed` (the engine's `is_newer`): numbers first,
 *  then a pre-release before its release before a lettered revision, then the words and numbers
 *  that follow (`1.7beta10` → `1.7beta11`). A tag without numbers is newer whenever it differs. */
export function tagIsNewer(latest: string, installed: string): boolean {
  const l = latest.trim();
  const i = installed.trim();
  if (!l || l === i) return false;
  if (!i) return true;
  const a = tagParts(l);
  const b = tagParts(i);
  if (!a || !b) return true;
  for (let k = 0; k < Math.max(a.nums.length, b.nums.length); k++) {
    const x = a.nums[k] ?? 0;
    const y = b.nums[k] ?? 0;
    if (x !== y) return x > y;
  }
  const sa = stage(a.rest);
  const sb = stage(b.rest);
  if (sa !== sb) return sa > sb;
  return cmpTokens(a.rest, b.rest) > 0;
}

/** The release to update to, or null when the stored one is current (or unknown). A stored
 *  pre-release newer than the catalogue's default release is not "updated" back to it. */
export function pmUpdateFor(
  rec: PmStoreRecord | undefined,
  latest: PayloadReleaseInfo | null | undefined,
): PayloadReleaseInfo | null {
  if (!rec || !latest || !latest.picked_asset_url) return null;
  return tagIsNewer(latest.tag, rec.tag) ? latest : null;
}

/** `list` with `oldKey` replaced by `newKey` in the same place (pauses kept). When `newKey` is
 *  already listed elsewhere, `oldKey` just goes. */
export function swapAutoloadKey(list: string[], oldKey: string, newKey: string): string[] {
  if (oldKey === newKey || !list.includes(oldKey)) return list;
  if (list.includes(newKey)) return list.filter((e) => e !== oldKey);
  return list.map((e) => (e === oldKey ? newKey : e));
}

/** For a payload stored by hand (not by this app, not from Payload Manager's repository): the
 *  catalogue payload whose downloaded file has the same name, so the tab can offer to replace
 *  it with the catalogue's release. Payload Manager records no version for these, so whether
 *  the stored one is older cannot be known; the offer says so. */
export function catalogueMatch(
  p: { path: string; slot: string; filename: string },
  inventory: { payload_id: string; path: string }[],
  catalog: readonly { id: string; file_name?: string; is_custom?: boolean }[] = [],
): string | null {
  if (isRepoManaged(p) || !p.path.startsWith("/data/pldmgr/")) return null;
  const name = p.filename.toLowerCase();
  const hit = inventory.find((e) => (e.path.split(/[\\/]/).pop() ?? "").toLowerCase() === name);
  if (hit) return hit.payload_id;
  // Not downloaded here: the catalogue payload whose file this is, by the name Payload Manager
  // keys it under (`shadowmountplus_v1.2.elf` and `shadowmountplus.elf` are both
  // "shadowmountplus"). Only one that is certain: two catalogue payloads sharing a file name
  // (the kstuff builds) match neither, and Payload Manager itself is its own to update.
  const key = slotStem(p.filename);
  if (!key || key.startsWith("pldmgr") || key.startsWith("payload-manager")) return null;
  const ids = catalog
    .filter((c) => !c.is_custom && c.file_name && c.id !== "payload-manager" && slotStem(c.file_name) === key)
    .map((c) => c.id);
  return ids.length === 1 ? ids[0] : null;
}

/** The name Payload Manager 0.6 files a payload under (its `slot_local_key`): the file name
 *  without `.elf`/`.bin`, cut at a version (`_v1.2`, `-1.0`) and a trailing `-ps5`/`-ps4`. */
export function slotStem(filename: string): string {
  let s = filename.replace(/\.(elf|bin)$/i, "");
  const v = /[_\-. ][vV]?\d/.exec(s);
  if (v) s = s.slice(0, v.index);
  s = s.replace(/[_\-. ][pP][sS][45]$/, "");
  return (s || filename.replace(/\.[^.]*$/, "")).toLowerCase();
}
