// Bounded-retry wrapper around a single fs_delete attempt.
//
// Used by the Library "move" flow (copy → verify-by-success →
// delete-with-retry). The payload's fs_delete can fail transiently if
// something on the PS5 side still has the source dir open after a copy
// — the linear-backoff retry gives the kernel a moment to release the
// handle without making the user re-trigger the operation.
//
// Pulled out of the screen so the policy (3 attempts, 500ms × n
// linear backoff, no jitter) is testable in isolation. The screen
// supplies the deleter and a `sleep` injection so tests can run
// without burning real time.
//
// Returns the final result. On success: `{ ok: true, attempts }`.
// On exhausted failure: `{ ok: false, attempts, lastError }`. The
// caller decides how to surface the failure (the Library screen tells
// the user "both copies now exist — delete the original yourself").

export interface DeleteWithRetryOptions {
  deleter: () => Promise<void>;
  /** How many total attempts. Defaults to 3. */
  maxAttempts?: number;
  /** Per-attempt linear backoff base in milliseconds. Defaults to
   *  500 (so attempt 2 waits 500ms, attempt 3 waits 1000ms). */
  backoffMs?: number;
  /** Async sleep. Defaults to setTimeout. Inject a fake in tests. */
  sleep?: (ms: number) => Promise<void>;
  /** Optional per-attempt failure callback for logging — fires for
   *  every failed attempt (including the final one). */
  onAttemptFail?: (attempt: number, error: unknown) => void;
}

export type DeleteWithRetryResult =
  | { ok: true; attempts: number }
  | { ok: false; attempts: number; lastError: unknown };

const defaultSleep = (ms: number) =>
  new Promise<void>((resolve) => setTimeout(resolve, ms));

export async function deleteWithRetry(
  opts: DeleteWithRetryOptions,
): Promise<DeleteWithRetryResult> {
  const {
    deleter,
    maxAttempts = 3,
    backoffMs = 500,
    sleep = defaultSleep,
    onAttemptFail,
  } = opts;

  let lastError: unknown = null;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    try {
      await deleter();
      return { ok: true, attempts: attempt };
    } catch (e) {
      lastError = e;
      if (onAttemptFail) onAttemptFail(attempt, e);
      if (attempt < maxAttempts) {
        // Linear backoff — short enough to be invisible to the user
        // unless we're persistently losing the race against the PS5
        // releasing its file handle.
        await sleep(backoffMs * attempt);
      }
    }
  }
  return { ok: false, attempts: maxAttempts, lastError };
}
