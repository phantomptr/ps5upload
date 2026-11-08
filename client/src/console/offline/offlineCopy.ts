/**
 * The page's offline copy, so opening the home-screen tile while the helper is
 * not running shows the page's own "not running" screen instead of a browser
 * error (Payload Manager does the same):
 *
 *   - a service worker (GET /sw.js, scope /): 127.0.0.1 is a secure context,
 *     so the PS5 browser can run one there. Network first for the page (/) and
 *     the strings (/strings/…); what arrives is stored, and it is answered
 *     from the store only when the network fails. Nothing else is touched:
 *     /api/* never reaches the store, POSTs are never intercepted.
 *   - the appcache (<html manifest="/minion.appcache">; pages up to 11.6.x
 *     used /cache.appcache, which the helper now answers 410 so the browser
 *     drops that copy), the fallback the PS5 browser honours for Payload
 *     Manager: the page is its only cached entry (NETWORK: * lets everything
 *     else through). An appcache serves its copy first and updates behind it,
 *     so on "updateready" the page offers a one-press reload (UpdateBanner,
 *     update.ts); a page older than the helper replaces itself without a
 *     press (selfUpdate.ts).
 *
 * vite.console.config.ts writes both files next to the page with the page's
 * hash in them (a new build = a new cache), and payload/Makefile embeds them in
 * the helper. With the copy shown and the helper away, GET /api/token fails:
 * main.tsx shows the offline screen (OfflinePage) and reloads once it answers.
 */

import { markUpdateReady } from "./update.ts";

/** The service worker. `__CW_SW_V__` is replaced with the page's build hash. */
export const SW_SOURCE = `/* PS5Upload Minion: offline copy of the page (generated; see offlineCopy.ts) */
"use strict";
var CACHE = "cw-page-__CW_SW_V__";
function cacheable(url) {
  if (url.origin !== self.location.origin) return false;
  if (url.pathname.indexOf("/api/") === 0) return false;
  return url.pathname === "/" || url.pathname.indexOf("/strings/") === 0;
}
self.addEventListener("install", function (e) {
  e.waitUntil(
    caches.open(CACHE)
      .then(function (c) { return c.add(new Request(self.location.origin + "/", { cache: "reload" })); })
      .catch(function () {})
      .then(function () { return self.skipWaiting(); })
  );
});
self.addEventListener("activate", function (e) {
  e.waitUntil(
    caches.keys()
      .then(function (keys) {
        return Promise.all(keys.filter(function (k) { return k.indexOf("cw-") === 0 && k !== CACHE; })
          .map(function (k) { return caches.delete(k); }));
      })
      .then(function () { return self.clients.claim(); })
  );
});
self.addEventListener("fetch", function (e) {
  var req = e.request;
  if (req.method !== "GET") return;
  var url = new URL(req.url);
  if (!cacheable(url)) return;
  var key = url.pathname === "/" ? "/" : req;
  e.respondWith(
    fetch(req).then(
      function (res) {
        if (res && res.ok) {
          var copy = res.clone();
          caches.open(CACHE).then(function (c) { return c.put(key, copy); }).catch(function () {});
        }
        return res;
      },
      function () {
        return caches.open(CACHE)
          .then(function (c) { return c.match(key); })
          .then(function (hit) { return hit || Response.error(); });
      }
    )
  );
});
`;

/** The appcache manifest: the page (its master entry) and nothing else; every other URL from the network. */
export function appcacheManifest(version: string): string {
  return `CACHE MANIFEST\n# PS5Upload Minion ${version}\n\nNETWORK:\n*\n`;
}

export function serviceWorkerSource(version: string): string {
  if (!/^[0-9a-z]+$/.test(version)) throw new Error(`offlineCopy: bad version ${version}`);
  return SW_SOURCE.split("__CW_SW_V__").join(version);
}

interface AppCacheLike {
  addEventListener(type: "updateready", fn: () => void): void;
  swapCache?: () => void;
}

/**
 * Register the service worker (where the browser has one and the page is a secure context: the
 * console's own browser at 127.0.0.1, not a phone at the console's LAN address) and follow the
 * appcache's updates. Never throws; nothing here is needed for the page to work.
 */
export function installOfflineCopy(win: Window = window, onUpdate: () => void = markUpdateReady): void {
  try {
    const nav = win.navigator as Navigator & { serviceWorker?: ServiceWorkerContainer };
    if (win.isSecureContext && nav.serviceWorker && typeof nav.serviceWorker.register === "function") {
      nav.serviceWorker.register("/sw.js", { scope: "/" }).catch(() => undefined);
    }
  } catch {
    /* no service worker: the appcache (if any) still works */
  }
  try {
    const ac = (win as unknown as { applicationCache?: AppCacheLike }).applicationCache;
    if (ac && typeof ac.addEventListener === "function") {
      ac.addEventListener("updateready", () => {
        try {
          ac.swapCache?.();
        } catch {
          /* already swapped */
        }
        onUpdate(); // the banner: one press reloads into the new page
      });
    }
  } catch {
    /* no appcache */
  }
}
