import { create } from "zustand";

import { hostOf } from "../lib/addr";
import {
  classifyMovePollError,
  isExpectedNotInFlight,
} from "../lib/movePollerPolicy";

/**
 * A Games-row Move (copy on the console, then remove the source), kept outside the row.
 *
 * The move used to run inside the row with its progress in component state. The console
 * finished the move whatever screen was open, but leaving Games dropped the progress line
 * and the Stop button, and the outcome was shown to nobody. Here the run belongs to the
 * console and the entry: a row that mounts later finds it where it is.
 */

export type LibraryMovePhase =
  | "copying"
  | "deleting"
  | "done"
  | "cancelled"
  | "copy-failed"
  /** The copy landed but the source is still there: both copies exist. */
  | "delete-failed";

export interface LibraryMoveState {
  phase: LibraryMovePhase;
  from: string;
  to: string;
  bytesCopied: number;
  totalBytes: number;
  /** The console's error, raw, for the two failed phases. */
  error: string | null;
  /** Set when the helper is too old to report byte progress (which fix it lacks). */
  progressUnsupported: "2.2.7" | "2.2.16" | null;
  stopRequested: boolean;
  startedAtMs: number;
}

interface LibraryMoveStore {
  byKey: Record<string, LibraryMoveState>;
  put: (key: string, patch: Partial<LibraryMoveState>) => void;
  drop: (key: string) => void;
}

export const useLibraryMoveStore = create<LibraryMoveStore>((set) => ({
  byKey: {},
  put: (key, patch) =>
    set((s) => {
      const cur = s.byKey[key];
      if (!cur && patch.phase === undefined) return s;
      return {
        byKey: {
          ...s.byKey,
          [key]: { ...(cur as LibraryMoveState), ...patch },
        },
      };
    }),
  drop: (key) =>
    set((s) => {
      if (!(key in s.byKey)) return s;
      const next = { ...s.byKey };
      delete next[key];
      return { byKey: next };
    }),
}));

export const libraryMoveKey = (host: string, from: string) =>
  `${hostOf(host)}:${from}`;

export function libraryMove(
  s: { byKey: Record<string, LibraryMoveState> },
  key: string,
): LibraryMoveState | null {
  return s.byKey[key] ?? null;
}

/** Where a move that ended can be tried again: the same destination after a stop or a failed
 *  copy (the console removes what a stopped copy left, so the name is free again). Null once
 *  the copy has landed, when a second copy would be refused, and while one runs. */
export function moveRetryDest(m: LibraryMoveState | null): string | null {
  return m?.phase === "cancelled" || m?.phase === "copy-failed" ? m.to : null;
}

const running = (m: LibraryMoveState | null) =>
  m?.phase === "copying" || m?.phase === "deleting";

/** What the run needs from the app. Injected so it is testable without a console. */
export interface LibraryMoveDeps {
  copy: (addr: string, from: string, to: string, opId: number) => Promise<void>;
  opStatus: (
    addr: string,
    opId: number,
  ) => Promise<{ bytes_copied: number; total_bytes: number }>;
  opCancel: (addr: string, opId: number) => Promise<unknown>;
  /** Removes the source after a good copy (with its own retries). */
  deleteSource: (
    addr: string,
    path: string,
  ) => Promise<{ ok: boolean; lastError?: unknown }>;
  sleep: (ms: number) => Promise<void>;
  newOpId: () => number;
}

/** For a caller that mirrors the run elsewhere. Called whichever screen is open. */
export interface LibraryMoveHooks {
  /** The copy's op id, once chosen (the activity log's Stop needs it). */
  onStart?: (opId: number) => void;
  onProgress?: (bytesCopied: number, totalBytes: number) => void;
  onProgressUnsupported?: (threshold: "2.2.7" | "2.2.16") => void;
  onEnd?: (state: LibraryMoveState) => void;
}

export interface LibraryMoveRequest {
  host: string;
  addr: string;
  from: string;
  to: string;
  /** The running helper's version, for telling an old helper from a blip. */
  payloadVersion: string | null;
}

/** Runs the move to its end. Resolves when it ends, whichever way; the outcome is in the
 *  store under `libraryMoveKey(host, from)`. An entry already moving is left alone. */
export async function runLibraryMove(
  req: LibraryMoveRequest,
  deps: LibraryMoveDeps,
  hooks: LibraryMoveHooks = {},
): Promise<void> {
  const { addr, from, to } = req;
  const key = libraryMoveKey(req.host, from);
  const store = useLibraryMoveStore.getState();
  const now = () => libraryMove(useLibraryMoveStore.getState(), key);
  if (running(now())) return;
  store.put(key, {
    phase: "copying",
    from,
    to,
    bytesCopied: 0,
    totalBytes: 0,
    error: null,
    progressUnsupported: null,
    stopRequested: false,
    startedAtMs: Date.now(),
  });
  // The helper stamps the copy with this id: it is how progress is read and Stop is sent.
  const opId = deps.newOpId();
  hooks.onStart?.(opId);
  const end = (patch: Partial<LibraryMoveState>) => {
    store.put(key, patch);
    const s = now();
    if (s) hooks.onEnd?.(s);
  };

  let copyDone = false;
  const poller = (async () => {
    // The copy's request is still on its way: no point asking before it lands.
    await deps.sleep(250);
    let failures = 0;
    while (!copyDone) {
      try {
        const snap = await deps.opStatus(addr, opId);
        failures = 0;
        if (copyDone) break;
        store.put(key, {
          bytesCopied: snap.bytes_copied,
          totalBytes: snap.total_bytes,
        });
        hooks.onProgress?.(snap.bytes_copied, snap.total_bytes);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        // "Not in flight" is normal at the very start and the very end of a healthy copy.
        if (!isExpectedNotInFlight(msg)) {
          failures += 1;
          const outcome = classifyMovePollError(
            req.payloadVersion,
            msg,
            failures,
          );
          if (outcome.kind === "stop-old-payload") {
            store.put(key, { progressUnsupported: outcome.threshold });
            hooks.onProgressUnsupported?.(outcome.threshold);
            return;
          }
          if (outcome.kind === "stop-silent") {
            // The move carries on by itself; only its progress is lost.
            console.warn(
              `[library] FS_OP_STATUS poll gave up after ${failures} consecutive failures:`,
              msg,
            );
            return;
          }
          if (failures === 1)
            console.warn(
              "[library] FS_OP_STATUS poll failed (will retry):",
              msg,
            );
        }
      }
      await deps.sleep(500);
    }
  })();
  const stopWatcher = (async () => {
    while (!copyDone) {
      if (now()?.stopRequested) {
        // Best effort: the helper's copy checks the flag between buffers.
        await deps
          .opCancel(addr, opId)
          .catch((e) => console.warn("fsOpCancel (library move) failed:", e));
        return;
      }
      await deps.sleep(200);
    }
  })();

  let copyErr: unknown = null;
  try {
    await deps.copy(addr, from, to, opId);
  } catch (e) {
    copyErr = e ?? new Error("copy failed");
  } finally {
    copyDone = true;
    await Promise.allSettled([poller, stopWatcher]);
  }
  if (copyErr !== null) {
    const msg = copyErr instanceof Error ? copyErr.message : String(copyErr);
    // The helper answers a stopped copy with "cancelled": the user's Stop, not a failure.
    end(
      msg.includes("cancelled")
        ? { phase: "cancelled", error: null }
        : { phase: "copy-failed", error: msg },
    );
    return;
  }
  store.put(key, { phase: "deleting" });
  const del = await deps.deleteSource(addr, from);
  if (!del.ok) {
    const e = del.lastError;
    end({
      phase: "delete-failed",
      error: e instanceof Error ? e.message : String(e),
    });
    return;
  }
  end({ phase: "done", error: null });
}

/** Asks the console to stop the copy; the run then ends "cancelled" with the source intact.
 *  Too late once the source is being removed. */
export function stopLibraryMove(key: string): void {
  const s = useLibraryMoveStore.getState();
  if (libraryMove(s, key)?.phase === "copying")
    s.put(key, { stopRequested: true });
}

/** Forgets a finished move (the row showed its outcome). A running one stays. */
export function dismissLibraryMove(key: string): void {
  const s = useLibraryMoveStore.getState();
  if (running(libraryMove(s, key))) return;
  s.drop(key);
}
