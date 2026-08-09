/** Native Tauri drag/drop can be observed by AppShell while routing and again
 * by the freshly-mounted Install Package screen. Keep a very short per-path
 * receipt window so one physical drop starts one upload. */
export const PKG_DROP_DEDUPE_MS = 2_500;

export function acceptPkgDrop(
  recent: Map<string, number>,
  path: string,
  nowMs = Date.now(),
): boolean {
  const normalized = path.trim();
  if (!normalized) return false;
  const previous = recent.get(normalized);
  if (previous !== undefined && nowMs - previous < PKG_DROP_DEDUPE_MS) {
    return false;
  }
  recent.set(normalized, nowMs);
  // Bound a long-running screen's map even if thousands of unique files are
  // dropped over time.
  for (const [p, seen] of recent) {
    if (nowMs - seen >= PKG_DROP_DEDUPE_MS) recent.delete(p);
  }
  return true;
}
