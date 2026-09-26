// Lightweight platform detection for gating mobile-specific behavior.
//
// Dependency-free (reads navigator.userAgent) so it works inside the
// Tauri webview and in jsdom unit tests without pulling the Tauri OS
// plugin. Mirrors lib/diagnosticBundle.ts, which already reads
// navigator.userAgent the same defensive way.
//
// Used to branch file/folder pickers: desktop uses native dialogs that
// return real paths; Android's scoped storage can't, so we open an
// in-app browser backed by real-path commands instead.
//
// v5 extensions (§18.1 of v5-mobile-design.md): formFactor(),
// inputMode(), responsiveTier(), and useResponsiveTier() hook. These
// gate layout decisions (bottom nav vs left rail, touch targets, dual
// pane, modal sizing) independently of isMobile().

import { useSyncExternalStore } from "react";

function ua(): string {
  if (typeof navigator === "undefined") return "";
  return navigator.userAgent || "";
}

export function isAndroid(): boolean {
  return /android/i.test(ua());
}

export function isIOS(): boolean {
  const s = ua();
  // iPadOS 13+ reports as Mac; the touch-points check disambiguates.
  return (
    /iphone|ipad|ipod/i.test(s) ||
    (/Macintosh/.test(s) &&
      typeof navigator !== "undefined" &&
      (navigator as Navigator & { maxTouchPoints?: number }).maxTouchPoints !==
        undefined &&
      ((navigator as Navigator & { maxTouchPoints?: number }).maxTouchPoints ??
        0) > 1)
  );
}

export function isMobile(): boolean {
  return isAndroid() || isIOS();
}

// ── v5 platform extensions ──────────────────────────────────────────────

export type FormFactor = "phone" | "tablet" | "desktop";
export type InputMode = "touch" | "mouse" | "hybrid";
export type ResponsiveTier = "xs" | "sm" | "md" | "lg" | "xl";

/** The smaller of the two viewport dimensions. Used by formFactor() so
 *  that a phone in landscape (wide but short) still classifies as a
 *  phone, not a tablet. */
function viewportMin(): number {
  if (typeof window === "undefined") return 1024;
  return Math.min(window.innerWidth, window.innerHeight);
}

/** Device class. "desktop" for non-mobile; "tablet" for mobile with
 *  viewportMin >= 600 (iPad, large Android tablet); "phone" otherwise.
 *  Per v5-mobile-design §1.1. */
export function formFactor(): FormFactor {
  if (!isMobile()) return "desktop";
  // iPad reports >= 768 in portrait; iPhones report <= 430.
  // 600px is the Android/Nexus 7 small-tablet boundary.
  return viewportMin() >= 600 ? "tablet" : "phone";
}

/** Primary input mechanism. Per v5-mobile-design §1.1. "hybrid" covers
 *  Surface tablets and iPad Pro with keyboard — both coarse and fine
 *  pointers. Pure touch devices (phones, tablets without keyboard) are
 *  "touch". Everything else is "mouse". */
export function inputMode(): InputMode {
  if (typeof window === "undefined") return "mouse";
  const coarse = window.matchMedia("(pointer: coarse)").matches;
  const fine = window.matchMedia("(pointer: fine)").matches;
  const hover = window.matchMedia("(hover: hover)").matches;
  if (coarse && fine) return "hybrid";
  if (coarse && !hover) return "touch";
  return "mouse";
}

/** Responsive layout tier based on viewport width. Per v5-mobile-design
 *  §2.1 breakpoints (Tailwind-aligned). xs < 480, sm 480-767, md
 *  768-1023, lg 1024-1535, xl >= 1536. */
export function responsiveTier(): ResponsiveTier {
  const w = typeof window === "undefined" ? 1280 : window.innerWidth;
  if (w < 480) return "xs";
  if (w < 768) return "sm";
  if (w < 1024) return "md";
  if (w < 1536) return "lg";
  return "xl";
}

// ── useResponsiveTier() — reactive hook ─────────────────────────────────

/** Cache of matchMedia listeners so multiple components sharing the same
 *  breakpoint boundary don't each register redundant listeners. */
const tierQueries: Record<string, MediaQueryList> = {};

function getTierQuery(boundary: string): MediaQueryList | null {
  if (typeof window === "undefined" || !window.matchMedia) return null;
  if (!tierQueries[boundary]) {
    tierQueries[boundary] = window.matchMedia(boundary);
  }
  return tierQueries[boundary];
}

/** Subscribe to ALL breakpoint boundaries. A single resize crossing any
 *  boundary triggers a re-render of every useResponsiveTier() consumer.
 *  Returns the current ResponsiveTier. Uses useSyncExternalStore (React
 *  18 idiomatic for external mutable state). */
function subscribeTier(callback: () => void): () => void {
  if (typeof window === "undefined" || !window.matchMedia) return () => {};
  const boundaries = [
    "(max-width: 479.98px)",
    "(min-width: 480px) and (max-width: 767.98px)",
    "(min-width: 768px) and (max-width: 1023.98px)",
    "(min-width: 1024px) and (max-width: 1535.98px)",
    "(min-width: 1536px)",
  ];
  const cleanups: (() => void)[] = [];
  for (const b of boundaries) {
    const mql = getTierQuery(b);
    if (!mql) continue;
    mql.addEventListener("change", callback);
    cleanups.push(() => mql.removeEventListener("change", callback));
  }
  // Also listen to visualViewport resize for foldable/multi-window
  // accuracy (v5-mobile-design §2.4).
  if (window.visualViewport) {
    window.visualViewport.addEventListener("resize", callback);
    cleanups.push(() =>
      window.visualViewport?.removeEventListener("resize", callback),
    );
  }
  return () => {
    for (const c of cleanups) c();
  };
}

function readTier(): ResponsiveTier {
  return responsiveTier();
}

function readTierSnapshot(): ResponsiveTier {
  return readTier();
}

/** React hook that returns the current ResponsiveTier and re-renders the
 *  component when the tier changes. SSR/test-safe: returns "lg" when
 *  window is undefined. */
export function useResponsiveTier(): ResponsiveTier {
  return useSyncExternalStore(
    subscribeTier,
    readTierSnapshot,
    () => "lg" as ResponsiveTier, // server snapshot
  );
}
