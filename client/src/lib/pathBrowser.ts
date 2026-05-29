// Pure helpers for the in-app file/folder browser (LocalPathPicker).
// Kept out of the component so they're unit-testable without a DOM.

/** Parent directory of a POSIX path, or null at the filesystem root. */
export function parentOf(path: string): string | null {
  if (!path || path === "/") return null;
  const trimmed = path.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  if (idx < 0) return null;
  return idx === 0 ? "/" : trimmed.slice(0, idx);
}

/** Human-readable byte size (e.g. "85.3 GB") for the file list. */
export function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 10 || i === 0 ? 0 : 1)} ${u[i]}`;
}
