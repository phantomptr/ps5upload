/**
 * Session cache for cover art fetched as `data:` URLs.
 *
 * The engine caches the image bytes on disk, so a repeat request is a local
 * read rather than a console round-trip. What that does NOT avoid is the
 * base64 encode and the IPC hop, which is paid again for every cover every
 * time a screen is mounted — and the desktop app takes that path for all of
 * them, because its window refuses the direct URL.
 *
 * Holding the finished strings for the session removes that entirely:
 * navigating back to a screen you just left costs nothing.
 *
 * Deliberately session-only and bounded. Freshness is the engine's job (it
 * has TTLs and drops a console's artwork on install/uninstall); this layer
 * only avoids repeating work for bytes we already hold. Anything longer
 * lived would need its own invalidation story to go wrong.
 */

/** Cover art runs ~290 KB, ~390 KB once base64'd. This holds roughly a
 *  screenful and a half before it starts evicting. */
const MAX_BYTES = 24 * 1024 * 1024;

const entries = new Map<string, string>();
let heldBytes = 0;

/** Approximate retained size of a data URL. Each base64 char is one byte
 *  of the string; exactness does not matter for a budget. */
function sizeOf(dataUrl: string): number {
  return dataUrl.length;
}

export function getCachedIcon(key: string): string | undefined {
  return entries.get(key);
}

export function setCachedIcon(key: string, dataUrl: string): void {
  if (!dataUrl) return;
  const size = sizeOf(dataUrl);
  // A single image larger than the whole budget would evict everything and
  // then not fit; skip it rather than thrash.
  if (size > MAX_BYTES) return;

  if (entries.has(key)) heldBytes -= sizeOf(entries.get(key) as string);
  entries.set(key, dataUrl);
  heldBytes += size;

  // Insertion order eviction. `Map` iterates in insertion order, so the
  // first key is the oldest — no bookkeeping needed.
  while (heldBytes > MAX_BYTES) {
    const oldest = entries.keys().next();
    if (oldest.done) break;
    heldBytes -= sizeOf(entries.get(oldest.value) as string);
    entries.delete(oldest.value);
  }
}

/** Drop everything. Called when the user clears cached artwork, so the UI
 *  does not keep showing images the engine has just deleted. */
export function clearIconMemoryCache(): void {
  entries.clear();
  heldBytes = 0;
}

/** For tests and diagnostics. */
export function iconMemoryCacheStats(): { count: number; bytes: number } {
  return { count: entries.size, bytes: heldBytes };
}
