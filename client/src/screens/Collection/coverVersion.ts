import type { CollectionLocation } from "../../api/collection";

/**
 * A version for one game's cover, for the `?v=` on its URL.
 *
 * The engine reads a cover out of the game's own files, and only again when
 * those are new or a deep rescan forces it. Versioning every cover by the
 * library's scan time made each rescan throw away every cover the webview had
 * cached and fetch them all again; versioning by the files the cover comes
 * from changes one game's URL only when that game changed.
 */
export function coverVersion(
  game: { local_cover?: string; locations?: Pick<CollectionLocation, "absolute_path" | "size_bytes" | "modified_at">[] },
): string | undefined {
  if (!game.local_cover) return undefined;
  const parts = [game.local_cover];
  for (const l of [...(game.locations ?? [])].sort((a, b) => a.absolute_path.localeCompare(b.absolute_path))) {
    parts.push(`${l.absolute_path}\u0000${l.size_bytes}\u0000${l.modified_at ?? ""}`);
  }
  // FNV-1a, 32-bit: short, stable, and plenty to tell two states of one game apart.
  let h = 0x811c9dc5;
  const s = parts.join("\u0001");
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(36);
}
