// Helpers for reasoning about PS5 mount paths.
//
// Removable mounts (`/mnt/usb*`, `/mnt/ext*`) are special in two ways the
// app cares about: a drive can be unplugged out from under a browsed path
// (File System falls back to /data), and Sony's installer can't install a
// .pkg directly off an exfat USB mount (pkgLibrary stages it to internal
// storage first). Both used to inline the same regex; centralizing it keeps
// the "what counts as removable" rule in one tested place.

/** True if a console path lives on a removable USB / external drive. */
export function isRemovableMount(path: string): boolean {
  return /^\/mnt\/(usb|ext)/i.test(path);
}

/** The removable-drive mount root for a path
 *  (`/mnt/usb0/games/x.pkg` → `/mnt/usb0`), or null when the path isn't on
 *  a removable mount. */
export function removableMountRoot(path: string): string | null {
  return path.match(/^(\/mnt\/(?:usb|ext)[^/]*)/i)?.[1] ?? null;
}

/** Root ShadowMount+ mounts every image under. Burned into SMP's source
 *  (sm_config_mount.c) — not user-configurable. */
export const SMP_MOUNT_ROOT = "/mnt/shadowmnt";

/**
 * Recover the source image's basename from a ShadowMount+ mount point.
 *
 * SMP names each mount `<image basename without extension>_<crc32 hex>` —
 * `/mnt/shadowmnt/PPSA09016_ca51a0d7` came from a file called
 * `PPSA09016.exfat`. It never records the original path anywhere we can read,
 * so this naming rule is the only link between a mounted title and the disk
 * image row it belongs to.
 *
 * Returns null for anything that isn't an SMP mount point in that exact shape,
 * so a future SMP naming scheme degrades to "no match" rather than a wrong one.
 *
 * Note the hash is over the full source path, so two images with the SAME
 * basename in different folders produce different mount points that both map
 * back to that one basename. They're the same title in practice; the caller
 * treats a basename collision as a match for either row.
 */
export function smpMountImageBasename(mountPoint: string): string | null {
  const prefix = `${SMP_MOUNT_ROOT}/`;
  if (!mountPoint.startsWith(prefix)) return null;
  // SMP nests some backends one level deeper (e.g. `/mnt/shadowmnt/pfsc/<x>`);
  // the leaf is always the `<name>_<crc32>` component.
  const leaf = mountPoint.slice(prefix.length).split("/").filter(Boolean).pop();
  if (!leaf) return null;
  const m = leaf.match(/^(.+)_([0-9a-f]{8})$/);
  return m ? m[1] : null;
}

/** A path's filename with any single trailing extension removed
 *  (`/mnt/usb0/homebrew/PPSA09016.exfat` → `PPSA09016`). Used to match a
 *  disk-image row against {@link smpMountImageBasename}. */
export function imageBasename(path: string): string {
  const leaf = path.split("/").pop() ?? "";
  return leaf.replace(/\.[^.]+$/, "");
}
