// Copy checks (the engine's /api/collection/check): started from a game's page or the
// Collection, polled only while one is queued or running.

import { create } from "zustand";

import { collection, type CopyCheck } from "../api/collection";

interface CopyChecksState {
  byPath: Record<string, CopyCheck>;
  error: string | null;
}

export const useCopyChecks = create<CopyChecksState>(() => ({
  byPath: {},
  error: null,
}));

const POLL_MS = 1000;
const RETRY_AFTER_ERROR_MS = 5000;
let timer: ReturnType<typeof setTimeout> | null = null;

function put(checks: CopyCheck[]) {
  useCopyChecks.setState({
    byPath: Object.fromEntries(checks.map((c) => [c.path, c])),
    error: null,
  });
  const active = checks.some(
    (c) => c.state === "queued" || c.state === "running",
  );
  if (active && !timer) {
    timer = setTimeout(() => {
      timer = null;
      void refreshCopyChecks();
    }, POLL_MS);
  }
}

export async function refreshCopyChecks(): Promise<void> {
  try {
    put((await collection.checks()).checks);
  } catch (e) {
    useCopyChecks.setState({
      error: e instanceof Error ? e.message : String(e),
    });
    // A check that was running may still be: keep looking, or its row stays "running".
    const active = Object.values(useCopyChecks.getState().byPath).some(
      (c) => c.state === "queued" || c.state === "running",
    );
    if (active && !timer) {
      timer = setTimeout(() => {
        timer = null;
        void refreshCopyChecks();
      }, RETRY_AFTER_ERROR_MS);
    }
  }
}

/** Checks these copies, one after another on the engine. */
export async function startCopyChecks(paths: string[]): Promise<void> {
  if (paths.length === 0) return;
  try {
    put((await collection.check(paths)).checks);
  } catch (e) {
    useCopyChecks.setState({
      error: e instanceof Error ? e.message : String(e),
    });
  }
}

/** Stops one copy's check, or every check. */
export async function cancelCopyChecks(path?: string): Promise<void> {
  try {
    put((await collection.checkCancel(path)).checks);
  } catch (e) {
    useCopyChecks.setState({
      error: e instanceof Error ? e.message : String(e),
    });
  }
}

/** One copy's check, when it has had one this engine session. */
export function useCopyCheck(path: string): CopyCheck | undefined {
  return useCopyChecks((s) => s.byPath[path]);
}

/** Percent through a running check, or null when its size is not known yet. */
export function checkPercent(
  c: Pick<CopyCheck, "bytes_done" | "bytes_total">,
): number | null {
  if (!c.bytes_total) return null;
  return Math.min(100, Math.floor((c.bytes_done / c.bytes_total) * 100));
}
