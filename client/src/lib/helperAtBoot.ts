// "Load the PS5Upload helper at boot" (#715): Payload Manager (itsPLK's pldmgr.elf) keeps a
// copy of the helper and starts it from its boot list, so Minion and the app work after a
// restart with no PC sending anything.
//
// - Turning it on stores this app's helper in Payload Manager (`ps5upload.elf`, stamped by the
//   engine like every stored copy) and adds it to the END of the boot list, after a short pause:
//   kstuff and whatever else is listed load first, as they must before the helper.
// - On every connection the stored copy is brought up to this app's helper when it differs.
//   The copy it replaces is kept beside it (`ps5upload.elf.prev`, a name no loader starts and no
//   other update touches), and the new copy is "pending" until a boot shows it came up.
// - A boot after the update where this app had to send the helper itself means the new copy
//   did not come up: the kept copy goes back, and this app's version is not stored there again
//   (an app update tries again).
// - Nothing is written when Payload Manager is not running; the switch is hidden then.
// - The switch here decides (#781): while it is on, a connection that finds the stored helper
//   missing from the boot list (taken out in Payload Manager, a list rewritten under other keys)
//   puts it back and logs why. Only turning it off here takes it out; that drops this console's
//   record, which is what "off" is. A copy deleted in Payload Manager is not stored again: the
//   record goes and the switch shows off.
// - The helper counts as listed under any entry Payload Manager starts it from: its slot, its file
//   name or its path. Every write is read back, and a list that does not hold (or still holds) the
//   helper afterwards is an error the caller shows, never a silent success.
//
// The app's auto-run playlist never sends the helper (lib/playlistOps playlistResendsOurHelper)
// and the reconnect loop waits for Payload Manager's boot list before sending one
// (AppShell, `helperAtBootSettleMs`), so a console that starts the helper at boot does not get a
// second one from this app.

import type { PmAutoload, PmOverview, PmStoredPayload } from "../api/payloadManager";
import { autoloadKeys, listedEntry } from "../api/payloadManager";
import { isHelperFileName } from "./helperStoredCopies";
import { safeGetItem, safeSetItem } from "./safeStorage";
import { LOADER_SETTLE_MS } from "./autoRedeploy";
import { hostOf } from "./addr";

/** The pause put in front of the helper in the boot list: kstuff settles before it starts. */
export const BOOT_PAUSE = "!3000";

export interface BootRecord {
  /** The stored copy in Payload Manager. */
  path: string;
  /** This app's version when it last wrote that copy ("" when unknown). */
  version: string;
  /** The version of the copy kept beside it ("" when none was kept). */
  prevVersion: string;
  /** When this app last wrote the copy, on this computer's clock (ms). */
  storedAt: number;
  /** Written and not yet seen coming up after a boot. */
  pending: boolean;
  /** A version that did not come up at boot here: not stored again. */
  badVersion: string;
  /** This app put `BOOT_PAUSE` in front of the helper (and takes it out again). */
  pauseAdded: boolean;
  /** Payload Manager's boot delay, seconds, as last read. */
  delayS: number;
}

const KEY = "ps5upload.helperAtBoot.v1";

function readAll(): Record<string, BootRecord> {
  try {
    const v = JSON.parse(safeGetItem(KEY) ?? "{}") as unknown;
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, BootRecord>) : {};
  } catch {
    return {};
  }
}

export function loadBootRecord(host: string): BootRecord | null {
  const r = readAll()[hostOf(host)];
  return r && typeof r.path === "string" ? r : null;
}

export function saveBootRecord(host: string, rec: BootRecord | null): void {
  const all = readAll();
  if (rec) all[hostOf(host)] = rec;
  else delete all[hostOf(host)];
  safeSetItem(KEY, JSON.stringify(all));
}

/** The copy this app keeps up to date for `host`'s boot list: the update after a send leaves it
 *  to `syncHelperAtBoot`, which keeps the previous copy and can put it back. */
export function isBootCopy(host: string, path: string): boolean {
  return loadBootRecord(host)?.path === path;
}

/** The helper Payload Manager keeps in its own storage, `ps5upload.elf` first. */
export function storedHelper(payloads: readonly PmStoredPayload[]): PmStoredPayload | null {
  const ours = payloads.filter(
    (p) => p.path.startsWith("/data/pldmgr/") && isHelperFileName(p.filename),
  );
  return ours.find((p) => p.filename.toLowerCase() === "ps5upload.elf") ?? ours[0] ?? null;
}

/** Whether the boot list starts `copy`, under its slot, file name or path. */
export function listsHelper(autoload: PmAutoload | null, copy: PmStoredPayload | null): boolean {
  return !!copy && listedEntry(autoload?.list, copy) !== null;
}

/** The entries that name the helper; the first is the one this app adds. */
type Keys = string | readonly string[];
const keysOf = (k: Keys): readonly string[] => (typeof k === "string" ? [k] : k);

/** The boot list with the helper last, behind a pause when something loads before it. Kept as
 *  it is when the helper is already listed under any of `keys` (the user may have moved it). */
export function withHelperAtBoot(
  list: readonly string[],
  keys: Keys,
): { list: string[]; pauseAdded: boolean } {
  const ks = keysOf(keys);
  if (list.some((e) => ks.includes(e))) return { list: [...list], pauseAdded: false };
  const before = list.some((e) => !e.startsWith("!"));
  const endsPaused = list.length > 0 && list[list.length - 1].startsWith("!");
  const pause = before && !endsPaused;
  return { list: [...list, ...(pause ? [BOOT_PAUSE] : []), ks[0]], pauseAdded: pause };
}

/** The boot list without the helper (under any of `keys`), and without the pause this app put
 *  in front of it. */
export function withoutHelperAtBoot(
  list: readonly string[],
  keys: Keys,
  pauseAdded: boolean,
): string[] {
  const ks = keysOf(keys);
  const out: string[] = [];
  for (let i = 0; i < list.length; i++) {
    if (ks.includes(list[i])) continue;
    if (pauseAdded && list[i] === BOOT_PAUSE && ks.includes(list[i + 1])) continue;
    out.push(list[i]);
  }
  return out;
}

const showList = (a: PmAutoload | null | undefined) =>
  a ? `enabled=${a.enabled} [${a.list.join(",")}]` : "unreadable";

/** What writing the boot list needs: the write (which answers with the list read back, when the
 *  engine sends it), a fresh read otherwise, and a log line for each step. */
interface ListDeps {
  pmOverview: (host: string) => Promise<PmOverview>;
  setAutoload: (host: string, enabled: boolean, list: string[]) => Promise<PmAutoload | null | void>;
  log?: (line: string) => void;
}

/** Write the boot list, read it back, and check it holds the helper (`want`) or no longer does.
 *  Throws with what was read back when not, so no caller reports a list that is not there. */
async function writeBootList(
  host: string,
  deps: ListDeps,
  enabled: boolean,
  list: string[],
  copy: PmStoredPayload,
  want: boolean,
): Promise<PmAutoload> {
  deps.log?.(`${host}: writing the boot list enabled=${enabled} [${list.join(",")}]`);
  let back = (await deps.setAutoload(host, enabled, list)) ?? null;
  if (!back) {
    const ov = await deps.pmOverview(host);
    back = ov.present ? ov.autoload : null;
  }
  deps.log?.(`${host}: boot list read back ${showList(back)}`);
  if (!back) throw new Error("Payload Manager's boot list could not be read back after saving it");
  if (listsHelper(back, copy) !== want) {
    const now = back.list.join(", ") || "empty";
    throw new Error(
      want
        ? `Payload Manager's boot list does not include the helper after saving it (it reads: ${now})`
        : `Payload Manager's boot list still includes the helper after saving it (it reads: ${now})`,
    );
  }
  return back;
}

/** How long the reconnect loop waits on a console whose loader just came up before sending a
 *  helper: longer when Payload Manager starts one at boot, by its boot delay and a minute. */
export function helperAtBootSettleMs(rec: BootRecord | null): number {
  if (!rec) return LOADER_SETTLE_MS;
  return LOADER_SETTLE_MS + Math.max(0, rec.delayS) * 1000 + 60_000;
}

/** A boot this long after the copy was written counts as a boot after it (clock slack). */
const BOOT_AFTER_STORE_SLACK_MS = 30_000;

export type BootVerdict = "none" | "wait" | "came-up" | "failed";

/** What the last boot says about a pending copy. `bootEpochMs`: when the console booted (this
 *  computer's clock); `appSentAt`: when this app last sent the helper; `runningVersion`: the
 *  helper answering now. */
export function bootVerdict(
  rec: BootRecord | null,
  facts: { bootEpochMs: number | null; appSentAt: number | null; runningVersion: string | null },
): BootVerdict {
  if (!rec?.pending) return "none";
  if (facts.bootEpochMs === null) return "wait";
  if (facts.bootEpochMs <= rec.storedAt + BOOT_AFTER_STORE_SLACK_MS) return "wait";
  // Booted after the update, and the helper answering is one this app sent since: the copy
  // Payload Manager started did not come up.
  if (facts.appSentAt !== null && facts.appSentAt >= facts.bootEpochMs) return "failed";
  if (rec.version && facts.runningVersion === rec.version) return "came-up";
  // Another helper came up (another loader, another device): no verdict on ours.
  return "wait";
}

export interface SyncDeps extends ListDeps {
  replace: (host: string, path: string) => Promise<"updated" | "unchanged" | "not_found">;
  restore: (host: string, path: string) => Promise<"restored" | "unchanged" | "not_found">;
  bootEpochMs: (host: string) => Promise<number | null>;
  appSentAt: (host: string) => number | null;
  runningVersion: (host: string) => string | null;
  appVersion: () => Promise<string | null>;
  now: () => number;
  load: (host: string) => BootRecord | null;
  save: (host: string, rec: BootRecord | null) => void;
}

export type SyncResult =
  | { kind: "off" }
  | { kind: "no-pm" }
  | { kind: "missing" }
  | { kind: "waiting" }
  | { kind: "current" }
  | { kind: "updated"; version: string }
  | { kind: "came-up"; version: string }
  | { kind: "restored"; failed: string; back: string }
  | { kind: "restore-failed"; failed: string; error: string }
  | { kind: "skipped-bad"; version: string }
  | { kind: "error"; error: string };

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Once per connection: put the helper back in the boot list when it went missing from it, then
 *  judge a pending copy, else bring the stored copy up to this app's helper. Never writes without
 *  Payload Manager answering with its stored payloads and its boot list. */
export async function syncHelperAtBoot(host: string, deps: SyncDeps): Promise<SyncResult> {
  let rec = deps.load(host);
  if (!rec) return { kind: "off" };
  let ov: PmOverview;
  try {
    ov = await deps.pmOverview(host);
  } catch (e) {
    return { kind: "error", error: message(e) };
  }
  if (!ov.present || !ov.autoload) return { kind: "no-pm" };
  // An unread payload list is not an empty one: nothing is decided from it.
  if (ov.payloadsError) return { kind: "error", error: ov.payloadsError };
  const copy = storedHelper(ov.payloads);
  deps.log?.(
    `${host}: boot list ${showList(ov.autoload)}; stored helper ${copy ? `${copy.slot || "-"} ${copy.path}` : "none"}`,
  );
  if (!copy) {
    // Deleted in Payload Manager itself: not stored again, and the switch here shows off.
    deps.log?.(`${host}: the stored helper was deleted in Payload Manager; "load at boot" is off here now`);
    deps.save(host, null);
    return { kind: "missing" };
  }
  rec = { ...rec, path: copy.path, delayS: ov.autoload.delay_s };
  if (!listsHelper(ov.autoload, copy)) {
    // The switch here is on, and only turning it off here takes the helper out (#781): a list
    // rewritten in Payload Manager, or keyed by a name this copy no longer matches, loses it.
    const keys = autoloadKeys(copy);
    deps.log?.(
      `${host}: the helper is stored but not in the boot list (looked for ${keys.join(" | ")}); putting it back`,
    );
    const next = withHelperAtBoot(ov.autoload.list, keys);
    try {
      await writeBootList(host, deps, ov.autoload.enabled, next.list, copy, true);
    } catch (e) {
      deps.save(host, rec);
      return { kind: "error", error: message(e) };
    }
    rec = { ...rec, pauseAdded: rec.pauseAdded || next.pauseAdded };
  }
  if (!ov.autoload.enabled)
    deps.log?.(`${host}: Payload Manager's boot list is turned off, so nothing in it starts at boot`);
  deps.save(host, rec);

  if (rec.pending) {
    const verdict = bootVerdict(rec, {
      bootEpochMs: await deps.bootEpochMs(host).catch(() => null),
      appSentAt: deps.appSentAt(host),
      runningVersion: deps.runningVersion(host),
    });
    if (verdict === "came-up") {
      deps.save(host, { ...rec, pending: false });
      return { kind: "came-up", version: rec.version };
    }
    if (verdict === "failed") {
      const failed = rec.version;
      try {
        const r = await deps.restore(host, copy.path);
        if (r === "not_found") {
          // Nothing was kept (the first copy stored): leave it, but do not store this one again.
          deps.save(host, { ...rec, pending: false, badVersion: failed });
          return { kind: "restore-failed", failed, error: "no previous copy" };
        }
        deps.save(host, {
          ...rec,
          pending: false,
          badVersion: failed,
          version: rec.prevVersion,
          storedAt: deps.now(),
        });
        return { kind: "restored", failed, back: rec.prevVersion };
      } catch (e) {
        return { kind: "restore-failed", failed, error: message(e) };
      }
    }
    // Not judged yet: the kept copy must stay the last one that came up, so nothing is written.
    return { kind: "waiting" };
  }

  const version = await deps.appVersion().catch(() => null);
  if (version && rec.badVersion === version) return { kind: "skipped-bad", version };
  try {
    const r = await deps.replace(host, copy.path);
    if (r === "updated") {
      deps.save(host, {
        ...rec,
        prevVersion: rec.version,
        version: version ?? "",
        storedAt: deps.now(),
        pending: true,
        badVersion: "",
      });
      return { kind: "updated", version: version ?? "" };
    }
    if (r === "unchanged" && version && rec.version !== version) {
      deps.save(host, { ...rec, version });
    }
    return { kind: "current" };
  } catch (e) {
    return { kind: "error", error: message(e) };
  }
}

export interface EnableDeps extends ListDeps {
  /** Store this app's helper as `ps5upload.elf`; `false` when the user declined replacing an
   *  existing folder. */
  store: (host: string) => Promise<boolean>;
  replace: (host: string, path: string) => Promise<"updated" | "unchanged" | "not_found">;
  /** Asked when Payload Manager's boot list is turned off (nothing in it starts at boot): true
   *  turns it on in the same write. It is never turned on without asking. */
  confirmTurnOnAutoload?: () => Promise<boolean>;
  appVersion: () => Promise<string | null>;
  now: () => number;
  load: (host: string) => BootRecord | null;
  save: (host: string, rec: BootRecord | null) => void;
}

/** Store the helper (or bring the stored one up to date) and list it last at boot. Throws with
 *  a reason the caller shows; returns false when the user declined. */
export async function enableHelperAtBoot(host: string, deps: EnableDeps): Promise<boolean> {
  const ov = await deps.pmOverview(host);
  if (!ov.present) throw new Error("Payload Manager is not running on this PS5");
  if (!ov.autoload) throw new Error(ov.autoloadError ?? "Payload Manager's boot list could not be read");
  if (ov.payloadsError) throw new Error(ov.payloadsError);
  deps.log?.(`${host}: turning "load at boot" on; boot list ${showList(ov.autoload)}`);
  const version = (await deps.appVersion().catch(() => null)) ?? "";
  let copy = storedHelper(ov.payloads);
  let wrote: boolean;
  let kept = false;
  if (!copy) {
    if (!(await deps.store(host))) return false;
    const after = await deps.pmOverview(host);
    copy = after.present ? storedHelper(after.payloads) : null;
    if (!copy) throw new Error("Payload Manager did not list the stored helper");
    wrote = true;
  } else {
    const r = await deps.replace(host, copy.path);
    wrote = r === "updated";
    kept = wrote;
  }
  const prev = deps.load(host);
  const next = withHelperAtBoot(ov.autoload.list, autoloadKeys(copy));
  const enabled =
    ov.autoload.enabled || (deps.confirmTurnOnAutoload ? await deps.confirmTurnOnAutoload() : false);
  // The choice is kept before the list is written: a write that fails here is made again on the
  // next connection (syncHelperAtBoot), not left as a stored copy nothing starts (#781).
  deps.save(host, {
    path: copy.path,
    version,
    prevVersion: kept ? (prev?.version ?? "") : "",
    storedAt: wrote ? deps.now() : (prev?.storedAt ?? 0),
    pending: wrote,
    badVersion: "",
    pauseAdded: next.pauseAdded || !!prev?.pauseAdded,
    delayS: ov.autoload.delay_s,
  });
  if (next.list.join("\n") !== ov.autoload.list.join("\n") || enabled !== ov.autoload.enabled) {
    await writeBootList(host, deps, enabled, next.list, copy, true);
  } else {
    deps.log?.(`${host}: the helper is already in the boot list`);
  }
  return true;
}

/** Take the helper out of the boot list (the stored file stays; Payload Manager deletes it). */
export async function disableHelperAtBoot(
  host: string,
  deps: Pick<EnableDeps, "pmOverview" | "setAutoload" | "load" | "save" | "log">,
): Promise<void> {
  const ov = await deps.pmOverview(host);
  if (!ov.present) throw new Error("Payload Manager is not running on this PS5");
  const rec = deps.load(host);
  const copy = storedHelper(ov.payloads);
  deps.log?.(`${host}: turning "load at boot" off; boot list ${showList(ov.autoload)}`);
  if (ov.autoload && copy) {
    const list = withoutHelperAtBoot(ov.autoload.list, autoloadKeys(copy), !!rec?.pauseAdded);
    if (list.length !== ov.autoload.list.length)
      await writeBootList(host, deps, ov.autoload.enabled, list, copy, false);
  }
  // No record is what "off" is: nothing puts the helper back after this.
  deps.save(host, null);
}
