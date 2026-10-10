import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/** Accessibility settings store (v5 §21).
 *
 * Drives three data-attributes on <html>:
 *   data-motion   = "full" | "reduced" | "none"
 *   data-contrast = "normal" | "high"
 *   data-dyslexia = "false" | "true"
 *
 * Plus one non-CSS setting:
 *   hapticsEnabled       — gates lib/haptics.ts (mobile only)
 *
 * Stored settings from older versions may still carry density,
 * screenReaderHints and colorBlindPalette: nothing ever read them, so they
 * are ignored on load and dropped on the next write.
 *
 * All settings persist to localStorage and apply on module load (before
 * React mounts) to avoid a flash of the wrong mode. Same pattern as
 * theme.ts and uiScale.ts.
 *
 * Motion "auto" follows the OS prefers-reduced-motion query; once the
 * user explicitly chooses Full/Reduced/None, that overrides auto. */

export type MotionMode = "auto" | "full" | "reduced" | "none";
export type ResolvedMotion = "full" | "reduced" | "none";
export type ContrastMode = "normal" | "high";

const STORAGE_KEY = "ps5upload.accessibility";

const VALID_MOTION: MotionMode[] = ["auto", "full", "reduced", "none"];
const VALID_CONTRAST: ContrastMode[] = ["normal", "high"];

/** Resolve "auto" to a concrete motion by reading the OS media query.
 *  Returns "reduced" if the OS prefers reduced motion, else "full". */
function resolveMotion(mode: MotionMode): ResolvedMotion {
  if (mode !== "auto") return mode;
  if (typeof window === "undefined" || !window.matchMedia) return "full";
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches
    ? "reduced"
    : "full";
}

/** Default high-contrast: auto-on if OS prefers-contrast: more. */
function defaultContrast(): ContrastMode {
  if (typeof window === "undefined" || !window.matchMedia) return "normal";
  return window.matchMedia("(prefers-contrast: more)").matches
    ? "high"
    : "normal";
}

export interface PersistedAccessibility {
  motion: MotionMode;
  contrast: ContrastMode;
  dyslexia: boolean;
  hapticsEnabled: boolean;
}

function defaults(): PersistedAccessibility {
  return {
    motion: "auto",
    contrast: defaultContrast(),
    dyslexia: false,
    hapticsEnabled: true,
  };
}

function isValidShape(raw: unknown): raw is Partial<PersistedAccessibility> {
  if (typeof raw !== "object" || raw === null) return false;
  const obj = raw as Record<string, unknown>;
  if ("motion" in obj && typeof obj.motion === "string" && !VALID_MOTION.includes(obj.motion as MotionMode)) {
    return false;
  }
  if ("contrast" in obj && typeof obj.contrast === "string" && !VALID_CONTRAST.includes(obj.contrast as ContrastMode)) {
    return false;
  }
  return true;
}

/** Read the persisted settings synchronously so the first paint is
 *  correct. Falls back to OS-detected defaults when nothing is stored
 *  or the stored shape is corrupt. */
function initialSettings(): PersistedAccessibility {
  if (typeof window === "undefined") return defaults();
  return parseAccessibility(safeGetItem(STORAGE_KEY));
}

/** The stored JSON → settings. Keys this version no longer has (density,
 *  screenReaderHints, colorBlindPalette) are ignored whatever their value.
 *  Exported for tests. */
export function parseAccessibility(raw: string | null): PersistedAccessibility {
  if (!raw) return defaults();
  try {
    const parsed = JSON.parse(raw);
    if (!isValidShape(parsed)) return defaults();
    const d = defaults();
    return {
      motion:
        typeof parsed.motion === "string" && VALID_MOTION.includes(parsed.motion)
          ? parsed.motion
          : d.motion,
      contrast:
        typeof parsed.contrast === "string" && VALID_CONTRAST.includes(parsed.contrast)
          ? parsed.contrast
          : d.contrast,
      dyslexia:
        typeof parsed.dyslexia === "boolean" ? parsed.dyslexia : d.dyslexia,
      hapticsEnabled:
        typeof parsed.hapticsEnabled === "boolean"
          ? parsed.hapticsEnabled
          : d.hapticsEnabled,
    };
  } catch {
    return defaults();
  }
}

/** Write the data-attributes onto <html>. The CSS in index.css scopes
 *  animations, borders, focus rings, and the font stack off these
 *  attributes. */
function applyAttributes(settings: PersistedAccessibility): void {
  if (typeof document === "undefined") return;
  const el = document.documentElement;
  el.dataset.motion = resolveMotion(settings.motion);
  el.dataset.contrast = settings.contrast;
  el.dataset.dyslexia = String(settings.dyslexia);
}

interface AccessibilityState extends PersistedAccessibility {
  setMotion: (mode: MotionMode) => void;
  setContrast: (contrast: ContrastMode) => void;
  setDyslexia: (on: boolean) => void;
  setHapticsEnabled: (on: boolean) => void;
  /** Resolve "auto" motion to a concrete value (for components that
   *  need to know the effective motion mode right now). */
  resolvedMotion: () => ResolvedMotion;
}

/** Only the persisted fields: `get()` also carries the setters. */
function persist(settings: PersistedAccessibility): void {
  const { motion, contrast, dyslexia, hapticsEnabled } = settings;
  safeSetItem(
    STORAGE_KEY,
    JSON.stringify({ motion, contrast, dyslexia, hapticsEnabled }),
  );
}

export const useAccessibilityStore = create<AccessibilityState>((set, get) => ({
  ...initialSettings(),

  setMotion: (motion) => {
    const next = { ...get(), motion };
    persist(next);
    applyAttributes(next);
    set({ motion });
  },
  setContrast: (contrast) => {
    const next = { ...get(), contrast };
    persist(next);
    applyAttributes(next);
    set({ contrast });
  },
  setDyslexia: (dyslexia) => {
    const next = { ...get(), dyslexia };
    persist(next);
    applyAttributes(next);
    set({ dyslexia });
  },
  setHapticsEnabled: (hapticsEnabled) => {
    const next = { ...get(), hapticsEnabled };
    persist(next);
    set({ hapticsEnabled });
  },
  resolvedMotion: () => resolveMotion(get().motion),
}));

// Apply data-attributes on module load so the very first paint is right
// — before React mounts. Matches theme + uiScale module-load behavior.
if (typeof document !== "undefined") {
  applyAttributes(initialSettings());
}
