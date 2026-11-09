import { useEffect, useState } from "react";

import { invoke } from "../../lib/invokeLogged";
import { isAndroid, isIOS, isMacOrLinuxDesktop, isMobile } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";

/**
 * Full screen for Remote Play on phones, tablets and computers.
 *
 * Two layers: "immersive" is our own layout (the picture fills the window, the controls float
 * over it, one small menu handle), and the browser's or app window's full screen hides the
 * system bars on top of that where the platform allows it. iPhone Safari has no element full
 * screen at all, so there the immersive layout is all there is; on Android the system back
 * gesture can drop the browser's full screen at any time, and the layout stays immersive and
 * asks again on the next touch. The Android app's WebView refuses the page's full screen, so
 * there the app hides the system bars and turns the screen itself (`android_system_ui`).
 */

/** A media query, live. */
export function useMedia(query: string): boolean {
  const get = () =>
    typeof window !== "undefined" && !!window.matchMedia?.(query).matches;
  const [on, setOn] = useState(get);
  useEffect(() => {
    const mq = window.matchMedia?.(query);
    if (!mq) return;
    const update = () => setOn(mq.matches);
    update();
    mq.addEventListener?.("change", update);
    return () => mq.removeEventListener?.("change", update);
  }, [query]);
  return on;
}

/** Touch-sized player: a touch screen, or a window too narrow for the bar on the picture. */
export const COMPACT_QUERY = "(pointer: coarse), (max-width: 639px)";
/** A phone held sideways: the picture takes the whole screen by itself. */
export const PHONE_LANDSCAPE_QUERY =
  "(pointer: coarse) and (orientation: landscape) and (max-height: 520px)";
/** A touch screen (fingers, not a mouse). */
export const COARSE_QUERY = "(pointer: coarse)";

type FsDoc = Document & {
  webkitFullscreenElement?: Element | null;
  webkitExitFullscreen?: () => void;
};
type FsEl = HTMLElement & { webkitRequestFullscreen?: () => void };

export function docFullscreen(): boolean {
  const d = document as FsDoc;
  return !!(d.fullscreenElement || d.webkitFullscreenElement);
}

/** The iOS app: its web view already has the whole screen. Asking WebKit for full screen on top
 *  of that adds Apple's own close button over the controller's L2, and WebKit leaves full screen
 *  when the keyboard opens, so the picture jumped each time ⌨ was tapped (an iPhone Duo, #675). */
function inIosApp(): boolean {
  return isTauriEnv() && isIOS();
}

/** Whether the page can ask the browser for full screen at all (not iPhone Safari). */
export function canDocFullscreen(): boolean {
  if (inIosApp()) return false;
  const el = document.documentElement as FsEl;
  return (
    typeof el.requestFullscreen === "function" ||
    typeof el.webkitRequestFullscreen === "function"
  );
}

/** The Android app: the system bars and the orientation are the app's own to set. */
export function inAndroidApp(): boolean {
  return isTauriEnv() && isAndroid();
}

/** What the Android app was last asked for; repeats (a touch on the picture asks again) are
 *  dropped here instead of crossing to the native side. */
const androidUi = { immersive: false, landscape: false };

function setAndroidUi(immersive: boolean, landscape: boolean): Promise<void> {
  if (androidUi.immersive === immersive && androidUi.landscape === landscape)
    return Promise.resolve();
  androidUi.immersive = immersive;
  androidUi.landscape = landscape;
  return invoke("android_system_ui", { immersive, landscape }).then(
    () => {},
    () => {
      // Not hidden this time: the picture still fills the window; ask again next time.
      androidUi.immersive = false;
      androidUi.landscape = false;
    },
  );
}

/** The iOS and Mac apps: make the page the first responder, so controllers (MFi, Backbone, a
 *  DualSense, any Bluetooth pad) reach its Gamepad API; WKWebView gives them only to the first
 *  responder (on the Mac, the key window's). Answers what had the focus before on the Mac
 *  ("webview" when the page already had it, see focus_webview), "" on iOS, null where nothing was
 *  asked or the app did not answer. */
export async function focusForControllers(): Promise<string | null> {
  if (!isTauriEnv() || !(isIOS() || isMacOrLinuxDesktop())) return null;
  try {
    return await invoke<string>("focus_webview");
  } catch {
    return null;
  }
}

/** What `focusForControllers` answered, as the log says it: null when the page had the focus
 *  already (or nothing was asked), else who had the keyboard and the controllers before. */
export function focusTakenFrom(was: string | null): string | null {
  return !was || was === "webview" ? null : was;
}

/** The iOS and Mac apps: ask for the controller's PS button for the page rather than the
 *  system's Games overlay. On iOS the system only agrees when the user's Home button setting
 *  allows it; otherwise the menu's PS button is the way. Elsewhere the command is a no-op. */
export function holdControllerHome(on: boolean): void {
  if (isTauriEnv() && (isIOS() || isMacOrLinuxDesktop())) void invoke("hold_controller_home", { on }).catch(() => {});
}

/** The iOS app: hide the status bar (the clock) while the picture fills the screen, and show it
 *  again after (#722). Elsewhere a no-op: the browser and the other apps have their own way. */
export function setIosStatusBarHidden(hidden: boolean): void {
  if (inIosApp()) void invoke("set_status_bar_hidden", { hidden }).catch(() => {});
}

/** Test hook: forget what the Android app was asked for. */
export function resetAndroidUiForTest(): void {
  androidUi.immersive = false;
  androidUi.landscape = false;
}

/** The Keyboard Lock API (Chromium), or null. */
function keyboardLock(): { lock(codes: string[]): Promise<void>; unlock(): void } | null {
  const k = (navigator as Navigator & { keyboard?: { lock?: unknown; unlock?: unknown } }).keyboard;
  return k && typeof k.lock === "function" && typeof k.unlock === "function"
    ? (k as { lock(codes: string[]): Promise<void>; unlock(): void })
    : null;
}

/** The window (desktop app) or the screen (browser, phone app) given to the picture. In the
 *  Android app this hides the status and navigation bars (a swipe shows them for a moment);
 *  leaving also gives the orientation back. */
export async function setWindowFullscreen(on: boolean): Promise<void> {
  try {
    if (inAndroidApp()) {
      await setAndroidUi(on, on && androidUi.landscape);
      return;
    }
    if (inIosApp()) {
      setIosStatusBarHidden(on);
      return;
    }
    if (isTauriEnv() && !isMobile()) {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().setFullscreen(on);
      return;
    }
    const d = document as FsDoc;
    const el = document.documentElement as FsEl;
    if (on) {
      if (docFullscreen()) return;
      if (el.requestFullscreen)
        await el.requestFullscreen({ navigationUI: "hide" });
      else el.webkitRequestFullscreen?.();
      // Chrome and Edge: a quick Esc reaches the game, and holding it leaves full screen (the
      // page does the same where the browser lets it). Elsewhere Esc always leaves.
      void keyboardLock()?.lock(["Escape"]).catch(() => {});
    } else if (d.fullscreenElement) {
      keyboardLock()?.unlock();
      await document.exitFullscreen();
    } else if (d.webkitFullscreenElement) {
      d.webkitExitFullscreen?.();
    }
  } catch {
    // Not allowed here (iPhone, no user gesture): the picture still fills the window.
  }
}

type LockableOrientation = ScreenOrientation & {
  lock?: (o: string) => Promise<void>;
};

/** Turn the picture sideways and keep it there (Android in full screen; not iOS). */
export function lockLandscape(): void {
  if (inAndroidApp()) {
    void setAndroidUi(androidUi.immersive, true);
    return;
  }
  try {
    const o = screen.orientation as LockableOrientation | undefined;
    void o?.lock?.("landscape")?.catch?.(() => {});
  } catch {
    // No orientation lock here.
  }
}

export function unlockOrientation(): void {
  if (inAndroidApp()) {
    void setAndroidUi(androidUi.immersive, false);
    return;
  }
  try {
    screen.orientation?.unlock?.();
  } catch {
    // Nothing was locked.
  }
}

/** The page may not scroll, bounce, zoom or pull to refresh while the picture fills it. */
export function holdPageStill(on: boolean): void {
  const root = document.documentElement.style;
  const body = document.body.style;
  root.overscrollBehavior = on ? "none" : "";
  body.overscrollBehavior = on ? "none" : "";
  body.overflow = on ? "hidden" : "";
}

/** A phone rather than a tablet: the picture is turned sideways for full screen. */
export function isPhoneSized(): boolean {
  if (typeof screen === "undefined") return false;
  return Math.min(screen.width, screen.height) < 600;
}
