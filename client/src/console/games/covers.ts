/**
 * Game covers for the Games grid. A console's icon0.png is 300-700 KiB at 512 px: a grid of a
 * hundred of them would hold tens of MiB in the PS5 browser and keep the helper busy. So a cover
 * is read only when its card scrolls into view, two at a time, drawn down to COVER_PX as a small
 * JPEG data URL, and kept for the page's life (at most CACHE_MAX).
 */
import { useEffect, useRef, useState, type RefObject } from "react";
import { ApiError, callBlob } from "../api";
import { TITLE_ID_RE } from "./model";

export const COVER_PX = 192;
const CONCURRENT = 2;
const CACHE_MAX = 600;

const cache = new Map<string, string>();
/** Titles with no cover (read failed or not an image): not asked again. */
const missing = new Set<string>();
const waiting: (() => void)[] = [];
let active = 0;

function pump(): void {
  while (active < CONCURRENT && waiting.length) {
    const job = waiting.shift()!;
    active++;
    job();
  }
}

function slot<T>(fn: () => Promise<T>): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    waiting.push(() => {
      fn()
        .then(resolve, reject)
        .finally(() => {
          active--;
          pump();
        });
    });
    pump();
  });
}

/**
 * `blob` drawn down to `px` square as a JPEG data URL. A picture that decodes but cannot be drawn
 * stays its object URL (revoked when the cache lets it go); bytes that are no picture at all give
 * null, with their URL revoked.
 */
function shrink(blob: Blob, px: number): Promise<string | null> {
  const url = URL.createObjectURL(blob);
  return new Promise<string | null>((resolve) => {
    const img = new Image();
    img.onload = () => {
      try {
        const c = document.createElement("canvas");
        c.width = px;
        c.height = px;
        const g = c.getContext("2d");
        if (!g) throw new Error("no 2d");
        g.drawImage(img, 0, 0, px, px);
        const data = c.toDataURL("image/jpeg", 0.85);
        URL.revokeObjectURL(url);
        resolve(data);
      } catch {
        resolve(url);
      }
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      resolve(null);
    };
    img.src = url;
  });
}

/** Drop a cached cover; one still held as an object URL gives its memory back. */
function forget(key: string): void {
  const url = cache.get(key);
  cache.delete(key);
  if (url?.startsWith("blob:")) URL.revokeObjectURL(url);
}

/** The cover of `titleId` (cached), or null when it has none. */
export function loadCover(titleId: string): Promise<string | null> {
  if (!TITLE_ID_RE.test(titleId)) return Promise.resolve(null);
  // appmeta first; a title a homebrew installer put on FW 13.60 has only its app folder (the engine's order).
  return loadImage(titleId, [`/user/appmeta/${titleId}/icon0.png`, `/user/app/${titleId}/sce_sys/icon0.png`]);
}

/** The first of `paths` that reads as a PNG/JPEG, drawn down to `px` and cached under `key`; null when none does. */
export async function loadImage(key: string, paths: string[], px: number = COVER_PX): Promise<string | null> {
  const titleId = key;
  const hit = cache.get(titleId);
  if (hit) return hit;
  if (missing.has(titleId) || !paths.length) return null;
  return slot(async () => {
    const again = cache.get(titleId);
    if (again) return again;
    let b: Blob | null = null;
    for (const path of paths) {
      try {
        const got = await callBlob("/api/fs/read", { path });
        if (/^image\/(png|jpeg)$/.test(got.type)) {
          b = got;
          break;
        }
      } catch (e) {
        if (!(e instanceof ApiError) || e.status === 0 || e.status === 401) return null;
      }
    }
    if (!b) {
      missing.add(titleId);
      return null;
    }
    try {
      const url = await shrink(b, px);
      if (!url) {
        missing.add(titleId);
        return null;
      }
      if (cache.size >= CACHE_MAX) {
        const first = cache.keys().next().value;
        if (first !== undefined) forget(first);
      }
      cache.set(titleId, url);
      return url;
    } catch {
      return null;
    }
  });
}

/**
 * One picture at full size (a detail view's cover): an object URL the caller revokes when it is
 * done with it (never cached: a 512 px icon0.png is up to 1 MiB). Null when it does not read as one.
 */
export async function loadFullImage(path: string): Promise<string | null> {
  try {
    const b = await slot(() => callBlob("/api/fs/read", { path }));
    return /^image\/(png|jpeg)$/.test(b.type) ? URL.createObjectURL(b) : null;
  } catch {
    return null;
  }
}

/** The cover once `ref`'s element has been on screen (at once where IntersectionObserver is missing). */
export function useLazyCover(titleId: string, ref: RefObject<Element | null>): string | null {
  return useLazyImage(titleId, ref, loadCover);
}

/** useLazyCover for any image: `load(key)` runs once the element has been on screen. */
export function useLazyImage(
  titleId: string,
  ref: RefObject<Element | null>,
  load: (key: string) => Promise<string | null>,
): string | null {
  const [url, setUrl] = useState<string | null>(() => cache.get(titleId) ?? null);
  const asked = useRef(false);
  useEffect(() => {
    let live = true;
    asked.current = false;
    setUrl(cache.get(titleId) ?? null);
    const ask = () => {
      if (asked.current) return;
      asked.current = true;
      void load(titleId).then((u) => {
        if (live) setUrl(u);
      });
    };
    const el = ref.current;
    if (!el || typeof IntersectionObserver !== "function") {
      ask();
      return () => {
        live = false;
      };
    }
    const io = new IntersectionObserver(
      (es) => {
        if (es.some((e) => e.isIntersecting)) {
          io.disconnect();
          ask();
        }
      },
      { rootMargin: "200px" },
    );
    io.observe(el);
    return () => {
      live = false;
      io.disconnect();
    };
  }, [titleId, ref, load]);
  return url;
}
