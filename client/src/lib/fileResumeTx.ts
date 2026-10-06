import { resumeTxidLookup, resumeTxidRemember } from "../api/ps5";

/** The job id a single-file upload of `src` to `dest` on `host` should run under.
 *
 *  The console keeps a failed upload's `.ava-part` and journal under the job id it was
 *  started with, and the free-space check only credits them to that same id. A second
 *  attempt under a new id is a new job: it asks for room for the whole file while the
 *  kept partial still occupies it (#401). So an earlier attempt's id is reused when one
 *  is remembered; otherwise `fresh` is remembered and returned.
 *
 *  The store is a convenience (it does not exist in the browser build): any failure
 *  falls back to `fresh`, which is exactly the old behaviour. */
export async function fileResumeTxId(
  host: string,
  src: string,
  dest: string,
  fresh: string,
): Promise<string> {
  try {
    const earlier = await resumeTxidLookup(host, src, dest);
    if (earlier) return earlier;
  } catch {
    /* no store: a fresh job */
  }
  try {
    await resumeTxidRemember(host, src, dest, fresh, "file");
  } catch {
    /* not remembered: this attempt still runs, only a later one starts over */
  }
  return fresh;
}
