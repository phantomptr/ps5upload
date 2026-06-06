import { toPng } from "html-to-image";
import { invoke } from "@tauri-apps/api/core";

import { log } from "../state/logs";

/**
 * Capture the current app view to a PNG and persist it for bug reports.
 *
 * DOM → image (in JS) is deliberate: it's the ONE capture path that works
 * identically across every webview the app runs in — WKWebView (macOS),
 * WebView2 (Windows), WebKitGTK (Linux) and Android WebView — with no native
 * screenshot API (Tauri has none, least of all on Android) and no OS
 * permission prompt. It captures the app's own UI, which is exactly what a bug
 * report wants to show.
 *
 * Saved to `~/.ps5upload/bug-screenshots/` (app-private dir on mobile) with a
 * datetime name; the file's real path then flows into the bug-report zip like
 * any other attachment.
 */

export interface SavedShot {
  name: string;
  path: string;
  bytes: number;
  /** data:image/png;base64,… for an instant thumbnail. */
  data_url: string;
}

/** `screenshot-YYYYMMDD-HHMMSS-mmm.png` in local time. The millis suffix keeps
 *  two captures in the same second from colliding. */
function fileName(d: Date): string {
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return (
    `screenshot-${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}` +
    `-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}` +
    `-${p(d.getMilliseconds(), 3)}.png`
  );
}

/**
 * Capture the whole app (the #root subtree, falling back to body) and save it.
 * Throws on failure (the caller shows feedback); logs either way so the action
 * itself appears in the bug log.
 */
export async function captureAppScreenshot(): Promise<SavedShot> {
  const node =
    (typeof document !== "undefined" &&
      (document.getElementById("root") || document.body)) ||
    null;
  if (!node) throw new Error("no DOM to capture");

  let dataUrl: string;
  try {
    // pixelRatio 1: capture at CSS-pixel size, not the 2–3× device ratio —
    // keeps the PNG to a few hundred KB instead of multiple MB on hi-dpi.
    // cacheBust avoids stale cached <img> bytes; skipFonts dodges a class of
    // cross-origin font-inlining failures that can otherwise reject the whole
    // capture in WebKit.
    dataUrl = await toPng(node as HTMLElement, {
      pixelRatio: 1,
      cacheBust: true,
      skipFonts: true,
    });
  } catch (e) {
    log.error(
      "screenshot",
      `capture failed: ${e instanceof Error ? e.message : String(e)}`,
    );
    throw e;
  }

  const base64 = dataUrl.split(",", 2)[1] ?? "";
  if (!base64) {
    log.error("screenshot", "capture produced empty image");
    throw new Error("capture produced empty image");
  }
  const name = fileName(new Date());
  try {
    const shot = await invoke<SavedShot>("screenshot_save", {
      name,
      base64Png: base64,
    });
    log.info(
      "screenshot",
      `captured ${name} (${Math.round(shot.bytes / 1024)} KB)`,
    );
    return shot;
  } catch (e) {
    log.error(
      "screenshot",
      `saving capture failed: ${e instanceof Error ? e.message : String(e)}`,
    );
    throw e;
  }
}
