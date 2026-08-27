# Cover-art cache

*2026-08-27*

## Problem

Cover art is refetched from the console constantly.

Two things happen on every visit to a screen that shows artwork:

1. The engine reads `icon0.png` off the console over the mgmt port.
2. In the desktop app that read then goes through the IPC fallback added
   in 5.10.1, which base64-encodes the bytes into a `data:` URL held only
   in React state — so it dies on unmount and is redone on the next visit.

Measured against a live console: covers average **290 KB** (11 KB–532 KB),
and a 23-title library is **6.6 MB**, ~8.8 MB once base64'd. That is paid
again on every navigation.

Before 5.10.1 the direct URL carried `Cache-Control: private, max-age=300`,
so the browser held covers for five minutes. The IPC fallback bypasses HTTP
caching entirely, so that regression is ours.

## What gets cached, and what must not

| Data | Decision |
| --- | --- |
| Cover art (`app-icon`, `game-icon`) | **Cache on disk in the engine.** Large, slow, effectively immutable per title. |
| Installed-apps list | **No.** Small, and a stale list right after an install is exactly the wrong thing to show. |
| `pkg` metadata, title details | Already cached (localStorage; 7-day TTL). No new machinery. |
| Sensors, processes, transfers, connection status | **Never.** This is the live state the app exists to display. |

## Design

The cache lives **in the engine, on disk**, with a small bounded
in-memory layer in the client.

The engine is the only component that talks to the console, so caching
there means the console is read once per title rather than once per client:
desktop, browser build and Android all benefit from one implementation.

It also deliberately avoids putting durable state in the webview. This
session established that the desktop webview silently refuses network
loads that Chrome allows — betting the cache on the same layer that just
proved unpredictable, inside the same feature, is not a trade worth making.
Rust filesystem behaviour is boring and testable.

### Layout

```
$PS5UPLOAD_CACHE_DIR (default ~/.ps5upload/cache)/icons/<console>/<hash>.img
```

`<console>` is derived from the console address, which keeps one console's
artwork from ever being served under another's name — the same invariant
`scripts/check-per-console-isolation.sh` protects in the route tree. A
changed DHCP address costs a cold cache, never a wrong image.

`<hash>` is `sha256(kind | identity)`, where identity is the title id or
the folder path.

### Freshness — nothing is cached forever

Three independent bounds, because each catches a case the others miss:

- **TTL, 24 h for a hit.** Read from the file's mtime; past it the entry is
  a miss and gets refetched. mtime is never touched on read, so age means
  age-since-fetch and cannot be extended by traffic.
- **TTL, 15 min for a miss.** A 404 is recorded as a zero-byte marker so a
  title with no artwork stops costing a console round-trip on every render.
  Much shorter than a hit: a title that just *gained* artwork should not
  wait a day to show it.
- **Event invalidation.** Installing or uninstalling is the operation that
  actually changes what artwork exists, so it drops that console's entries
  immediately rather than waiting for the TTL.

### Size

A 512 MB ceiling, enforced opportunistically on write, evicting oldest
mtime first. Eviction is by age rather than by use, which keeps it
consistent with the TTL rule above and needs no index file to corrupt.

### Revalidation

The engine sends a strong `ETag` and honours `If-None-Match`, so the
direct-URL path answers a revalidation with a 304 and no body instead of
re-sending 290 KB when `max-age` lapses.

### Client memory layer

A module-level FIFO of `data:` URLs, capped at 24 MB, so repeated renders
within a session skip the IPC round-trip and the base64 encode. Session
lifetime only; the engine's TTL governs freshness of the bytes themselves.

## Deleting the cache

Cached artwork outlives the game it came from — the files remain after a
title is uninstalled. Small, and on the user's own machine, but it is data
that persists past its subject, so it must be visible and removable:

- `GET /api/cache/artwork` reports file count and total bytes.
- `DELETE /api/cache/artwork` removes it.
- Settings gains **Clear cached artwork**, showing the size it will free.
- The existing "wipe all local data" path removes it too.

## Failure behaviour

The cache is an optimisation and never a dependency. An unwritable or
missing cache directory disables caching with one log line; every route
behaves exactly as it does today. A corrupt or truncated entry is treated
as a miss and overwritten.

## Testing

- Rust unit tests over the cache module: hit, miss, TTL expiry for both
  positive and negative entries, per-console isolation, size eviction,
  and a read-only directory degrading to disabled rather than failing.
- An end-to-end check that a second request for the same cover performs no
  console round-trip.
