// The copies of the ps5upload helper a console keeps and starts at boot (#540, #541).
//
// "Send helper" replaces the running helper in memory only. A console set up with Payload
// Manager's autoload, itsPLK's WebKit Autoloader (a ps5_autoloader folder with autoload.txt
// on /data or a USB drive) or the app's own USB autoloader wizard starts its STORED copy at
// every boot, so the old version came back after each restart and the app said "helper is
// vX but app is vY" every time. After a send, this finds those copies and replaces them
// with the helper just sent (engine route /api/ps5/helper/replace-copy), then says what it
// did — or what it could not do and how to do it by hand.
//
// Only files named like the helper are touched (`ps5upload*.elf`, never the installer
// daemon), always under the same name, and only files that already exist.

import type { PmOverview } from "../api/payloadManager";
import type { Translator } from "../state/lang";

/** Where a stored copy lives: Payload Manager's storage, an autoloader folder
 *  (ps5_autoloader/ or a USB drive's autoload.txt), or etaHEN's payload folder. */
export type CopyPlace = "payload-manager" | "autoloader" | "etahen";

export interface StoredCopy {
  path: string;
  place: CopyPlace;
  /** Named in that loader's boot list (Payload Manager's autoload, autoload.txt). */
  atBoot: boolean;
}

export type CopyStatus = "updated" | "unchanged" | "failed";

export interface CopyResult {
  copy: StoredCopy;
  status: CopyStatus;
  error?: string;
}

export interface CopyDeps {
  pmOverview: (host: string) => Promise<PmOverview>;
  /** Throws when the folder is not there. */
  listDir: (host: string, path: string) => Promise<Array<{ name: string; kind: string }>>;
  readText: (host: string, path: string) => Promise<string>;
}

export interface ReplaceDeps extends CopyDeps {
  replace: (host: string, path: string) => Promise<"updated" | "unchanged" | "not_found">;
}

/** A file name the helper is stored under: `ps5upload.elf`, `PS5Upload-8.5.1.elf`,
 *  `ps5upload_v9.elf`… never the installer daemon (`ps5upload-installer.elf`). Same rule as
 *  the engine's `is_helper_file_name`. */
export function isHelperFileName(name: string): boolean {
  const l = name.trim().toLowerCase();
  return l.startsWith("ps5upload") && l.endsWith(".elf") && !l.includes("installer");
}

/** The file names an autoload.txt starts, in order: no comments (`#`), no pauses
 *  (`!1000`), no blank lines; a line with a folder in it gives its last part. */
export function parseAutoloadTxt(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#") || line.startsWith("!")) continue;
    const name = line.split("/").pop()?.trim() ?? "";
    if (name) out.push(name);
  }
  return out;
}

const AUTOLOAD_TXT = "autoload.txt";
/** Mounts to try when /mnt itself cannot be listed. */
const FALLBACK_MOUNTS = ["usb0", "usb1", "usb2", "usb3", "usb4", "usb5", "usb6", "usb7", "ext0", "ext1"];

async function tryList(deps: CopyDeps, host: string, path: string) {
  try {
    return await deps.listDir(host, path);
  } catch {
    return null;
  }
}

/** The helper files in `dir`, and whether its autoload.txt names them. With `onlyListed`
 *  (a drive's root), only folders with an autoload.txt count, and only the files it names. */
async function scanFolder(
  deps: CopyDeps,
  host: string,
  dir: string,
  place: CopyPlace,
  onlyListed: boolean,
): Promise<StoredCopy[]> {
  const entries = await tryList(deps, host, dir);
  if (!entries) return [];
  const files = entries.filter((e) => e.kind === "file");
  const txt = files.find((e) => e.name.toLowerCase() === AUTOLOAD_TXT);
  let listed: Set<string> | null = null;
  if (txt) {
    try {
      listed = new Set(
        parseAutoloadTxt(await deps.readText(host, `${dir}/${txt.name}`)).map((n) => n.toLowerCase()),
      );
    } catch {
      listed = null;
    }
  }
  if (onlyListed && !listed) return [];
  return files
    .filter((e) => isHelperFileName(e.name))
    .filter((e) => !onlyListed || listed!.has(e.name.toLowerCase()))
    .map((e) => ({
      path: `${dir}/${e.name}`,
      place,
      atBoot: !!listed?.has(e.name.toLowerCase()),
    }));
}

async function fromPayloadManager(deps: CopyDeps, host: string): Promise<StoredCopy[] | null> {
  let pm: PmOverview;
  try {
    pm = await deps.pmOverview(host);
  } catch {
    return null;
  }
  if (!pm.present || pm.payloadsError) return null;
  const boot = new Set(pm.autoload?.enabled ? pm.autoload.list.map((e) => e.toLowerCase()) : []);
  return pm.payloads
    .filter((p) => isHelperFileName(p.filename) && p.path.startsWith("/"))
    .map((p) => ({
      path: p.path,
      // A USB find Payload Manager lists is still a file on that drive.
      place: p.path.startsWith("/data/pldmgr/") ? ("payload-manager" as const) : ("autoloader" as const),
      atBoot: boot.has((p.slot || p.filename).toLowerCase()) || boot.has(p.filename.toLowerCase()),
    }));
}

/** Payload Manager's storage read as folders, for when it does not answer: 0.5 keeps
 *  `/data/pldmgr/payloads/<stem>/<file>`, 0.6 `/data/pldmgr/payloads/local/<stem>/<file>`. */
async function pmStorageFolders(deps: CopyDeps, host: string): Promise<StoredCopy[]> {
  const out: StoredCopy[] = [];
  for (const root of ["/data/pldmgr/payloads", "/data/pldmgr/payloads/local"]) {
    const dirs = (await tryList(deps, host, root)) ?? [];
    for (const d of dirs) {
      if (d.kind !== "dir" || !d.name.toLowerCase().startsWith("ps5upload")) continue;
      if (d.name.toLowerCase().includes("installer")) continue;
      out.push(...(await scanFolder(deps, host, `${root}/${d.name}`, "payload-manager", false)));
    }
  }
  return out;
}

/** Every stored copy of the helper on `host` that a boot loader may start. Never throws:
 *  a place that cannot be read is skipped. */
export async function findStoredCopies(host: string, deps: CopyDeps): Promise<StoredCopy[]> {
  const mntList = await tryList(deps, host, "/mnt");
  const mounts = mntList
    ? mntList.filter((e) => e.kind === "dir" && /^(usb|ext)\d+$/.test(e.name)).map((e) => e.name)
    : FALLBACK_MOUNTS;
  const scans: Array<Promise<StoredCopy[] | null>> = [
    fromPayloadManager(deps, host).then((r) => r ?? pmStorageFolders(deps, host)),
    scanFolder(deps, host, "/data/ps5_autoloader", "autoloader", false),
    scanFolder(deps, host, "/data/etaHEN/payloads", "etahen", false),
    ...mounts.flatMap((m) => [
      scanFolder(deps, host, `/mnt/${m}/ps5_autoloader`, "autoloader", false),
      scanFolder(deps, host, `/mnt/${m}`, "autoloader", true),
    ]),
  ];
  const seen = new Map<string, StoredCopy>();
  for (const list of await Promise.all(scans.map((p) => p.catch(() => null)))) {
    for (const c of list ?? []) {
      const prev = seen.get(c.path);
      seen.set(c.path, prev ? { ...prev, atBoot: prev.atBoot || c.atBoot } : c);
    }
  }
  return [...seen.values()];
}

/** Find the stored copies and replace each with the helper just sent. Never throws. */
export async function updateStoredCopies(
  host: string,
  deps: ReplaceDeps,
): Promise<CopyResult[]> {
  const copies = await findStoredCopies(host, deps).catch(() => []);
  const results: CopyResult[] = [];
  // One at a time: each is a multi-MB write to the console.
  for (const copy of copies) {
    try {
      const r = await deps.replace(host, copy.path);
      if (r === "not_found") continue;
      results.push({ copy, status: r });
    } catch (e) {
      results.push({ copy, status: "failed", error: e instanceof Error ? e.message : String(e) });
    }
  }
  return results;
}

export function placeLabel(place: CopyPlace, tr: Translator): string {
  switch (place) {
    case "payload-manager":
      return tr("helper_copy_place_pldmgr", undefined, "Payload Manager");
    case "etahen":
      return tr("helper_copy_place_etahen", undefined, "etaHEN payloads");
    default:
      return tr("helper_copy_place_autoloader", undefined, "autoloader");
  }
}

function describe(results: CopyResult[], tr: Translator): string {
  return results.map((r) => `${r.copy.path} (${placeLabel(r.copy.place, tr)})`).join("; ");
}

export interface CopiesNotice {
  level: "success" | "warning";
  title: string;
  body: string;
}

/** What to tell the user after `updateStoredCopies`: what was replaced, and what could not
 *  be with how to do it by hand. Nothing when every copy was already current (or none). */
export function storedCopiesNotices(
  results: CopyResult[],
  tr: Translator,
  version: string | null,
): CopiesNotice[] {
  const v = version ? `v${version}` : "";
  const out: CopiesNotice[] = [];
  const updated = results.filter((r) => r.status === "updated");
  const failed = results.filter((r) => r.status === "failed");
  if (updated.length) {
    out.push({
      level: "success",
      title: tr(
        "helper_copies_updated_title",
        undefined,
        "Helper updated",
      ),
      body: tr(
        "helper_copies_updated_body",
        { version: v },
        "Your PS5 will start helper {version} the next time it restarts.",
      ),
    });
  }
  if (failed.length) {
    const errors = failed
      .map((r) => r.error)
      .filter((e): e is string => !!e)
      .join("; ");
    out.push({
      level: "warning",
      title: tr(
        "helper_copies_failed_title",
        undefined,
        "Couldn't update the helper your PS5 starts at boot",
      ),
      body: tr(
        "helper_copies_failed_body",
        { list: describe(failed, tr), version: v, error: errors },
        "Still the old helper: {list}. The PS5 starts it again after every restart. To fix it by hand, download ps5upload.elf {version} from the releases page and copy it over that file (Files screen, FTP or the USB drive), or upload it again in Payload Manager under the same name. ({error})",
      ),
    });
  }
  return out;
}

const running = new Set<string>();

/** `updateStoredCopies` then `notify` for each notice; one run per console at a time. */
export async function refreshStoredCopies(
  host: string,
  deps: ReplaceDeps,
  tr: Translator,
  version: string | null,
  notify: (n: CopiesNotice) => void,
): Promise<CopyResult[]> {
  if (running.has(host)) return [];
  running.add(host);
  try {
    const results = await updateStoredCopies(host, deps);
    for (const n of storedCopiesNotices(results, tr, version)) notify(n);
    return results;
  } catch {
    return [];
  } finally {
    running.delete(host);
  }
}
