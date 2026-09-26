/**
 * Give the page the Android navigation-bar height as `--nav-inset-bottom`.
 *
 * The Android app draws edge-to-edge, and the layout pads the bottom tab bar
 * with the safe area. Android WebView does not report the navigation bar
 * through `env(safe-area-inset-bottom)` — measured in the app on the emulator
 * (Chrome 134): top 49px, bottom 0px — so the tab bar sat under the gesture
 * bar with its labels covered. The Android shell (MainActivity, patched by
 * scripts/release/android-postinit-patch.sh) exposes the real value as
 * `window.PS5UploadInsets.navBottomPx()`, in physical pixels, and fires
 * `ps5upload-insets` whenever the insets change (rotation, gesture ↔ 3-button
 * navigation). `--safe-bottom` in index.css takes the larger of the two, so
 * iOS and desktop browsers, where env() is right, are unaffected.
 *
 * A no-op everywhere the interface is absent: desktop, browser build, iOS.
 */

interface InsetsBridge {
  navBottomPx(): number;
}

type WindowWithInsets = Window & { PS5UploadInsets?: InsetsBridge };

/** CSS pixels for a physical-pixel inset. Exported for tests. */
export function navInsetCssPx(physicalPx: number, dpr: number): number {
  if (!Number.isFinite(physicalPx) || physicalPx <= 0) return 0;
  const ratio = Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
  return Math.round((physicalPx / ratio) * 100) / 100;
}

export function applyNavInset(w: Window = window): void {
  const bridge = (w as WindowWithInsets).PS5UploadInsets;
  if (!bridge) return;
  let px: number;
  try {
    px = navInsetCssPx(bridge.navBottomPx(), w.devicePixelRatio);
  } catch {
    // A bridge that throws is treated like one that is absent: the layout
    // falls back to env(), which is what it did before.
    return;
  }
  w.document.documentElement.style.setProperty("--nav-inset-bottom", `${px}px`);
}

/** Apply now and whenever the shell reports a change or the window resizes. */
export function installNavInsets(w: Window = window): void {
  if (!(w as WindowWithInsets).PS5UploadInsets) return;
  const apply = () => applyNavInset(w);
  apply();
  w.addEventListener("ps5upload-insets", apply);
  w.addEventListener("resize", apply);
}

if (typeof window !== "undefined") installNavInsets();
