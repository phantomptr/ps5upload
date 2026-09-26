import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { isMobile } from "../lib/platform";

/** Accessibility settings store (v5 §21).
 *
 * Drives four data-attributes on <html>:
 *   data-motion   = "full" | "reduced" | "none"
 *   data-density  = "comfortable" | "compact" | "spacious"
 *   data-contrast = "normal" | "high"
 *   data-dyslexia = "false" | "true"
 *
 * Plus two non-CSS settings:
 *   hapticsEnabled       — gates lib/haptics.ts (mobile only)
 *   screenReaderHints    — verbose aria-labels
 *   colorBlindPalette    — "default" | "deuteranopia" | "protanopia" | "tritanopia"
 *
 * All settings persist to localStorage and apply on module load (before
 * React mounts) to avoid a flash of the wrong mode. Same pattern as
 * theme.ts and uiScale.ts.
 *
 * Motion "auto" follows the OS prefers-reduced-motion query; once the
 * user explicitly chooses Full/Reduced/None, that overrides auto. */

export type MotionMode = "auto" | "full" | "reduced" | "none";
export type ResolvedMotion = "full" | "reduced" | "none";
export type DensityMode = "comfortable" | "compact" | "spacious";
export type ContrastMode = "normal" | "high";
export type ColorBlindPalette =
  | "default"
  | "deuteranopia"
  | "protanopia"
  | "tritanopia";

const STORAGE_KEY = "ps5upload.accessibility";

const VALID_MOTION: MotionMode[] = ["auto", "full", "reduced", "none"];
const VALID_DENSITY: DensityMode[] = ["comfortable", "compact", "spacious"];
const VALID_CONTRAST: ContrastMode[] = ["normal", "high"];
const VALID_CB: ColorBlindPalette[] = [
  "default",
  "deuteranopia",
  "protanopia",
  "tritanopia",
];

/** Resolve "auto" to a concrete motion by reading the OS media query.
 *  Returns "reduced" if the OS prefers reduced motion, else "full". */
function resolveMotion(mode: MotionMode): ResolvedMotion {
  if (mode !== "auto") return mode;
  if (typeof window === "undefined" || !window.matchMedia) return "full";
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches
    ? "reduced"
    : "full";
}

/** Default density based on input mode: Compact on desktop (mouse),
 *  Comfortable on touch devices. Per §17 "Auto-density". */
function defaultDensity(): DensityMode {
  if (isMobile()) return "comfortable";
  return "compact";
}

/** Default high-contrast: auto-on if OS prefers-contrast: more. */
function defaultContrast(): ContrastMode {
  if (typeof window === "undefined" || !window.matchMedia) return "normal";
  return window.matchMedia("(prefers-contrast: more)").matches
    ? "high"
    : "normal";
}

interface PersistedAccessibility {
  motion: MotionMode;
  density: DensityMode;
  contrast: ContrastMode;
  dyslexia: boolean;
  hapticsEnabled: boolean;
  screenReaderHints: boolean;
  colorBlindPalette: ColorBlindPalette;
}

function defaults(): PersistedAccessibility {
  return {
    motion: "auto",
    density: defaultDensity(),
    contrast: defaultContrast(),
    dyslexia: false,
    hapticsEnabled: true,
    screenReaderHints: false,
    colorBlindPalette: "default",
  };
}

function isValidShape(raw: unknown): raw is Partial<PersistedAccessibility> {
  if (typeof raw !== "object" || raw === null) return false;
  const obj = raw as Record<string, unknown>;
  if ("motion" in obj && typeof obj.motion === "string" && !VALID_MOTION.includes(obj.motion as MotionMode)) {
    return false;
  }
  if ("density" in obj && typeof obj.density === "string" && !VALID_DENSITY.includes(obj.density as DensityMode)) {
    return false;
  }
  if ("contrast" in obj && typeof obj.contrast === "string" && !VALID_CONTRAST.includes(obj.contrast as ContrastMode)) {
    return false;
  }
  if ("colorBlindPalette" in obj && typeof obj.colorBlindPalette === "string" && !VALID_CB.includes(obj.colorBlindPalette as ColorBlindPalette)) {
    return false;
  }
  return true;
}

/** Read the persisted settings synchronously so the first paint is
 *  correct. Falls back to OS-detected defaults when nothing is stored
 *  or the stored shape is corrupt. */
function initialSettings(): PersistedAccessibility {
  if (typeof window === "undefined") return defaults();
  const raw = safeGetItem(STORAGE_KEY);
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
      density:
        typeof parsed.density === "string" && VALID_DENSITY.includes(parsed.density)
          ? parsed.density
          : d.density,
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
      screenReaderHints:
        typeof parsed.screenReaderHints === "boolean"
          ? parsed.screenReaderHints
          : d.screenReaderHints,
      colorBlindPalette:
        typeof parsed.colorBlindPalette === "string" && VALID_CB.includes(parsed.colorBlindPalette)
          ? parsed.colorBlindPalette
          : d.colorBlindPalette,
    };
  } catch {
    return defaults();
  }
}

/** Write the data-attributes onto <html>. The CSS in index.css scopes
 *  animations, borders, focus rings, font stack, and density tokens
 *  off these attributes. */
function applyAttributes(settings: PersistedAccessibility): void {
  if (typeof document === "undefined") return;
  const el = document.documentElement;
  el.dataset.motion = resolveMotion(settings.motion);
  el.dataset.density = settings.density;
  el.dataset.contrast = settings.contrast;
  el.dataset.dyslexia = String(settings.dyslexia);
}

interface AccessibilityState extends PersistedAccessibility {
  setMotion: (mode: MotionMode) => void;
  setDensity: (density: DensityMode) => void;
  setContrast: (contrast: ContrastMode) => void;
  setDyslexia: (on: boolean) => void;
  setHapticsEnabled: (on: boolean) => void;
  setScreenReaderHints: (on: boolean) => void;
  setColorBlindPalette: (palette: ColorBlindPalette) => void;
  /** Resolve "auto" motion to a concrete value (for components that
   *  need to know the effective motion mode right now). */
  resolvedMotion: () => ResolvedMotion;
}

function persist(settings: PersistedAccessibility): void {
  safeSetItem(STORAGE_KEY, JSON.stringify(settings));
}

export const useAccessibilityStore = create<AccessibilityState>((set, get) => ({
  ...initialSettings(),

  setMotion: (motion) => {
    const next = { ...get(), motion };
    persist(next);
    applyAttributes(next);
    set({ motion });
  },
  setDensity: (density) => {
    const next = { ...get(), density };
    persist(next);
    applyAttributes(next);
    set({ density });
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
  setScreenReaderHints: (screenReaderHints) => {
    const next = { ...get(), screenReaderHints };
    persist(next);
    set({ screenReaderHints });
  },
  setColorBlindPalette: (colorBlindPalette) => {
    const next = { ...get(), colorBlindPalette };
    persist(next);
    set({ colorBlindPalette });
  },
  resolvedMotion: () => resolveMotion(get().motion),
}));

// Apply data-attributes on module load so the very first paint is right
// — before React mounts. Matches theme + uiScale module-load behavior.
if (typeof document !== "undefined") {
  applyAttributes(initialSettings());
}
