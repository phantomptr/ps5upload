import { invoke } from "@tauri-apps/api/core";

/**
 * Drop-in replacement for `@tauri-apps/plugin-fs`'s `writeTextFile(path,
 * contents)` for the case where `path` is an absolute path the user just
 * picked through the save dialog.
 *
 * The fs plugin enforces *path* scopes, so writing to a dialog-chosen
 * path (e.g. `~/Downloads/…`) is rejected with "fs.write_text_file not
 * allowed". This routes the write through the backend `save_text_file`
 * command instead, which writes from trusted Rust (same pattern as the
 * save-archive zip path). See client/src-tauri/src/commands/save_text_file.rs.
 *
 * Signature intentionally matches plugin-fs `writeTextFile` so call
 * sites only swap the import.
 */
export async function writeTextFileToPath(
  path: string,
  contents: string,
  // The filename the caller offered as the dialog's defaultPath. On Android
  // `path` is a content:// URI with no usable name, so the backend uses this
  // to keep distinct exports from colliding on one fixed Downloads filename.
  // Pass it whenever you have it; desktop ignores it.
  fileName?: string,
): Promise<void> {
  await invoke("save_text_file", { path, contents, destFilename: fileName });
}

/**
 * Drop-in replacement for plugin-fs `readTextFile(path)` when `path`
 * came from the open dialog. Same fs-scope reason as
 * {@link writeTextFileToPath}: opening a file via the dialog plugin does
 * not grant the fs plugin read access to it. Reads through the backend
 * `read_text_file` command (16 MiB cap enforced in Rust).
 */
export async function readTextFileFromPath(path: string): Promise<string> {
  return invoke<string>("read_text_file", { path });
}
