import { useLiveRegionStore } from "../state/liveRegion";

/**
 * Polite live region. Mounted once at the app root. Any component can
 * push a message via `useLiveRegionStore.getState().announce(msg)` and
 * screen readers will read it aloud.
 *
 * `.sr-only` keeps it visually hidden. `aria-atomic` so the whole
 * message is read each time (not just the diff).
 */
export function LiveRegion() {
  const message = useLiveRegionStore((s) => s.message);
  return (
    <div aria-live="polite" aria-atomic="true" className="sr-only">
      {message}
    </div>
  );
}
