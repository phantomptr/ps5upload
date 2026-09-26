import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/** In-app UI scale (text + layout). Every size in the app is rem-based off a
 *  single root font-size, so one multiplier proportionally resizes the WHOLE
 *  interface — text, padding, icons, controls.
 *
 *  Why this exists: on Android the system WebView inherits the device's
 *  accessibility **Font size** AND **Display size** settings. A user running a
 *  large Display size sees the entire app inflated, which overflows the dense
 *  phone layout and clips list rows to "ps5-image…" / "shadowmo…". The native
 *  `webView.settings.textZoom = 100` in MainActivity.kt only counters Font
 *  size (and only when that generated file is present in the build) — it does
 *  nothing for Display size. This setting is the durable, cross-platform
 *  escape hatch: the user dials the app's own scale down (or up) to taste,
 *  independent of any OS setting.
 *
 *  The number is a multiplier on the 18px base (see `BASE_PX`). 1.0 = the
 *  app's designed size; 0.8 = 80% (more fits on screen). */
export type UiScale = number;

const STORAGE_KEY = "ps5upload.uiScale";

/** The designed root size (mirrors the `--ui-base-size` default in index.css).
 *  Keep these in sync. */
const BASE_PX = 18;

/** Selectable steps, smallest → largest. 1.0 is the default. The range is
 *  deliberately bounded: below ~0.7 dense tables get hard to tap, above ~1.4
 *  the phone layout starts to overflow even on a default OS scale. */
export const UI_SCALE_STEPS: UiScale[] = [0.7, 0.8, 0.9, 1.0, 1.1, 1.25, 1.4];

const MIN_SCALE = UI_SCALE_STEPS[0];
const MAX_SCALE = UI_SCALE_STEPS[UI_SCALE_STEPS.length - 1];

/** Clamp a scale into the designed range; non-finite → 1.0. Keeping the UI
 *  scale bounded is what prevents an out-of-range value (corrupt storage, a
 *  future wider control) from making the app illegibly tiny or overflowing
 *  every layout. Pure → unit-tested directly. */
export function clampUiScale(scale: number): UiScale {
  if (!Number.isFinite(scale)) return 1.0;
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
}

/** Read the persisted scale synchronously so the first paint is correct. */
function initialScale(): UiScale {
  if (typeof window === "undefined") return 1.0;
  const raw = safeGetItem(STORAGE_KEY);
  if (raw == null) return 1.0;
  const n = Number.parseFloat(raw);
  return Number.isFinite(n) ? clampUiScale(n) : 1.0;
}

/** Drive the `--ui-base-size` CSS custom property on <html>. index.css applies
 *  it to `html, body, #root { font-size: var(--ui-base-size) }`, so this one
 *  property scales rem-based layout AND the inherited base text size. */
function applyScale(scale: UiScale) {
  if (typeof document === "undefined") return;
  const px = Math.round(BASE_PX * scale * 100) / 100;
  document.documentElement.style.setProperty("--ui-base-size", `${px}px`);
}

interface UiScaleState {
  scale: UiScale;
  setScale: (scale: UiScale) => void;
  /** Reset to the designed default (1.0). */
  reset: () => void;
}

export const useUiScaleStore = create<UiScaleState>((set) => ({
  scale: initialScale(),
  setScale: (scale) => {
    const next = clampUiScale(scale);
    safeSetItem(STORAGE_KEY, String(next));
    applyScale(next);
    set({ scale: next });
  },
  reset: () => {
    safeSetItem(STORAGE_KEY, "1");
    applyScale(1.0);
    set({ scale: 1.0 });
  },
}));

/** Human label for a step, e.g. 0.8 → "80%". */
export function uiScaleLabel(scale: UiScale): string {
  return `${Math.round(scale * 100)}%`;
}

// Apply the persisted scale on module load so the very first paint is right —
// before React mounts, matching the theme module's behavior.
if (typeof document !== "undefined") {
  applyScale(initialScale());
}
