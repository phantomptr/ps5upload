// Helpers for ShadowMount+'s watched manual-install list.
//
// SMP (drakmor/ShadowMountPlus) has no control API — it's an autonomous
// scanner. The one explicit "install THIS source" hook is the file
// `/data/shadowmount/manual.lst`: SMP watches it, and on edit it mounts +
// registers every source path it lists (a game folder or a
// .ffpkg/.exfat/.ffpfs/.ffpfsc image). So when SMP is running, ps5upload hands
// a game off by appending its PS5-side source path here instead of doing its
// own mount + register (which would race SMP for the same /user/app + app.db).
//
// The list is a small text file — one source path per line; `#` comments and
// blank lines ignored. We read-modify-write it (it's tiny).

/** Canonical on-console path of SMP's manual-install list. */
export const SMP_MANUAL_LIST_PATH = "/data/shadowmount/manual.lst";

/** Pure: given the current `manual.lst` text and a source `path`, return the
 *  new text with `path` appended on its own line — or `null` if it's already
 *  listed (so the caller can skip the write). Preserves existing lines,
 *  collapses any trailing whitespace to exactly one trailing newline, and
 *  dedups against non-comment lines (exact trimmed match). */
export function appendManualListLine(
  existing: string,
  path: string,
): string | null {
  const target = path.trim();
  if (!target) return null;
  for (const ln of existing.split(/\r?\n/)) {
    const t = ln.trim();
    if (t && !t.startsWith("#") && t === target) return null; // already present
  }
  const body = existing.replace(/\s+$/, ""); // strip trailing blank lines/spaces
  return (body ? body + "\n" : "") + target + "\n";
}
