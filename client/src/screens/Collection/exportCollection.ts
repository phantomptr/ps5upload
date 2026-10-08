import { collection } from "../../api/collection";
import { browserDownloadText } from "../../lib/browserDownload";
import { isTauriEnv } from "../../lib/tauriEnv";

const MIME = {
  csv: "text/csv",
  md: "text/markdown",
  json: "application/json",
} as const;

/** Saves an export: the native save dialog on the desktop, a download in the web UI. */
export async function exportCollection(
  format: "csv" | "md" | "json",
): Promise<void> {
  const text = await collection.exportText(format);
  const fileName = `game-collection.${format}`;
  if (!isTauriEnv()) {
    browserDownloadText(fileName, text, MIME[format]);
    return;
  }
  const { save } = await import("@tauri-apps/plugin-dialog");
  const { writeTextFileToPath } = await import("../../lib/saveTextFile");
  const dest = await save({
    defaultPath: fileName,
    filters: [{ name: format.toUpperCase(), extensions: [format] }],
  });
  if (dest) await writeTextFileToPath(dest, text, fileName);
}
