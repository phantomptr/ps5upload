// Pure logic for the FileSystem bulk-delete loop.
//
// The screen wraps this with the React state, the in-flight store
// (`useFsBulkOpStore`), the modal confirm, and the post-op refresh.
// Pulling the loop body out gives us:
//
//   1. A testable contract: assert ordering, error aggregation, and
//      progress callback invariants without mounting a screen.
//   2. A single place to change the per-item failure policy. The
//      contract today is "continue on error and report the first",
//      because in a multi-select delete the user typically wants the
//      survivors removed rather than aborting at item one.
//
// The function is intentionally framework-free — no Zustand, no React,
// no fetch — so the test can stay a pure unit test (vitest, no jsdom).
//
// Path joining is callable-injected so the caller controls the
// host-prefix and join-rules, keeping this module unaware of the
// PS5 payload's URL shape.

export interface BulkDeleteProgress {
  /** Items completed so far, [0..total]. Monotonic. */
  done: number;
  /** Joined path of the item currently being processed. */
  currentPath: string;
  /** Display name (last segment) of the item currently being processed. */
  currentName: string;
  /** Pre-fetched size, if known. `null` when the entry was not in the
   *  pre-op listing (e.g. user re-typed a name into selection). */
  currentSize: number | null;
}

export interface BulkDeleteOptions {
  /** Names of the items to delete, in deletion order. */
  names: string[];
  /** Directory containing the items. */
  basePath: string;
  /** Per-item size lookup. Used to populate progress.currentSize. */
  sizeByName: Map<string, number>;
  /** Joins the directory path with a name. */
  joinPath: (dir: string, name: string) => string;
  /** Performs the delete for a single resolved path. */
  deleter: (itemPath: string) => Promise<void>;
  /** Receives a progress event before each per-item delete attempt. */
  onProgress: (p: BulkDeleteProgress) => void;
  /** Optional cancel-checker invoked before each per-item delete.
   *  Returning `true` aborts the loop after the previous item
   *  completes (the in-flight `fs_delete` RPC is single-shot and
   *  not interruptible). Omit for non-cancellable callers. */
  shouldCancel?: () => boolean;
}

export interface BulkDeleteResult {
  /** First per-item failure (`"<name>: <message>"`), or null on full
   *  success. The screen surfaces this in the error banner. */
  firstError: string | null;
  /** Number of items that failed. The screen uses this to decide
   *  whether the post-op refresh is "everything went through" vs
   *  "partial — re-list to see what survived". */
  failureCount: number;
  /** True when the caller requested cancel (via shouldCancel) and
   *  the loop bailed early. UI uses this to surface a "stopped after
   *  N of M" banner instead of a partial-success message. */
  cancelled: boolean;
}

/**
 * Run a bulk delete: iterate names in order, attempt each delete,
 * aggregate failures.
 *
 * Failure policy: continue on per-item error, report the first error
 * verbatim. The user picked N items and probably wants surviving
 * items removed even if one is, e.g., locked by the PS5 OS.
 */
export async function runBulkDelete(
  opts: BulkDeleteOptions,
): Promise<BulkDeleteResult> {
  const { names, basePath, sizeByName, joinPath, deleter, onProgress } = opts;

  let firstError: string | null = null;
  let failureCount = 0;
  let cancelled = false;

  for (let i = 0; i < names.length; i++) {
    if (opts.shouldCancel && opts.shouldCancel()) {
      cancelled = true;
      break;
    }
    const name = names[i];
    const itemPath = joinPath(basePath, name);
    onProgress({
      done: i,
      currentPath: itemPath,
      currentName: name,
      currentSize: sizeByName.get(name) ?? null,
    });
    try {
      await deleter(itemPath);
    } catch (e) {
      failureCount += 1;
      if (firstError === null) {
        const msg = e instanceof Error ? e.message : String(e);
        firstError = `${name}: ${msg}`;
      }
    }
  }

  return { firstError, failureCount, cancelled };
}
