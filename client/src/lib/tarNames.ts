// Tar archives and lone .zst files the engine unpacks on this computer as it uploads them
// (`/api/transfer/tar`): a tar plain or compressed with zstd, gzip, xz or bzip2, and one file
// compressed with zstd. Keep in step with `TAR_SUFFIXES` in
// engine/crates/ps5upload-core/src/tar_archive.rs.

/** The endings, longest first, so {@link tarSuffix} names the whole ending. */
export const TAR_SUFFIXES = [
  ".tar.zst",
  ".tar.bz2",
  ".tar.gz",
  ".tar.xz",
  ".tbz2",
  ".tzst",
  ".tar",
  ".tbz",
  ".tgz",
  ".txz",
  ".zst",
] as const;

/** The ending `name` has from {@link TAR_SUFFIXES} (".tar.zst", ".tgz", ".zst"), or null. */
export function tarSuffix(name: string): string | null {
  const lower = name.toLowerCase();
  return TAR_SUFFIXES.find((s) => lower.endsWith(s)) ?? null;
}

/** True for a tar (plain or compressed) or a lone `.zst`. */
export function isTarArchiveName(name: string): boolean {
  return tarSuffix(name) !== null;
}

/** `name` without its tar ending (`Game.tar.zst` → `Game`, `disc.exfat.zst` → `disc.exfat`). */
export function stripTarSuffix(name: string): string {
  const s = tarSuffix(name);
  return s ? name.slice(0, name.length - s.length) : name;
}
