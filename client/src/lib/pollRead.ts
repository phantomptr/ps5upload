// One status read that fails is not a job that failed. The engine runs a job by itself, so a
// failed read (the engine restarting, a proxy blip, a busy helper) says nothing about it. Ending
// the flow there tells the user "failed" while the job carries on, a retry can collide with it,
// and a caller's clean-up can delete what the job is still using.

import { sleep as wakeableSleep } from "./wakeableSleep";

/** Status reads that may fail in a row before the caller gives up on the job. */
export const MAX_POLL_FAILURES = 6;
const WAIT_AFTER_MISS_MS = 1_000;

/** The engine answered that it has no such job: it restarted, or the job ended long ago. Asking
 *  again cannot change that. */
export function isJobUnknownError(e: unknown): boolean {
  return /job not found/i.test(e instanceof Error ? e.message : String(e));
}

/** Runs one status read, asking again while it fails. Rejects with the last error after
 *  MAX_POLL_FAILURES in a row, or at once when the engine does not know the job. */
export async function readWithRetries<T>(
  read: () => Promise<T>,
  sleep: (ms: number) => Promise<void> = wakeableSleep,
): Promise<T> {
  for (let misses = 1; ; misses++) {
    try {
      return await read();
    } catch (e) {
      if (isJobUnknownError(e) || misses >= MAX_POLL_FAILURES) throw e;
      await sleep(WAIT_AFTER_MISS_MS);
    }
  }
}
