/**
 * Trigger a browser download of in-memory text via a Blob + anchor click.
 *
 * Desktop/Tauri callers must NOT use this: the Tauri webview intercepts an
 * anchor `download` click and routes it through `plugin:fs|write_text_file`,
 * which the app's capability ACL blocks. That's a Tauri-webview-only quirk —
 * a real browser has no such interception, so this is the browser-only
 * counterpart to the native save-dialog path (see e.g.
 * `screens/Logs/AppLogsPanel.tsx`'s `downloadAll`).
 */
export function browserDownloadText(
  fileName: string,
  text: string,
  mimeType = "text/plain",
): void {
  const blob = new Blob([text], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = fileName;
  a.click();
  URL.revokeObjectURL(url);
}
