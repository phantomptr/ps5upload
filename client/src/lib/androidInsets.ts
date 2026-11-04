/**
 * Give the page the Android system bar and cutout insets as `--sys-inset-top`, `-right`, `-left`
 * and `--nav-inset-bottom`.
 *
 * The Android app draws edge-to-edge, and the layout pads the shell with the safe area. Android
 * WebView reports only part of it through `env(safe-area-inset-*)` — measured in the app on the
 * emulator (Chrome 134): top 49px, bottom 0px — so the tab bar sat under the gesture bar with its
 * labels covered, and a 3-button nav bar on the side of a phone held sideways is not reported at
 * all. The Android shell (MainActivity, patched by
 * scripts/release/android-postinit-patch.sh) exposes the real values as
 * `window.PS5UploadInsets.systemInsetsPx()`, "top,right,bottom,left,keyboard" in physical pixels, and fires
 * `ps5upload-insets` whenever they change (rotation, gesture ↔ 3-button navigation). `--safe-*`
 * in index.css takes the larger of the two on each side, so iOS and desktop browsers, where env()
 * is right, are unaffected.
 *
 * A no-op everywhere the interface is absent: desktop, browser build, iOS.
 */

interface InsetsBridge {
  systemInsetsPx(): string;
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
  let sides: number[];
  try {
    sides = String(bridge.systemInsetsPx()).split(",").map(Number);
  } catch {
    // A bridge that throws is treated like one that is absent: the layout
    // falls back to env(), which is what it did before.
    return;
  }
  const [top, right, bottom, left] = sides.map((px) => navInsetCssPx(px, w.devicePixelRatio));
  const root = w.document.documentElement;
  // The shell resizes the page for the keyboard; the bottom tab bar and activity pill would sit
  // on top of what is left of the screen (index.css hides them).
  root.toggleAttribute("data-keyboard", (sides[4] ?? 0) > 0);
  const style = root.style;
  style.setProperty("--sys-inset-top", `${top ?? 0}px`);
  style.setProperty("--sys-inset-right", `${right ?? 0}px`);
  style.setProperty("--nav-inset-bottom", `${bottom ?? 0}px`);
  style.setProperty("--sys-inset-left", `${left ?? 0}px`);
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
