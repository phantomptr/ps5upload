// A folder job is continued under its old id so the console's journal skips what it has. The
// journal does not look at the disk: when the staged tree has been deleted (the app says "free up
// space, then Retry", and that is what a person deletes), the console would count every file it
// remembers as done and publish a tree with those files missing, with status OK. So the disk is
// asked first, and a job whose tree is gone starts over under a new id.

export type ListDir = (addr: string, path: string) => Promise<{ name: string }[]>;

/** Whether what `dest`'s job wrote is certainly gone: neither `<dest>.ava-part` nor anything in
 *  `<dest>`. Any doubt (the console does not answer, a listing fails) says no: starting over
 *  throws away real progress and leaves the old tree behind. */
export async function stagedTreeGone(
  addr: string,
  dest: string,
  listDir: ListDir,
): Promise<boolean> {
  const trimmed = dest.replace(/\/+$/, "");
  const slash = trimmed.lastIndexOf("/");
  if (slash < 0) return false;
  const name = trimmed.slice(slash + 1);
  let names: Set<string>;
  try {
    names = new Set((await listDir(addr, trimmed.slice(0, slash) || "/")).map((e) => e.name));
  } catch {
    return false;
  }
  if (names.has(`${name}.ava-part`)) return false;
  if (!names.has(name)) return true;
  // Only the empty folder that held its place: the rename to it never happened.
  try {
    return (await listDir(addr, trimmed)).length === 0;
  } catch {
    return false;
  }
}

/** `txId`, or a new id from `fresh` when the job's staged tree is gone. On a job that has not run
 *  yet that changes nothing: the old id was unused. */
export async function resumableTxId(
  addr: string,
  dest: string,
  txId: string,
  fresh: () => string,
  listDir: ListDir,
): Promise<string> {
  return (await stagedTreeGone(addr, dest, listDir)) ? fresh() : txId;
}
