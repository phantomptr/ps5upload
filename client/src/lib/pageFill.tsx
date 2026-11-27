import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { safeGetItem, safeSetItem } from "./safeStorage";
import { useTr } from "../state/lang";

/** The browser can put the page itself full screen (not on iPhone: no browser there can). */
export function hasFullscreenApi(): boolean {
  return typeof document !== "undefined" && !!document.documentElement.requestFullscreen;
}

/**
 * The join page's Full screen button: always in the app or with the API; without the API (every
 * iPhone browser) only to switch to the full layout, as there is nothing to undo after.
 */
export function showsFullButton(inApp: boolean, full: boolean, api = hasFullscreenApi()): boolean {
  return inApp || api || !full;
}

/** iPhone or iPad (an iPad says Mac but has touch). */
export function isAppleTouch(): boolean {
  if (typeof navigator === "undefined") return false;
  const ua = navigator.userAgent || "";
  return /iPhone|iPad|iPod/.test(ua) || (/Macintosh/.test(ua) && (navigator.maxTouchPoints ?? 0) > 1);
}

/** Opened from the Home Screen: no browser bars to hide. */
function standalone(): boolean {
  try {
    return (
      (navigator as Navigator & { standalone?: boolean }).standalone === true ||
      !!window.matchMedia?.("(display-mode: standalone)").matches
    );
  } catch {
    return false;
  }
}

export const HOME_TIP_KEY = "ps5upload.homeScreenTip";

/**
 * Full screen without the API: the page's own full layout already fills the window; bring the
 * page to the top so the picture fills what the browser shows. True when the Home Screen tip
 * should show (iPhone or iPad, in the browser, not shown before).
 */
export function fillWithoutApi(): boolean {
  try {
    window.scrollTo(0, 0);
  } catch {
    // Not a real window (tests).
  }
  return isAppleTouch() && !standalone() && safeGetItem(HOME_TIP_KEY) !== "1";
}

/** The one-time tip: the browser's bars stay; the Home Screen has none. */
export function HomeScreenTip({ onClose }: { onClose: () => void }) {
  const tr = useTr();
  const close = () => {
    safeSetItem(HOME_TIP_KEY, "1");
    onClose();
  };
  return (
    <div
      role="status"
      className="fixed inset-x-3 z-[260] mx-auto flex max-w-md items-start gap-2 rounded-xl bg-white/95 p-3 text-sm text-black shadow-lg"
      style={{ top: "calc(max(8px, env(safe-area-inset-top)) + 2.5rem)" }}
      data-testid="home-screen-tip"
    >
      <p className="flex-1">
        {tr(
          "page_home_screen_tip",
          undefined,
          "Your browser can't hide its own bars. For the whole screen, add this page to your Home Screen (Share, then Add to Home Screen) and open it from there.",
        )}
      </p>
      <button
        type="button"
        onClick={close}
        aria-label={tr("page_home_screen_tip_close", undefined, "Got it")}
        className="rounded-full p-1"
        data-testid="home-screen-tip-close"
      >
        <X size={14} />
      </button>
    </div>
  );
}

/** The tip's state: showIfNeeded() after a Full screen tap without the API. */
export function useHomeScreenTip() {
  const [tip, setTip] = useState(false);
  return { tip, showIfNeeded: () => setTip(fillWithoutApi()), hide: () => setTip(false) };
}

/** The phone is sideways now: the screen's own orientation where the browser says it, else the
 *  older `window.orientation`, else the window's shape. */
export function sidewaysNow(): boolean {
  if (typeof window === "undefined") return false;
  try {
    const t = window.screen?.orientation?.type;
    if (t) return t.startsWith("landscape");
  } catch {
    // Not a real screen (tests).
  }
  const o = (window as Window & { orientation?: number }).orientation;
  if (typeof o === "number") return Math.abs(o) === 90;
  return window.innerWidth > window.innerHeight;
}

/** What a turn does to the page's full layout: in when the phone turned sideways, out when it
 *  turned upright again, nothing when it already is that way. */
export function turnStep(sideways: boolean, full: boolean): "enter" | "leave" | null {
  if (sideways && !full) return "enter";
  if (!sideways && full) return "leave";
  return null;
}

/**
 * A phone turned (and once at the start when it is already sideways): `onTurn(sideways)`, called
 * inside the browser's own orientation event, the one moment Android Chrome lets a page go full
 * screen without a tap.
 */
export function useTurns(active: boolean, onTurn: (sideways: boolean) => void): void {
  const latest = useRef(onTurn);
  useEffect(() => {
    latest.current = onTurn;
  });
  useEffect(() => {
    if (!active || typeof window === "undefined") return;
    if (sidewaysNow()) latest.current(true);
    const fire = () => latest.current(sidewaysNow());
    const so = window.screen?.orientation;
    if (so?.addEventListener) {
      so.addEventListener("change", fire);
      return () => so.removeEventListener("change", fire);
    }
    window.addEventListener("orientationchange", fire);
    return () => window.removeEventListener("orientationchange", fire);
  }, [active]);
}

export const COLLAPSE_BARS_ATTR = "data-collapse-bars";

/**
 * Full screen in a phone browser without the Fullscreen API (iPhone): the page is made a little
 * taller than the window (index.css, `data-collapse-bars`) and scrolled down once, which lets
 * Safari fold its bars away (its minimal UI) where it allows. The game stays fixed over the whole
 * window and keeps every touch; nothing scrolls under a finger. Undone when it ends.
 */
export function useCollapseBars(active: boolean): void {
  useEffect(() => {
    if (!active || typeof document === "undefined") return;
    const root = document.documentElement;
    root.setAttribute(COLLAPSE_BARS_ATTR, "");
    const down = () => {
      try {
        window.scrollTo(0, root.scrollHeight);
      } catch {
        // Not a real window (tests).
      }
    };
    // After the taller page is laid out, and again after a turn settles.
    const t = window.setTimeout(down, 60);
    window.addEventListener("resize", down);
    return () => {
      window.clearTimeout(t);
      window.removeEventListener("resize", down);
      root.removeAttribute(COLLAPSE_BARS_ATTR);
      try {
        window.scrollTo(0, 0);
      } catch {
        // Not a real window (tests).
      }
    };
  }, [active]);
}
