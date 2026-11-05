export interface UploadPartDeps {
  /** Throws unless `path` is a folder on the console. */
  listDir: (addr: string, path: string) => Promise<unknown[]>;
  remove: (addr: string, path: string) => Promise<void>;
}

/**
 * Deletes the part or staging tree of an upload to `dest`, and the empty folder holding a staged
 * upload's place. `<dest>` is only ever removed when a staging tree proves the job made it, and
 * only when empty: anywhere else it can be the user's own folder. Throws when nothing was there
 * to delete or the console refused; the caller decides whether that matters.
 */
export async function removeUploadPart(
  addr: string,
  dest: string,
  deps: UploadPartDeps,
): Promise<void> {
  const part = `${dest}.ava-part`;
  const staged = await deps.listDir(addr, part).then(
    () => true,
    () => false,
  );
  await deps.remove(addr, part);
  if (!staged) return;
  const inDest = await deps.listDir(addr, dest).catch(() => null);
  if (inDest && inDest.length === 0) await deps.remove(addr, dest);
}
