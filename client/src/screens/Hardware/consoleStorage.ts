import type { HwStorage } from "../../api/ps5";

/**
 * The figures of the Console Storage card: the partition games and files are on (`/user`, which
 * `/data` is a folder of), as its filesystem counts it. They are the same counters the Volumes
 * screen shows for `/data`, so the two screens say the same thing (#944).
 *
 * The card used to add the two system partitions in. Their free space is not room for anything
 * a person copies, so "Free" read some gigabytes high and a full console never showed zero.
 *
 * These are NOT the PS5's own Settings > Storage figures: that screen can show a smaller drive
 * and less free space. Nothing here tries to reproduce it.
 */
export interface ConsoleStorageRows {
  totalBytes: number;
  /** Everything that is not free: stored data and what the console holds back. */
  usedBytes: number;
  /** Free blocks the filesystem does not offer to a writer. Part of `usedBytes`; 0 = not reported. */
  heldBackBytes: number;
  /** What the filesystem offers a writer now. `heldBackBytes` is already left out of it. */
  freeBytes: number;
}

export function consoleStorageRows(s: HwStorage): ConsoleStorageRows {
  // A helper that sends no /user figures (none known) still gets its totals shown.
  const user = s.user_total_bytes > 0;
  const totalBytes = user ? s.user_total_bytes : s.total_bytes;
  const freeBytes = Math.min(totalBytes, user ? s.user_free_bytes : s.free_bytes);
  const heldBackBytes = user ? s.user_reserved_bytes : s.reserved_bytes;
  return {
    totalBytes,
    usedBytes: Math.max(0, totalBytes - freeBytes),
    heldBackBytes: Math.max(0, Math.min(heldBackBytes, totalBytes - freeBytes)),
    freeBytes,
  };
}
