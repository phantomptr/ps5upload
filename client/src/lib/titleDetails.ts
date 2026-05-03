// Best-effort title metadata fetch for a PS5 title id. Scrapes
// public title pages on prosperopatches.com — they fill `<title>`
// and a `<meta name="twitter:image">` cover URL for known titles
// (PPSAxxxxx and CUSAxxxxx), and serve images off
// cdn.prosperopatches.com. The Library's Game Details modal renders
// the cover thumbnail + display title alongside whatever local
// sce_sys/param.json we have.
//
// **Why these calls go through Tauri**: the renderer's CSP
// `connect-src` does not whitelist external hosts, and a cross-origin
// fetch would not satisfy the webview's CORS policy regardless. We
// invoke a Rust-side `title_meta_fetch` command — the request is
// issued from the desktop process (no CSP, no CORS), and a hostname
// allowlist there acts as SSRF defense in case a compromised
// renderer tries to pivot through this command.
//
// Persisted cache: localStorage `ps5upload.titleinfo.cache` keyed
// by titleId, value is `{ ts, info | null }` where ts is the fetch
// timestamp. Cache lifetime is 7 days — long enough to skip the
// fetch on every modal open, short enough that a corrected title
// shows up within a week.

import { invoke } from "@tauri-apps/api/core";

const CACHE_KEY = "ps5upload.titleinfo.cache";
const CACHE_TTL_MS = 7 * 24 * 60 * 60 * 1000;

export interface TitleInfo {
  /** Display title scraped from the page's `<title>` tag (after
   *  stripping the "TITLEID: " prefix). Often more user-friendly
   *  than what param.json says. */
  title?: string;
  /** Highest-quality cover-art URL we found. Served from
   *  cdn.prosperopatches.com with no auth. */
  coverImageUrl?: string;
}

interface CacheEntry {
  ts: number;
  info: TitleInfo | null;
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
    // localStorage write can fail (quota, private mode). Title cache
    // is purely an optimisation; swallow.
  }
}

/** Look up a cached result, returning undefined if missing or stale. */
export function readTitleCache(titleId: string): TitleInfo | null | undefined {
  const map = loadCache();
  const entry = map[titleId];
  if (!entry) return undefined;
  if (Date.now() - entry.ts > CACHE_TTL_MS) return undefined;
  return entry.info;
}

/** Persist a result. `null` is a valid value — it means "we fetched
 *  and the source had nothing usable," and we want to remember that
 *  so the modal doesn't retry on every open. */
export function writeTitleCache(titleId: string, info: TitleInfo | null): void {
  const map = loadCache();
  map[titleId] = { ts: Date.now(), info };
  saveCache(map);
}

/** Fetch a URL through the Rust-side `title_meta_fetch` command,
 *  which enforces a hostname allowlist and bypasses renderer
 *  CSP/CORS. The AbortSignal short-circuits the await; the in-flight
 *  Rust request cannot be cancelled, but the caller never observes
 *  the result. */
async function titleMetaFetchText(
  url: string,
  signal?: AbortSignal,
): Promise<string> {
  if (signal?.aborted) {
    throw new DOMException("aborted", "AbortError");
  }
  const fetchPromise = invoke<string>("title_meta_fetch", { url });
  if (!signal) return fetchPromise;
  return await new Promise<string>((resolve, reject) => {
    const onAbort = () => {
      reject(new DOMException("aborted", "AbortError"));
    };
    signal.addEventListener("abort", onAbort, { once: true });
    fetchPromise.then(
      (v) => {
        signal.removeEventListener("abort", onAbort);
        resolve(v);
      },
      (e) => {
        signal.removeEventListener("abort", onAbort);
        reject(e);
      },
    );
  });
}

/** Parse a prosperopatches title page. The two pieces we care about
 *  are stable in the static HTML (no JS execution required):
 *
 *    <title>PPSA01285: Returnal</title>
 *    <meta name="twitter:image" content="https://cdn.prosperopatches.com/...">
 *
 *  Returns null when neither was extractable — typically a stub
 *  page for an unknown title id, or markup we don't recognise. */
export function parseProsperoPatchesHtml(html: string): TitleInfo | null {
  // Use DOMParser when available (renderer); fall back to regex when
  // not (tests sometimes run without a DOM polyfill).
  let title: string | undefined;
  let coverImageUrl: string | undefined;

  if (typeof DOMParser !== "undefined") {
    const doc = new DOMParser().parseFromString(html, "text/html");
    const rawTitle = doc.querySelector("title")?.textContent ?? "";
    title = stripTitleIdPrefix(rawTitle);
    const meta =
      doc.querySelector('meta[name="twitter:image"]') ??
      doc.querySelector('meta[property="og:image"]');
    const content = meta?.getAttribute("content") ?? "";
    if (content && /^https:\/\/cdn\.prosperopatches\.com\//.test(content)) {
      coverImageUrl = content;
    }
  } else {
    const titleMatch = html.match(/<title>([^<]*)<\/title>/i);
    if (titleMatch) title = stripTitleIdPrefix(titleMatch[1]);
    const imgMatch = html.match(
      /<meta[^>]+(?:name="twitter:image"|property="og:image")[^>]+content="([^"]+)"/i,
    );
    if (imgMatch && /^https:\/\/cdn\.prosperopatches\.com\//.test(imgMatch[1])) {
      coverImageUrl = imgMatch[1];
    }
  }

  if (!title && !coverImageUrl) return null;
  return { title, coverImageUrl };
}

/** "PPSA01285: Returnal" → "Returnal".
 *  "CUSA12345: Foo Bar" → "Foo Bar".
 *  Anything not matching the prefix is returned trimmed as-is. */
function stripTitleIdPrefix(raw: string): string | undefined {
  const trimmed = raw.trim();
  if (!trimmed) return undefined;
  const m = trimmed.match(/^[A-Z]{4}\d{5}:\s*(.+?)\s*$/);
  return m ? m[1] : trimmed;
}

/** Fetch title metadata for a title id. Hits the cache first, then
 *  scrapes prosperopatches.com. Returns null on miss (and the result
 *  is cached so subsequent calls don't re-fetch). */
export async function fetchTitleInfo(
  titleId: string,
  signal?: AbortSignal,
): Promise<TitleInfo | null> {
  if (!titleId || typeof titleId !== "string") return null;
  const trimmed = titleId.trim();
  // Title id format check — PS5 ids are 4 letters + 5 digits
  // (CUSAxxxxx, PPSAxxxxx, NPXSxxxxx). Reject obviously malformed
  // values so we don't waste a network round-trip.
  if (!/^[A-Z]{4}\d{5}$/.test(trimmed)) return null;

  const cached = readTitleCache(trimmed);
  if (cached !== undefined) return cached;

  try {
    const url = `https://prosperopatches.com/${trimmed}`;
    const html = await titleMetaFetchText(url, signal);
    const parsed = parseProsperoPatchesHtml(html);
    writeTitleCache(trimmed, parsed);
    return parsed;
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") return null;
    // Unknown title (404), network failure, body cap, etc. Cache
    // null so we don't re-hit the network on every modal open.
    writeTitleCache(trimmed, null);
    return null;
  }
}

/** Build the URL the user can click through to to view the full
 *  title page (patch list, content IDs, etc.) on the upstream site. */
export function prosperoPatchesUrl(titleId: string): string {
  return `https://prosperopatches.com/${encodeURIComponent(titleId)}`;
}
