// Best-effort PSN store metadata fetch for a PS5 title id. Calls
// PlayStation's public titlecontainer + valkyrie endpoints without
// an API key. The Library shows a Game Details modal that uses
// this to render the official cover art + description alongside
// whatever local sce_sys/param.json we have.
//
// **Why we keep this separate from api/ps5.ts**: those calls go
// through Tauri IPC into the engine; this is plain `fetch()` against
// public PSN endpoints, has no auth, and degrades gracefully on
// failure (network down, region mismatch, title not in store) — the
// modal still shows local data when the fetch fails, and we cache
// per-title so we don't re-fetch on every modal open.
//
// PSN store layout:
//   - Resolve URL:
//     https://store.playstation.com/valkyrie-api/{lang}/{country}/19/resolve/{titleId}_{suffix}
//     where {suffix} = "00" most of the time. Try a list of common
//     region pairs (en/US, en/GB, en/DE, en/JP) to find a match.
//   - Title container fallback:
//     https://store.playstation.com/store/api/chihiro/00_09_000/titlecontainer/{lang}/{country}/999/{titleId}
//     Older API, sometimes returns when valkyrie's resolve doesn't.
//   - Search fallback opens
//     https://store.playstation.com/en-gb/search/{query}
//     in the user's browser when the API gives up — same UX the suite
//     ships.
//
// Persisted cache: localStorage `ps5upload.psn.cache` keyed by
// titleId, value is `{ ts, info | null }` where ts is the fetch
// timestamp. Cache lifetime is 7 days — long enough to skip the
// fetch on every modal open, short enough that a corrected title
// shows up within a week.

const CACHE_KEY = "ps5upload.psn.cache";
const CACHE_TTL_MS = 7 * 24 * 60 * 60 * 1000;

const RESOLVE_REGIONS = ["en/US", "en/GB", "en/DE", "en/JP"] as const;

export interface PsnGameInfo {
  /** Display title from the store (often slightly different from
   *  what param.json says — e.g. regional subtitle). */
  title?: string;
  /** Long-form description / about text. May contain plain HTML;
   *  consumers should render with `dangerouslySetInnerHTML` only
   *  after sanitizing. The modal renders text-only. */
  description?: string;
  /** Highest-quality cover-art URL we found. PSN serves these via
   *  CDN with no auth. */
  coverImageUrl?: string;
  /** "Game" / "Bundle" / "DLC" etc. as classified by PSN. */
  contentType?: string;
  /** Genre tags. */
  genres?: string[];
  publisher?: string;
  /** ISO-ish age rating from PSN (e.g. "ESRB MATURE 17+"). */
  ageRating?: string;
}

interface CacheEntry {
  ts: number;
  info: PsnGameInfo | null;
}

type CacheMap = Record<string, CacheEntry>;

function loadCache(): CacheMap {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(CACHE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {};
    }
    return parsed as CacheMap;
  } catch {
    return {};
  }
}

function saveCache(map: CacheMap): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(CACHE_KEY, JSON.stringify(map));
  } catch {
    // localStorage write can fail (quota, private mode). PSN cache
    // is purely an optimisation; swallow.
  }
}

/** Look up a cached PSN result, returning null if missing or stale. */
export function readPsnCache(titleId: string): PsnGameInfo | null | undefined {
  const map = loadCache();
  const entry = map[titleId];
  if (!entry) return undefined;
  if (Date.now() - entry.ts > CACHE_TTL_MS) return undefined;
  return entry.info;
}

/** Persist a PSN result. `null` is a valid value — it means "we
 *  fetched and PSN had no data," and we want to remember that so
 *  the modal doesn't retry on every open. */
export function writePsnCache(titleId: string, info: PsnGameInfo | null): void {
  const map = loadCache();
  map[titleId] = { ts: Date.now(), info };
  saveCache(map);
}

interface ResolveResponse {
  data?: {
    relationships?: {
      children?: {
        data?: Array<{ id?: string; type?: string }>;
      };
    };
  };
  included?: Array<{
    id?: string;
    type?: string;
    attributes?: {
      name?: string;
      "long-description"?: string;
      "thumbnail-url-base"?: string;
      "media-list"?: {
        screenshots?: Array<{ url?: string }>;
        previews?: Array<{ url?: string }>;
      };
      "primary-classification"?: string;
      "content-rating"?: { url?: string; description?: string };
      "publisher-name"?: string;
      "secondary-classification"?: string;
      "tertiary-classification"?: string;
      genres?: string[];
    };
  }>;
}

function parseResolve(payload: ResolveResponse): PsnGameInfo | null {
  const included = payload.included ?? [];
  const game = included.find(
    (e) => e.type === "game" || e.type === "game-related",
  );
  if (!game?.attributes) return null;
  const attrs = game.attributes;
  return {
    title: attrs.name,
    description: attrs["long-description"]?.replace(/\s+/g, " ").trim(),
    coverImageUrl: attrs["thumbnail-url-base"]
      ? `${attrs["thumbnail-url-base"]}?w=720`
      : undefined,
    contentType: attrs["primary-classification"],
    genres: attrs.genres,
    publisher: attrs["publisher-name"],
    ageRating: attrs["content-rating"]?.description,
  };
}

interface TitleContainerResponse {
  name?: string;
  long_desc?: string;
  images?: Array<{ url?: string; type?: number }>;
  star_rating?: { score?: number };
  content_descriptors?: Array<{ description?: string }>;
  publisher_name?: string;
  game_contentType?: string;
  category?: string;
}

function parseTitleContainer(payload: TitleContainerResponse): PsnGameInfo | null {
  if (!payload?.name) return null;
  // type 1 = cover art (boxart) on the legacy API.
  const coverImage = payload.images?.find((i) => i.type === 1)?.url;
  return {
    title: payload.name,
    description: payload.long_desc?.replace(/\s+/g, " ").trim(),
    coverImageUrl: coverImage,
    contentType: payload.game_contentType ?? payload.category,
    publisher: payload.publisher_name,
  };
}

/** Try the modern valkyrie resolve endpoint across a small list of
 *  regions. Returns the first match. */
async function fetchValkyrie(titleId: string, signal?: AbortSignal): Promise<PsnGameInfo | null> {
  for (const region of RESOLVE_REGIONS) {
    try {
      const url = `https://store.playstation.com/valkyrie-api/${region}/19/resolve/${titleId}_00`;
      const res = await fetch(url, { signal });
      if (!res.ok) continue;
      const json = (await res.json()) as ResolveResponse;
      const parsed = parseResolve(json);
      if (parsed?.title || parsed?.description || parsed?.coverImageUrl) {
        return parsed;
      }
    } catch {
      // network or parse failure → try the next region
    }
  }
  return null;
}

/** Older chihiro `titlecontainer` endpoint. Sometimes returns when
 *  valkyrie's resolve doesn't (typically for very old titles or
 *  ones missing from the modern catalog). */
async function fetchTitleContainer(titleId: string, signal?: AbortSignal): Promise<PsnGameInfo | null> {
  const langs = ["en/US", "en/GB"] as const;
  for (const lang of langs) {
    try {
      const [l, c] = lang.split("/");
      const url = `https://store.playstation.com/store/api/chihiro/00_09_000/titlecontainer/${l}/${c}/999/${titleId}`;
      const res = await fetch(url, { signal });
      if (!res.ok) continue;
      const json = (await res.json()) as TitleContainerResponse;
      const parsed = parseTitleContainer(json);
      if (parsed) return parsed;
    } catch {
      // try next lang
    }
  }
  return null;
}

/** Fetch PSN metadata for a title id. Hits the cache first, then
 *  valkyrie, then titlecontainer fallback. Returns null on miss
 *  (and the result is cached so subsequent calls don't re-fetch). */
export async function fetchPsnGameInfo(
  titleId: string,
  signal?: AbortSignal,
): Promise<PsnGameInfo | null> {
  if (!titleId || typeof titleId !== "string") return null;
  const trimmed = titleId.trim();
  // Title id format check — PS5 ids are 4 letters + 5 digits
  // (CUSAxxxxx, PPSAxxxxx, NPXSxxxxx). Reject obviously malformed
  // values so we don't waste a network round-trip.
  if (!/^[A-Z]{4}\d{5}$/.test(trimmed)) return null;

  const cached = readPsnCache(trimmed);
  if (cached !== undefined) return cached;

  const fromValkyrie = await fetchValkyrie(trimmed, signal);
  if (fromValkyrie) {
    writePsnCache(trimmed, fromValkyrie);
    return fromValkyrie;
  }
  const fromContainer = await fetchTitleContainer(trimmed, signal);
  writePsnCache(trimmed, fromContainer);
  return fromContainer;
}

/** Build the URL the PSN search page should open if our API fetch
 *  comes back empty. The suite uses the same en-gb path. */
export function psnStoreSearchUrl(query: string): string {
  return `https://store.playstation.com/en-gb/search/${encodeURIComponent(query)}`;
}
