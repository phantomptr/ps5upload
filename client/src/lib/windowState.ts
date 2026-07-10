import { useEffect } from "react";
import {
  getCurrentWindow,
  LogicalPosition,
  LogicalSize,
} from "@tauri-apps/api/window";
import { isMobile } from "./platform";
import { isTauriEnv } from "./tauriEnv";

/**
 * Persist + restore window size/position across launches. Cross-
 * platform: Tauri abstracts the OS-level window APIs, so the same
 * code path works on macOS, Windows, and Linux.
 *
 * Storage: localStorage. The key holds {width, height, x, y} in
 * logical (CSS) pixels. Logical coords are HiDPI-safe — restoring on
 * a Retina display gives the same on-screen size as the original.
 *
 * Apply order on mount:
 *   1. setSize first (so the window doesn't briefly stretch when
 *      moved into a smaller monitor area)
 *   2. setPosition
 *
 * Persistence is debounced (500ms) so a continuous drag-resize fires
 * one localStorage write at the end, not 30+ during the gesture.
 *
 * Edge cases handled:
 *   - Stored coords on a now-disconnected monitor: Tauri silently
 *     clamps to the primary monitor's bounds, so the window can't
 *     end up off-screen.
 *   - Maximized state: not currently persisted (intentional — users
 *     who maximize tend to want fresh state on next launch).
 */

const STORAGE_KEY = "ps5upload.window_state.v1";

interface StoredWindowState {
  width: number;
  height: number;
  x: number;
  y: number;
}

function loadStored(): StoredWindowState | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as StoredWindowState;
    // Validate; reject if any field is missing or non-finite. A bad
    // partial write (e.g. quota mid-flush) shouldn't crash startup.
    for (const k of ["width", "height", "x", "y"] as const) {
      if (typeof parsed[k] !== "number" || !Number.isFinite(parsed[k])) {
        return null;
      }
    }
    // Sanity floor: a width or height under 200 means corrupt state.
    // We refuse to apply rather than show a 1×1 window.
    if (parsed.width < 200 || parsed.height < 200) return null;
    return parsed;
  } catch {
    return null;
  }
}

function persist(state: StoredWindowState) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // best-effort
  }
}

/** Apply stored window state on mount; subscribe to resize/move and
 *  persist (debounced) for the rest of the session. */
export function useWindowStatePersistence() {
  useEffect(() => {
    // Mobile (Android/iOS): the app window is the screen — there is no
    // size/position to persist, and the desktop-only window APIs below
    // (outerSize/setPosition/onResized…) just throw. Gate inside the
    // effect (not before the hook call) to respect React hooks rules.
    // Browser: same window APIs don't exist there either.
    if (isMobile() || !isTauriEnv()) return;
    let cancelled = false;
    let cleanupResize: (() => void) | null = null;
    let cleanupMove: (() => void) | null = null;
    let debounceId: number | null = null;

    const schedulePersist = async () => {
      if (cancelled) return;
      if (debounceId !== null) window.clearTimeout(debounceId);
      debounceId = window.setTimeout(async () => {
        try {
          const w = getCurrentWindow();
          const size = await w.outerSize();
          const pos = await w.outerPosition();
          const factor = await w.scaleFactor();
          persist({
            width: size.width / factor,
            height: size.height / factor,
            x: pos.x / factor,
            y: pos.y / factor,
          });
        } catch {
          // Window may be in the middle of close; ignore.
        }
      }, 500);
    };

    void (async () => {
      const stored = loadStored();
      if (!stored) {
        // Even with no stored state, still subscribe to changes so
        // the first resize gets persisted.
      } else {
        try {
          const w = getCurrentWindow();
          await w.setSize(new LogicalSize(stored.width, stored.height));
          await w.setPosition(new LogicalPosition(stored.x, stored.y));
        } catch {
          // Silently fall back to default. A broken restore is not
          // worth interrupting startup.
        }
      }
      try {
        const w = getCurrentWindow();
        cleanupResize = await w.onResized(schedulePersist);
        cleanupMove = await w.onMoved(schedulePersist);
        // If the effect was torn down (unmount, StrictMode double-invoke,
        // HMR) while the awaits above were in flight, the cleanup closure
        // already ran with both handles still null and tore down nothing.
        // Detach immediately so the listeners attached post-cleanup don't
        // leak.
        if (cancelled) {
          cleanupResize?.();
          cleanupMove?.();
        }
      } catch {
        // No-op
      }
    })();

    return () => {
      cancelled = true;
      if (debounceId !== null) window.clearTimeout(debounceId);
      cleanupResize?.();
      cleanupMove?.();
    };
  }, []);
}
