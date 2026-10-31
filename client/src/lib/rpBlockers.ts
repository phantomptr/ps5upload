// Remote Play blockers: payloads that, while on, keep a stream from getting a picture. Each is
// one entry here (catalogue id, why, how to pause it and turn it back on), so the Play start
// path, its errors, the setup checklist and the payload's card share one list. Adding a
// payload = one more entry in RP_BLOCKERS.
//
// Everything is read and written through the helper (fs reads/writes, the process list);
// never FTP. A pause leaves a marker in our own folder on the console, so a pause the app
// could not undo (it closed mid-stream) is offered again on the payload's card.

import {
  fsDelete,
  fsListDir,
  fsMkdir,
  fsReadPreview,
  fsWriteText,
  payloadsDownload,
  payloadsLocalPath,
  payloadsRelease,
  processList,
  sendPayload,
  type ProcessInfo,
} from "../api/ps5";
import {
  HUSH_CONFIG_PATH,
  HUSH_LOCK_PATH,
  HUSH_LOG_PATH,
  hushLockPid,
  hushPauseWaitMs,
  hushProcess,
  hushRevertDone,
  newLogLines,
  parseHushConfig,
  setHushEnabled,
} from "../screens/Hush/hushConfig";
import { mgmtAddr } from "./addr";
import { isTauriEnv } from "./tauriEnv";

/** Our own folder on the console for "this app paused it" markers (one file per blocker). */
export const RP_PAUSED_DIR = "/data/ps5upload/rp-paused";

export function pausedMarkerPath(id: string): string {
  return `${RP_PAUSED_DIR}/${id}`;
}

/** The console a check runs against: its helper address, and its process list read once. */
export interface BlockerCtx {
  /** The console's address as the roster has it (what payloads are sent to). */
  host: string;
  /** The helper's address. */
  addr: string;
  procs: () => Promise<ProcessInfo[]>;
}

export function blockerCtx(host: string): BlockerCtx {
  const addr = mgmtAddr(host.trim());
  let list: Promise<ProcessInfo[]> | null = null;
  return {
    host: host.trim(),
    addr,
    procs: () => (list ??= processList(addr).then((r) => r.processes)),
  };
}

/** How a pause went: the payload gave the encoder back, or the wait ran out first. */
export type PauseOutcome = "restored" | "timeout";

/** How turning it back on went: running again, or set on but it needs loading (a browser
 *  cannot send payloads, or the send failed). */
export type ResumeOutcome = "running" | "needs_send";

export interface RpBlocker {
  /** Its catalogue id. */
  id: string;
  name: string;
  /** Why it blocks Remote Play (i18n key and English). */
  reasonKey: string;
  reason: string;
  /** On and in a stream's way. Reads as little as it can; any read error counts as no. */
  detect(ctx: BlockerCtx): Promise<boolean>;
  /** Switch it off for a stream and leave our marker; resolves once it is out of the way or
   *  the wait ran out. */
  pause(ctx: BlockerCtx, opts?: { pollMs?: number; now?: () => number; sleep?: (ms: number) => Promise<void> }): Promise<PauseOutcome>;
  /** Switch it back on (loading it again when it exited), and clear our marker. */
  resume(ctx: BlockerCtx): Promise<ResumeOutcome>;
}

function decodeBase64Text(base64: string): string {
  const bin = atob(base64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

/** A small text file on the console, or null when it is not there (or unreadable). */
export async function readConsoleText(addr: string, path: string, maxBytes = 16 * 1024): Promise<string | null> {
  try {
    const r = await fsReadPreview(addr, path, maxBytes);
    return r.base64 ? decodeBase64Text(r.base64) : "";
  } catch {
    return null;
  }
}

async function writeConsoleText(addr: string, path: string, text: string): Promise<void> {
  const r = await fsWriteText(addr, path, text);
  if (!r.ok) throw new Error(r.err || `could not write ${path}`);
}

/** Load a catalogue payload on the console again: the cached ELF, else the latest release
 *  (desktop and Android only; a browser cannot send payloads). */
export async function sendCataloguePayload(host: string, id: string): Promise<boolean> {
  if (!isTauriEnv()) return false;
  let path = await payloadsLocalPath(id);
  if (!path) {
    const rel = await payloadsRelease(id);
    if (!rel.picked_asset_url) return false;
    path = (await payloadsDownload(id, rel.picked_asset_url, rel.tag)).path;
  }
  await sendPayload(host, path);
  return true;
}

const sleepMs = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** Hush log reads cover the whole log (rotated at 64 KB). */
const HUSH_LOG_MAX = 72 * 1024;

export const HUSH_BLOCKER: RpBlocker = {
  id: "hush",
  name: "Hush",
  reasonKey: "rp_blocker_hush_reason",
  reason: "Hush is on. It blocks Remote Play's picture.",

  async detect(ctx) {
    const cfg = await readConsoleText(ctx.addr, HUSH_CONFIG_PATH);
    if (cfg === null || !parseHushConfig(cfg).enabled) return false;
    const lock = await readConsoleText(ctx.addr, HUSH_LOCK_PATH, 64);
    try {
      return hushProcess(await ctx.procs(), hushLockPid(lock)) !== null;
    } catch {
      return false;
    }
  },

  async pause(ctx, opts = {}) {
    const now = opts.now ?? Date.now;
    const sleep = opts.sleep ?? sleepMs;
    const pollMs = opts.pollMs ?? 2000;
    const cfg = (await readConsoleText(ctx.addr, HUSH_CONFIG_PATH)) ?? "";
    const before = (await readConsoleText(ctx.addr, HUSH_LOG_PATH, HUSH_LOG_MAX)) ?? "";
    await writeConsoleText(ctx.addr, HUSH_CONFIG_PATH, setHushEnabled(cfg, false));
    await markPaused(ctx.addr, "hush");
    const deadline = now() + hushPauseWaitMs(parseHushConfig(cfg));
    while (now() < deadline) {
      await sleep(pollMs);
      const log = await readConsoleText(ctx.addr, HUSH_LOG_PATH, HUSH_LOG_MAX);
      if (log !== null && hushRevertDone(newLogLines(before, log))) return "restored";
    }
    return "timeout";
  },

  async resume(ctx) {
    const cfg = await readConsoleText(ctx.addr, HUSH_CONFIG_PATH);
    if (cfg !== null && !parseHushConfig(cfg).enabled) {
      await writeConsoleText(ctx.addr, HUSH_CONFIG_PATH, setHushEnabled(cfg, true));
    }
    await clearPaused(ctx.addr, "hush");
    // enabled=0 made Hush exit: it starts again only when loaded.
    const lock = await readConsoleText(ctx.addr, HUSH_LOCK_PATH, 64);
    const running = await ctx
      .procs()
      .then((p) => hushProcess(p, hushLockPid(lock)) !== null)
      .catch(() => false);
    if (running) return "running";
    try {
      return (await sendCataloguePayload(ctx.host, "hush")) ? "running" : "needs_send";
    } catch {
      return "needs_send";
    }
  },
};

/** Every payload known to block Remote Play. */
export const RP_BLOCKERS: readonly RpBlocker[] = [HUSH_BLOCKER];

export function blockerById(id: string): RpBlocker | null {
  return RP_BLOCKERS.find((b) => b.id === id) ?? null;
}

/** The blockers on for this console (each check on its own; one failing is a no). */
export async function detectBlockers(ctx: BlockerCtx, list: readonly RpBlocker[] = RP_BLOCKERS): Promise<RpBlocker[]> {
  const on = await Promise.all(list.map((b) => b.detect(ctx).catch(() => false)));
  return list.filter((_, i) => on[i]);
}

export async function markPaused(addr: string, id: string): Promise<void> {
  await fsMkdir(addr, RP_PAUSED_DIR).catch(() => undefined);
  await writeConsoleText(addr, pausedMarkerPath(id), `paused for Remote Play at ${new Date().toISOString()}\n`);
}

export async function clearPaused(addr: string, id: string): Promise<void> {
  await fsDelete(addr, pausedMarkerPath(id)).catch(() => undefined);
}

/** The blockers this app paused on the console and has not turned back on. */
export async function pausedOnConsole(addr: string): Promise<string[]> {
  try {
    const names = (await fsListDir(addr, RP_PAUSED_DIR)).map((e) => e.name);
    return RP_BLOCKERS.filter((b) => names.includes(b.id)).map((b) => b.id);
  } catch {
    return [];
  }
}
