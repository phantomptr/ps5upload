/**
 * Auto-resume-after-failure policy.
 *
 * The engine already retries at the SHARD level (`resumable_retry`, ~6 tries
 * with backoff). What this module governs is the QUEUE-runner level: when a
 * whole upload job dies — most importantly when the PS5 payload crashes
 * mid-transfer and the listener stops answering — should we automatically
 * heal and resume, or surface the failure?
 *
 * The recovery action (in uploadQueue's drain loop) is: wait a backoff, call
 * `ensurePayloadCurrent(host)` (which re-streams the payload ELF to :9021 if
 * the payload is down and waits until it reports healthy), then re-run the
 * upload. Because a resume re-runs reconcile — diffing local against what's
 * actually on the PS5 — the retry picks up exactly the unfinished files
 * (including the one that was mid-flight), so recovery is idempotent and can
 * never double-write.
 *
 * Auto-recovery only makes sense for failures a wait + re-deploy + resume can
 * actually fix: transport drops and payload-liveness problems. Failures that
 * are about the bytes/space/path themselves (out of space, file too big, path
 * rejected, source missing) would just loop on a guaranteed failure, so they
 * surface immediately. We default UNKNOWN reasons to recoverable — the attempt
 * budget bounds the wasted effort, and re-deploying a possibly-crashed payload
 * is cheap and safe.
 */

/** One initial attempt + this many automatic recovery attempts before the job
 *  is marked terminally failed and left for the manual Retry/Resume button. */
export const MAX_AUTO_RECOVER_ATTEMPTS = 3;

/** Backoff before each recovery attempt (index = attempt number, 0-based).
 *  Sized to give a crashed payload room to be re-sent and re-initialise
 *  (`ensurePayloadCurrent` itself then polls up to ~30 s for it to come up).
 *  The last value repeats if attempts somehow exceed the array. */
export const AUTO_RECOVER_BACKOFF_MS = [5_000, 15_000, 30_000] as const;

/** Backoff (ms) before recovery attempt `attempt` (0-based). */
export function autoRecoverBackoffMs(attempt: number): number {
  const i = Math.max(0, Math.min(attempt, AUTO_RECOVER_BACKOFF_MS.length - 1));
  return AUTO_RECOVER_BACKOFF_MS[i];
}

/** Payload `error_reason` substrings that are FATAL — retrying can't help, so
 *  auto-recovery is skipped and the failure surfaces immediately. Matched
 *  case-insensitively as substrings so `fs_write_failed_errno_28` etc. hit. */
const FATAL_REASON_SUBSTRINGS = [
  "errno_28", // ENOSPC — PS5 out of space
  "errno_27", // EFBIG — file exceeds the target filesystem's limit
  "insufficient_space",
  "no_space",
  "direct_writer_io_error", // humanised as "PS5 ran out of space mid-transfer"
  "path_not_allowed", // destination path the payload refuses to write
  "not_allowed",
  "tx_table_full", // payload is UP but saturated — re-deploy won't run, retry loops
  "corrupt", // direct_tx_corrupt etc. — data integrity, not transport
  "source_missing",
  "src_not_found",
] as const;

/** Local (no payload `reason`) error-message substrings that are FATAL. These
 *  are client-side problems a resume can't fix (the source moved, a local
 *  permission issue, the local disk filled). Connection-class messages
 *  (refused/reset/broken pipe/timeout) are intentionally absent — those ARE
 *  recoverable. Matched case-insensitively.
 *
 *  Kept deliberately SPECIFIC to local-filesystem failures. A bare "not found"
 *  was removed after hardware testing: a payload crash mid-commit surfaces
 *  "CommitTx rejected: tx_not_active" and a reconnect surfaces "host not
 *  found"-style transients — both are recoverable, and a broad "not found"
 *  would wrongly strand them. ENOENT on the *source* still matches via the
 *  precise "no such file" / "os error 2" forms. */
const FATAL_MESSAGE_SUBSTRINGS = [
  "no such file", // ENOENT — source file/dir is gone
  // NOTE: ENOENT's numeric form ("os error 2") is matched separately via a
  // digit-anchored regex below — a plain substring "os error 2" also matches
  // "os error 20".."os error 29", which includes the TRANSIENT, recoverable
  // EMFILE (24) / ENFILE (23) "too many open files" errors. Folding those in
  // here would strand a large directory upload that a wait+retry could clear.
  "permission denied", // local EACCES on the source
  "no space left", // local disk full
  "enospc",
] as const;

/** ENOENT only — `(os error 2)` not followed by another digit, so it won't
 *  swallow `os error 20`..`os error 29` (EMFILE/ENFILE etc. are recoverable). */
const FATAL_ENOENT_NUMERIC = /os error 2(?!\d)/;

/**
 * Thrown when a step AFTER the bytes are committed fails — the post-upload
 * install, or the post-upload mount.
 *
 * Auto-recovery re-runs the *whole* queue item, which for a pkg means
 * re-uploading it. That is correct for a transport failure and pure waste
 * once the transfer has already committed: the file is on the console, and
 * nothing about sending it again changes whether Sony's installer accepts it.
 *
 * A user on 5.17.6 watched exactly that: a 5.93 GiB update failed to install,
 * and six seconds later the app started re-uploading the same file with no
 * prompt. `isAutoRecoverable` defaults unknown failures to recoverable — the
 * right default for the transport errors it was written for, and the wrong
 * one for an install rejection, whose message it had never seen.
 *
 * So the runner marks these failures by type rather than by wording. Message
 * matching would be a losing game: install errors are translated into 19
 * languages.
 */
export class PostUploadStepError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PostUploadStepError";
  }
}

/**
 * Decide whether a failed upload job should be auto-recovered (wait +
 * re-deploy payload + resume) or surfaced as a terminal failure.
 *
 * @param reason  the payload's structured `error_reason`, or null for a
 *                local/connection failure that never reached the payload.
 * @param message the human error message (the only signal when `reason` is
 *                null).
 */
export function isAutoRecoverable(
  reason: string | null | undefined,
  message: string | null | undefined,
): boolean {
  const r = (reason ?? "").toLowerCase();
  if (FATAL_REASON_SUBSTRINGS.some((s) => r.includes(s))) return false;

  // Only consult the raw message for fatality when there's no structured
  // reason — a payload that gave us a (non-fatal) reason has already told us
  // the real category; the message may incidentally contain a fatal-looking
  // word in its detail text.
  if (!reason) {
    const m = (message ?? "").toLowerCase();
    if (FATAL_MESSAGE_SUBSTRINGS.some((s) => m.includes(s))) return false;
    if (FATAL_ENOENT_NUMERIC.test(m)) return false; // ENOENT, not os error 20-29
  }

  // Default: transport/liveness failure → recoverable. Bounded by
  // MAX_AUTO_RECOVER_ATTEMPTS so an unknown-but-truly-fatal error still
  // surfaces after a few cheap tries.
  return true;
}

/**
 * The decision the queue runner actually makes: never auto-recover a failure
 * that happened after the upload committed, otherwise apply the reason/message
 * policy above.
 */
export function shouldAutoRecover(
  err: unknown,
  reason: string | null | undefined,
  message: string | null | undefined,
): boolean {
  if (err instanceof PostUploadStepError) return false;
  return isAutoRecoverable(reason, message);
}
