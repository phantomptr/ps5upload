import { useCallback, useEffect, useState } from "react";
import { Gamepad2 } from "lucide-react";

import {
  appIconUrl,
  appIconDataUrl,
  cachedAppIcon,
  cachedGameIcon,
  engineImageCors,
  gameIconUrl,
  gameIconDataUrl,
  GRID_ICON_SIZE,
  iconSizeFor,
  type IconSize,
} from "../api/ps5";
import { transferAddr } from "../lib/addr";
import { iconTitleId } from "../lib/titleId";
import { useImageRetry } from "../lib/useImageRetry";

/**
 * Shared game cover/icon with a graceful glyph fallback.
 *
 * Sources are tried in order, matching the ways the app knows about a title:
 *   - `sourceSrc` → the icon read from the source itself (a queued package)
 *   - `titleId` → `/user/appmeta/<id>/icon0.png` (installed / registered titles)
 *   - `gamePath` → `<game folder>/sce_sys/icon0.png` (library scan, pre-install)
 *   - `fallbackSrc` → any URL (e.g. an external cover-art CDN) for titles the
 *     console has no local art for — a save for a game that isn't installed.
 * Pass whichever you have; `sourceSrc` wins, then `titleId`, then `gamePath`. On a 404 at one
 * source it advances to the next, and only shows a controller glyph once every
 * candidate has failed — same fallback the Install Package / Installed Apps /
 * Library screens each reimplemented inline before this consolidated them.
 */
export interface IconCandidate {
  url: string;
  /** The bytes this session already holds for a console-served source. */
  cached?: () => string | undefined;
  /** Fetch a console-served source over IPC when the URL itself fails. */
  load?: () => Promise<string | null>;
}

/** The icon sources in the order they are tried: the source's own icon, the console's icon for
 *  the title, the console's game folder, then the online cover. Sources you lack are left out. */
export function gameIconCandidates({
  host,
  titleId,
  gamePath,
  sourceSrc,
  fallbackSrc,
  px,
}: {
  host: string;
  titleId?: string | null;
  gamePath?: string | null;
  sourceSrc?: string | null;
  fallbackSrc?: string | null;
  px: IconSize;
}): IconCandidate[] {
  const hostReady = !!host.trim();
  // A folder named after a title but not exactly it (a save backup `PPSA17221.bak`) shows that
  // title's icon; a name that isn't a title id at all skips the console lookup.
  const iconId = titleId ? iconTitleId(titleId) : null;
  const addr = hostReady ? transferAddr(host) : "";
  const out: IconCandidate[] = [];
  if (sourceSrc) out.push({ url: sourceSrc });
  if (hostReady && iconId) {
    out.push({
      url: appIconUrl(addr, iconId, px),
      cached: () => cachedAppIcon(addr, iconId, px),
      load: () => appIconDataUrl(addr, iconId, px),
    });
  }
  if (hostReady && gamePath) {
    out.push({
      url: gameIconUrl(addr, gamePath, px),
      cached: () => cachedGameIcon(addr, gamePath, px),
      load: () => gameIconDataUrl(addr, gamePath, px),
    });
  }
  if (fallbackSrc) out.push({ url: fallbackSrc });
  return out;
}

export function GameIcon({
  host,
  titleId,
  gamePath,
  sourceSrc,
  fallbackSrc,
  alt = "",
  size = 56,
  rounded = "rounded-md",
  className = "",
  fill = false,
  label,
}: {
  host: string;
  titleId?: string | null;
  /** A `<game folder>` ON THE CONSOLE. Never a path on this computer: the console would be
   *  asked for a file it does not have. */
  gamePath?: string | null;
  /** The source's own icon (a `data:` URL read from the package, folder or image before it
   *  reached the console). Tried before every console source. */
  sourceSrc?: string | null;
  /** Last-resort cover URL (e.g. external CDN) tried after the local sources. */
  fallbackSrc?: string | null;
  /** Alt text for the icon. Defaults to "" (decorative — use when the
   *  game title is already shown next to the icon). Pass the title
   *  when the icon stands alone (grid tiles, hero cards). */
  alt?: string;
  /** Square edge length in px. */
  size?: number;
  /** Tailwind rounding class for the frame. */
  rounded?: string;
  className?: string;
  /** Fill the parent (a CoverFrame) instead of a fixed `size` square. */
  fill?: boolean;
  /** The game's name, when known: with no art its initials stand in for the controller glyph,
   *  the same tile the Collection shows for a game without a cover. */
  label?: string | null;
}) {
  // A scaled copy from the engine: a full icon0.png is 300-700 KiB (#450).
  const px = fill ? GRID_ICON_SIZE : iconSizeFor(size);
  const candidates = gameIconCandidates({ host, titleId, gamePath, sourceSrc, fallbackSrc, px });
  const [idx, setIdx] = useState(0);
  // Reset to the first candidate whenever the set changes — otherwise an
  // instance reused for a different title (list reorder) would stay on a
  // stale fallback index even when the new title's primary art exists.
  const key = candidates.map((c) => c.url).join("|");
  useEffect(() => {
    setIdx(0);
  }, [key]);
  const current = idx < candidates.length ? candidates[idx] : null;
  // Retry the current candidate a couple of times before moving on. Every
  // source here answers 404 for a transient read miss as readily as for a
  // real absence, so advancing on the first error threw away art that was
  // simply a moment late. Only after the retries are spent does the chain
  // step to the next source, and then to the glyph.
  // The IPC fallback only applies to the two console-served sources; a
  // `sourceSrc` data URL or a `fallbackSrc` CDN URL needs none.
  const { src, onError, failed } = useImageRetry(current?.url ?? null, {
    // If the session already holds a console source's bytes there is nothing to fetch.
    cached: current?.cached?.(),
    // Only a console source has another transport. Offering a do-nothing loader for a `data:`
    // or CDN URL would, once the session found direct loads blocked, skip it unseen.
    fallbackLoader: current?.load,
  });
  const advance = useCallback(() => setIdx((i) => i + 1), [setIdx]);
  useEffect(() => {
    if (failed) advance();
  }, [failed, advance]);
  // The art shows only once it has loaded: while a source is retried or fails, the tile shows
  // the glyph (or the game's initials), never the browser's broken-image icon.
  const [loadedSrc, setLoadedSrc] = useState<string | null>(null);
  const loaded = !!src && loadedSrc === src;
  // An image the browser already had can finish before React's onLoad is attached.
  const imgRef = useCallback(
    (el: HTMLImageElement | null) => {
      if (el && src && el.complete && el.naturalWidth > 0) setLoadedSrc(src);
    },
    [src],
  );
  const initials = iconInitials(label);
  return (
    <div
      className={`relative flex shrink-0 items-center justify-center overflow-hidden bg-[var(--color-surface-3)] ${fill ? "h-full w-full" : ""} ${rounded} ${className}`}
      style={fill ? undefined : { width: size, height: size }}
    >
      {!loaded &&
        (initials ? (
          <span
            className="select-none font-semibold leading-none text-[var(--color-muted)]"
            style={{ fontSize: Math.max(10, Math.round(size * 0.36)) }}
            aria-hidden
          >
            {initials}
          </span>
        ) : (
          <Gamepad2 size={Math.round(size * 0.36)} className="text-[var(--color-muted)]" />
        ))}
      {src && (
        <img
          ref={imgRef}
          src={src}
          crossOrigin={engineImageCors(src)}
          alt={alt}
          onLoad={() => setLoadedSrc(src)}
          className={`absolute inset-0 h-full w-full object-cover ${loaded ? "" : "opacity-0"}`}
          // Safe here: none of this component's call sites sit inside a
          // `content-visibility: auto` row. If you ever place a <GameIcon>
          // inside one of index.css's *-contain classes, drop this — the
          // two together leave the image unloaded. See index.css.
          loading="lazy"
          // Failures go through useImageRetry first; it advances `failed`
          // only once the retries for this candidate are spent, and the
          // effect above then steps to the next source.
          onError={onError}
        />
      )}
    </div>
  );
}

/** Up to two initials of a game's name ("Quiet Ocean" → "QO"), or "" for none. */
export function iconInitials(label: string | null | undefined): string {
  return (label ?? "")
    .split(/\s+/)
    .filter((w) => /^[\p{L}\p{N}]/u.test(w))
    .slice(0, 2)
    .map((w) => [...w][0]!.toUpperCase())
    .join("");
}
