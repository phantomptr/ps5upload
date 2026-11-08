// "One invite": an internet link (`#room=…&k=…&fp=…&hub=…`) also names the host's own page on
// their home network (`&lan=http%3A%2F%2F192.168.1.10%3A19113%2Fjoin`, engine join_net link_for).
// When this page can't reach the invite service (the hub) within a few seconds, a friend who is on
// the host's Wi-Fi can still play: "Open on this Wi-Fi" goes to that page. It is a top-level
// navigation the friend taps: a secure page may not fetch a plain-http LAN address (mixed content,
// Private Network Access), but it may be left for one. The hint is no secret: the page there asks
// the host, as the link's own key does (the join page gets the same invite key; the phone link's
// LAN page none, so the phone asks).

/** How long the page gives the hub before it offers the Wi-Fi's page. */
export const HUB_PROBE_MS = 4000;

/** The home network's page the link names (`lan`), or null: http(s) only, ending in /join or /pad
 *  (an engine served under a path keeps it), no fragment or query of its own. */
export function lanPage(hash: string, kind: "join" | "pad"): string | null {
  const raw = new URLSearchParams(hash.replace(/^#/, "")).get("lan");
  if (!raw) return null;
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  if (u.username || u.password || u.search || u.hash) return null;
  if (!new RegExp(`/${kind}/?$`).test(u.pathname)) return null;
  return `${u.origin}${u.pathname}`;
}

/** Where "Open on this Wi-Fi" goes: the join page with the invite's key, or the phone link's page
 *  (no key: the phone asks the computer there), `w=1` to watch. */
export function lanTarget(page: string, o: { key?: string; watch?: boolean }): string {
  const parts: string[] = [];
  if (o.key) parts.push(`i=${encodeURIComponent(o.key)}`);
  if (o.watch) parts.push("w=1");
  return parts.length ? `${page}#${parts.join("&")}` : page;
}

/** Whether the hub answers within `ms`: a WebSocket that opens (closed again at once). False when
 *  it can't be made, closes first, or takes longer. */
export function probeHub(url: string, ms = HUB_PROBE_MS, WS: typeof WebSocket | undefined = typeof WebSocket === "undefined" ? undefined : WebSocket): Promise<boolean> {
  return new Promise((resolve) => {
    if (!WS) {
      resolve(false);
      return;
    }
    let ws: WebSocket;
    try {
      ws = new WS(url);
    } catch {
      resolve(false);
      return;
    }
    let done = false;
    const end = (ok: boolean) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      ws.onopen = ws.onclose = ws.onerror = null;
      try {
        ws.close();
      } catch {
        // Already closed.
      }
      resolve(ok);
    };
    const timer = setTimeout(() => end(false), ms);
    ws.onopen = () => end(true);
    ws.onclose = () => end(false);
    ws.onerror = () => end(false);
  });
}
