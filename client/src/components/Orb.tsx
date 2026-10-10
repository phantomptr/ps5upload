/**
 * The brand orb: a glossy coral-to-pink sphere. The app's mark in the
 * sidebar, and the soft focal point of the Home greeting and empty states.
 * Pure CSS (`.orb` in index.css), so it costs no image and scales cleanly.
 * Decorative: always hidden from assistive tech.
 */
export function Orb({
  size = 40,
  className = "",
}: {
  /** Diameter in px. */
  size?: number;
  className?: string;
}) {
  return (
    <span
      aria-hidden="true"
      data-orb=""
      className={`orb ${className}`}
      style={{ width: size, height: size }}
    />
  );
}
