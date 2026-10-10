// Pure helpers for the in-app file/folder browser (LocalPathPicker).
// Kept out of the component so they're unit-testable without a DOM.

import { formatBytes } from "./format";

/** Parent directory of a POSIX path, or null at the filesystem root. */
export function parentOf(path: string): string | null {
  if (!path || path === "/") return null;
  const trimmed = path.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  if (idx < 0) return null;
  return idx === 0 ? "/" : trimmed.slice(0, idx);
}

/** Human-readable byte size (e.g. "85.3 GiB") for the file list: the app's
 *  one formatter, so a size reads the same here as on every other screen. */
export const fmtSize = formatBytes;
