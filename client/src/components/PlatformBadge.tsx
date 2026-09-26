/**
 * PS4 / PS5 platform badge — the single source of truth for the small
 * coloured "PS4"/"PS5" chip shown next to a title or package.
 *
 * Before this, three different versions existed (a filled chip on Installed
 * Apps, an outlined pill on Install Package's external rows, and a third
 * inline pill in the library rows), so the same concept looked different
 * depending on where you were. This consolidates them on the filled style —
 * the most legible, and the one that matches the brand badges on game cards.
 *
 * Renders nothing for anything that isn't "ps4"/"ps5" (system apps, homebrew
 * with no recognizable id), so callers can pass a raw platform string —
 * including "system"/"other"/"" — without guarding first.
 */
export function PlatformBadge({
  platform,
  className = "",
}: {
  platform?: string | null;
  className?: string;
}) {
  if (platform !== "ps4" && platform !== "ps5") return null;
  const isPs5 = platform === "ps5";
  return (
    <span
      className={`inline-flex shrink-0 items-center whitespace-nowrap rounded px-1.5 py-0.5 text-xs font-semibold ${
        isPs5
          ? "bg-[var(--color-ps5-soft)] text-[var(--color-ps5)]"
          : "bg-[var(--color-ps4-soft)] text-[var(--color-ps4)]"
      } ${className}`}
    >
      {isPs5 ? "PS5" : "PS4"}
    </span>
  );
}

export default PlatformBadge;
