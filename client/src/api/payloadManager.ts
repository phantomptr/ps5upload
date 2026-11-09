// The console's Payload Manager (itsPLK's pldmgr.elf, :8084) through the engine's
// /api/ps5/pldmgr routes. Plain fetches against the engine origin, so the desktop app, the
// browser build and the Docker build take the same path.

import { getEngineUrl } from "../state/engine";
import { hostOf } from "../lib/addr";

export interface PmStoredPayload {
  path: string;
  /** Payload Manager 0.6's slot (`local/<stem>`, `repo/<id>@<source>`); "" on 0.5. */
  slot: string;
  filename: string;
  folder: string;
  name: string;
  version: string;
  source: string;
  install_source: string;
}

export interface PmAutoload {
  enabled: boolean;
  delay_s: number;
  /** What Payload Manager loads at boot, in order: file names (0.5) or slots (0.6), with
   *  `!<ms>` pauses. `autoloadKey` gives a payload's entry. */
  list: string[];
}

export type PmOverview =
  | { present: false; error: string }
  | {
      present: true;
      version: string;
      payloads: PmStoredPayload[];
      payloadsError: string | null;
      autoload: PmAutoload | null;
      autoloadError: string | null;
    };

interface Wire {
  ok?: boolean;
  error?: string | null;
  present?: boolean;
  version?: string;
  payloads?: PmStoredPayload[] | null;
  payloads_error?: string | null;
  autoload?: PmAutoload | null;
  autoload_error?: string | null;
  needs_confirm?: boolean;
  folder?: string;
  file_exists?: boolean;
}

async function read(res: Response): Promise<Wire> {
  const body = (await res.json().catch(() => ({}))) as Wire;
  if (!res.ok) throw new Error(body.error || `request failed (${res.status})`);
  return body;
}

async function post(path: string, body: unknown, timeoutMs = 30_000): Promise<Wire> {
  const res = await fetch(`${getEngineUrl()}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  return read(res);
}

/** What Payload Manager on `host` stores and how its autoload is set. */
export async function pmOverview(host: string): Promise<PmOverview> {
  const h = encodeURIComponent(hostOf(host));
  const res = await fetch(`${getEngineUrl()}/api/ps5/pldmgr?host=${h}`, {
    signal: AbortSignal.timeout(20_000),
  });
  const w = await read(res);
  if (!w.present) return { present: false, error: w.error ?? "not running" };
  return {
    present: true,
    version: w.version ?? "",
    payloads: w.payloads ?? [],
    payloadsError: w.payloads_error ?? null,
    autoload: w.autoload ?? null,
    autoloadError: w.autoload_error ?? null,
  };
}

export async function pmLaunch(host: string, path: string): Promise<void> {
  const w = await post("/api/ps5/pldmgr/launch", { host: hostOf(host), path });
  if (!w.ok) throw new Error(w.error || "launch failed");
}

export async function pmDelete(host: string, filename: string): Promise<void> {
  const w = await post("/api/ps5/pldmgr/delete", { host: hostOf(host), filename });
  if (!w.ok) throw new Error(w.error || "delete failed");
}

/** Write the boot list. The engine reads it back and fails when Payload Manager did not keep
 *  what was written; the list read back is returned (null from an engine that does not send it). */
export async function pmSetAutoload(host: string, enabled: boolean, list: string[]): Promise<PmAutoload | null> {
  const w = await post("/api/ps5/pldmgr/autoload", { host: hostOf(host), enabled, list });
  if (!w.ok) throw new Error(w.error || "autoload update failed");
  return w.autoload ?? null;
}

export type PmStoreResult =
  | { stored: true; folder: string }
  | { stored: false; needsConfirm: true; folder: string; fileExists: boolean };

/** Store a file from this computer in Payload Manager under `filename`. Without `confirmed`,
 *  an existing folder is reported back instead of written to (storing removes that folder's
 *  other files). */
export async function pmStore(
  host: string,
  filename: string,
  localPath: string,
  confirmed: boolean,
): Promise<PmStoreResult> {
  const w = await post(
    "/api/ps5/pldmgr/store",
    { host: hostOf(host), filename, local_path: localPath, confirmed },
    120_000,
  );
  if (w.ok) return { stored: true, folder: w.folder ?? "" };
  if (w.needs_confirm) {
    return { stored: false, needsConfirm: true, folder: w.folder ?? "", fileExists: !!w.file_exists };
  }
  throw new Error(w.error || "store failed");
}

/** `pmStore` for bytes the caller holds (a payload picked from this computer in any build,
 *  the browser one included). */
export async function pmStoreBytes(
  host: string,
  filename: string,
  bytes: ArrayBuffer | Uint8Array,
  confirmed: boolean,
): Promise<PmStoreResult> {
  const q = new URLSearchParams({ host: hostOf(host), filename, confirmed: String(confirmed) });
  const res = await fetch(`${getEngineUrl()}/api/ps5/pldmgr/upload?${q}`, {
    method: "POST",
    headers: { "content-type": "application/octet-stream" },
    body: bytes as BodyInit,
    signal: AbortSignal.timeout(120_000),
  });
  const w = await read(res);
  if (w.ok) return { stored: true, folder: w.folder ?? "" };
  if (w.needs_confirm) {
    return { stored: false, needsConfirm: true, folder: w.folder ?? "", fileExists: !!w.file_exists };
  }
  throw new Error(w.error || "store failed");
}

/** The autoload-list entry that names `p`: its slot on Payload Manager 0.6, else its file name. */
export function autoloadKey(p: Pick<PmStoredPayload, "slot" | "filename">): string {
  return p.slot || p.filename;
}

/** Every boot-list entry Payload Manager starts `p` from: its slot, its file name (0.5 lists,
 *  and 0.6 still matches a bare `.elf`/`.bin` name against stored files) or its path. */
export function autoloadKeys(p: Pick<PmStoredPayload, "slot" | "filename" | "path">): string[] {
  return [...new Set([p.slot, p.filename, p.path].filter((k) => !!k))];
}

/** The entry of `list` that names `p`, if any (see `autoloadKeys`). */
export function listedEntry(
  list: readonly string[] | null | undefined,
  p: Pick<PmStoredPayload, "slot" | "filename" | "path">,
): string | null {
  const keys = autoloadKeys(p);
  return list?.find((e) => keys.includes(e)) ?? null;
}
