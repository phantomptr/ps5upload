import { isTauriEnv } from "./tauriEnv";

/**
 * Stop an accidental WebView reload from tearing the user away from in-flight
 * work.
 *
 * The desktop WebView exposes a native right-click menu (Reload / Back /
 * Inspect) and the usual reload shortcuts (Ctrl/Cmd+R, F5). Triggering a
 * reload restarts the React app: the progress UI for a running upload /
 * install disappears and its polling stops, which reads to the user as "it
 * stopped everything." Suppress both paths so a stray right-click or keypress
 * can't do it.
 *
 * Scope:
 *  - Tauri desktop only. A plain browser tab (dev server in a browser) keeps
 *    its normal controls, and Android has no right-click / F5.
 *  - The right-click menu (which carries Back / Reload / Inspect) is suppressed
 *    in BOTH dev and production so the reported reload path is gone — and so it
 *    can be verified during `tauri dev`. Editable fields keep their native
 *    menu so copy / paste / spellcheck still work. The devtools keyboard
 *    shortcut (Cmd/Ctrl+Opt+I) is untouched, so inspecting is still possible.
 *  - The reload KEYBOARD shortcuts (Ctrl/Cmd+R, F5) are blocked in PRODUCTION
 *    only — dev keeps them so development reloads still work.
 */
export function installAccidentalReloadGuard(): void {
  if (typeof document === "undefined" || !isTauriEnv()) return;

  // Right-click context menu — suppress everywhere in the Tauri app (this is
  // the Back/Reload/Inspect menu the user can otherwise reload from).
  document.addEventListener(
    "contextmenu",
    (e) => {
      const t = e.target as HTMLElement | null;
      if (
        t &&
        (t.isContentEditable ||
          t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA")
      ) {
        return; // let editable fields show the native copy/paste menu
      }
      e.preventDefault();
    },
    { capture: true },
  );

  // Reload keyboard shortcuts — production only (keep them in dev). Vite injects
  // import.meta.env.DEV.
  if (import.meta.env?.DEV) return;
  document.addEventListener(
    "keydown",
    (e) => {
      const k = e.key.toLowerCase();
      // F5 / Ctrl+F5, and Ctrl/Cmd+R (+ Shift for hard reload — e.key is still
      // "r" once lower-cased).
      const isReload = k === "f5" || ((e.ctrlKey || e.metaKey) && k === "r");
      if (isReload) {
        e.preventDefault();
        e.stopPropagation();
      }
    },
    { capture: true },
  );
}
