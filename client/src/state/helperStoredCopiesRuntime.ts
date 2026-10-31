// lib/helperStoredCopies wired to the real console and engine (#540, #541).

import { pmOverview } from "../api/payloadManager";
import { bundledPayloadPath, fsListDir, fsReadPreview, helperReplaceCopy } from "../api/ps5";
import { getAppVersion } from "../lib/appVersion";
import {
  refreshStoredCopies,
  type CopyDeps,
  type CopyResult,
  type ReplaceDeps,
} from "../lib/helperStoredCopies";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { isTauriEnv } from "../lib/tauriEnv";
import type { Translator } from "./lang";
import { pushNotification } from "./notifications";

function decodeText(b64: string): string {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

export const liveCopyDeps: CopyDeps = {
  pmOverview,
  listDir: (host, path) => fsListDir(host, path),
  readText: async (host, path) => decodeText((await fsReadPreview(host, path, 64 * 1024)).base64),
};

function liveReplaceDeps(): ReplaceDeps {
  // The desktop app replaces with the very file it just sent; the browser build has none,
  // and the engine uses its own bundled helper.
  let local: Promise<string | null> | null = null;
  const localPath = () =>
    (local ??= isTauriEnv() ? bundledPayloadPath().catch(() => null) : Promise.resolve(null));
  return {
    ...liveCopyDeps,
    replace: async (host, path) => helperReplaceCopy(host, path, await localPath()),
  };
}

/** After a send the new helper answered: bring the copies `host` starts at boot up to this
 *  app's helper, in the background, and say what happened. Never throws. */
export async function updateHelperCopiesAfterSend(
  host: string,
  tr: Translator,
): Promise<CopyResult[]> {
  const version = await getAppVersion().catch(() => null);
  return refreshStoredCopies(host, liveReplaceDeps(), tr, version, (n) => {
    // Payload Manager can put its repo copy back, so the same copy is replaced on every
    // send: say it once per console and version, not each time.
    if (n.level === "success" && !firstUpdateNotice(host, version)) return;
    pushNotification(n.level, n.title, { body: n.body });
  });
}

const NOTIFIED_KEY = "ps5upload.helper_copies_notified";

/** True the first time `host` is told about `version`; remembers it. */
export function firstUpdateNotice(host: string, version: string | null): boolean {
  let seen: Record<string, string> = {};
  try {
    seen = JSON.parse(safeGetItem(NOTIFIED_KEY) ?? "{}") as Record<string, string>;
  } catch {
    // A damaged value: start over.
  }
  const v = version ?? "";
  if (seen[host] === v) return false;
  seen[host] = v;
  safeSetItem(NOTIFIED_KEY, JSON.stringify(seen));
  return true;
}
