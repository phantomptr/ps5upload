// Where an export (settings backup, logs, a bug report, a save backup, a folder as .zip...) goes.
//
// Desktop: the system save dialog. `null` means the person cancelled.
//
// Android: no dialog either. The dialog answered with a content:// URI the backend cannot write,
// so the backend wrote the file into Downloads instead (save_dest.rs), and the empty file the
// dialog had already created where the person pointed stayed behind: "the bug report zip is zero
// bytes". The file goes straight into Downloads, the folder every Android file manager shows,
// under the suggested name (" (2)", " (3)"... when taken).
//
// iPhone: the save dialog does not hand back a path this app can write, so there is no dialog:
// the file goes into the app's own Documents folder, the one the Files app shows under
// On My iPhone → PS5Upload, under the suggested name (with " (2)", " (3)"... when taken).
import { localFs } from "../api/localFs";
import { isAndroid, isIOS } from "./platform";

export interface SaveDestOptions {
  defaultPath?: string;
  filters?: { name: string; extensions: string[] }[];
  title?: string;
}

export async function pickSaveDest(
  opts: SaveDestOptions,
): Promise<string | null> {
  if (isIOS()) return documentsDest(opts.defaultPath ?? "ps5upload-export");
  if (isAndroid())
    return freeDest(await androidDownloads(), opts.defaultPath ?? "ps5upload-export");
  const { save } = await import("@tauri-apps/plugin-dialog");
  return save(opts);
}

/** A free name for `suggested` in the downloads folder, with no dialog (a screenshot taken
 *  mid-game): Downloads on the desktop and Android, the app's Documents folder on iPhone. */
export async function downloadsDest(suggested: string): Promise<string> {
  if (isIOS()) return documentsDest(suggested);
  if (isAndroid()) return freeDest(await androidDownloads(), suggested);
  const { downloadDir } = await import("@tauri-apps/api/path");
  return freeDest(await downloadDir(), suggested);
}

/** Android's public Downloads folder, for this user: a work profile or a second user has another
 *  number there. The literal is the primary user's, if Android cannot say. */
async function androidDownloads(): Promise<string> {
  return localFs.downloadsDir().catch(() => "/storage/emulated/0/Download");
}

/** A free name in the iPhone app's Documents folder. Exported for tests. */
export async function documentsDest(suggested: string): Promise<string> {
  const [docs] = await localFs.storageRoots();
  return freeDest(docs, suggested);
}

/** A free name for `suggested` in `dir`: the name itself, else "name (2).ext", "name (3).ext"…
 *  A folder that cannot be listed gets the name as it is. Exported for tests. */
export async function freeDest(
  dir: string,
  suggested: string,
): Promise<string> {
  const leaf = suggested.split(/[\\/]/).pop()?.trim() || "ps5upload-export";
  const docs = dir;
  const taken = new Set(
    (await localFs.listDir(docs).catch(() => [])).map((e) => e.name),
  );
  const dot = leaf.lastIndexOf(".");
  const stem = dot > 0 ? leaf.slice(0, dot) : leaf;
  const ext = dot > 0 ? leaf.slice(dot) : "";
  let name = leaf;
  for (let n = 2; taken.has(name); n++) name = `${stem} (${n})${ext}`;
  return `${docs.replace(/\/+$/, "")}/${name}`;
}
