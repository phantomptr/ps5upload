import { useEffect, useState } from "react";
import { Gamepad2 } from "lucide-react";

import { appIconUrl, gameIconUrl } from "../api/ps5";
import { transferAddr } from "../lib/addr";

/**
 * Shared game cover/icon with a graceful glyph fallback.
 *
 * Sources are tried in order, matching the ways the app knows about a title:
 *   - `titleId` → `/user/appmeta/<id>/icon0.png` (installed / registered titles)
 *   - `gamePath` → `<game folder>/sce_sys/icon0.png` (library scan, pre-install)
 *   - `fallbackSrc` → any URL (e.g. an external cover-art CDN) for titles the
 *     console has no local art for — a save for a game that isn't installed.
 * Pass whichever you have; `titleId` wins, then `gamePath`. On a 404 at one
 * source it advances to the next, and only shows a controller glyph once every
 * candidate has failed — same fallback the Install Package / Installed Apps /
 * Library screens each reimplemented inline before this consolidated them.
 */
export function GameIcon({
  host,
  titleId,
  gamePath,
  fallbackSrc,
  size = 56,
  rounded = "rounded-md",
  className = "",
}: {
  host: string;
  titleId?: string | null;
  gamePath?: string | null;
  /** Last-resort cover URL (e.g. external CDN) tried after the local sources. */
  fallbackSrc?: string | null;
  /** Square edge length in px. */
  size?: number;
  /** Tailwind rounding class for the frame. */
  rounded?: string;
  className?: string;
}) {
  const hostReady = !!host.trim();
  // Ordered candidate list: local appmeta → local game folder → external cover.
  // Filtering nulls here means the fallback chain naturally skips sources we
  // don't have rather than rendering a broken <img>.
  const candidates = [
    hostReady && titleId ? appIconUrl(transferAddr(host), titleId) : null,
    hostReady && gamePath ? gameIconUrl(transferAddr(host), gamePath) : null,
    fallbackSrc || null,
  ].filter((s): s is string => !!s);
  const [idx, setIdx] = useState(0);
  // Reset to the first candidate whenever the set changes — otherwise an
  // instance reused for a different title (list reorder) would stay on a
  // stale fallback index even when the new title's primary art exists.
  const key = candidates.join("|");
  useEffect(() => {
    setIdx(0);
  }, [key]);
  const src = idx < candidates.length ? candidates[idx] : null;
  return (
    <div
      className={`flex shrink-0 items-center justify-center overflow-hidden bg-[var(--color-surface-3)] ${rounded} ${className}`}
      style={{ width: size, height: size }}
    >
      {src ? (
        <img
          src={src}
          alt=""
          className="h-full w-full object-cover"
          loading="lazy"
          // Advance to the next candidate on failure; the glyph shows once idx
          // walks past the end of the list.
          onError={() => setIdx((i) => i + 1)}
        />
      ) : (
        <Gamepad2
          size={Math.round(size * 0.36)}
          className="text-[var(--color-muted)]"
        />
      )}
    </div>
  );
}

export default GameIcon;
