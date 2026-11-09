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
 *  (an engine served under a path keeps it), no fragment or query of its own. `param` "ts": the
 *  same page at the host's Tailscale address (engine join_net link_for). */
export function lanPage(hash: string, kind: "join" | "pad", param: "lan" | "ts" = "lan"): string | null {
  const raw = new URLSearchParams(hash.replace(/^#/, "")).get(param);
  if (!raw) return null;
  try {
    const u = new URL(raw);
    const page = u.origin + u.pathname;
    // Anything more (a user, a query, a fragment, another scheme) is not the engine's page.
    return /^https?:$/.test(u.protocol) && raw === page && new RegExp(`/${kind}/?$`).test(u.pathname) ? page : null;
  } catch {
    return null;
  }
}

/** Where "Open on this Wi-Fi" goes: the join page with the invite's key, or the phone link's page
 *  (no key: the phone asks the computer there), `w=1` to watch. */
export function lanTarget(page: string, o: { key?: string; watch?: boolean }): string {
  return o.key ? `${page}#i=${encodeURIComponent(o.key)}` : o.watch ? `${page}#w=1` : page;
}

/** Whether the hub answers within `ms`: a WebSocket that opens (closed again at once). False when
 *  it can't be made, closes first, or takes longer. */
export function probeHub(url: string, ms = HUB_PROBE_MS, WS: typeof WebSocket | undefined = typeof WebSocket === "undefined" ? undefined : WebSocket): Promise<boolean> {
  return new Promise((resolve) => {
    try {
      const ws = new WS!(url);
      const end = (ok: boolean) => {
        clearTimeout(timer);
        ws.onopen = ws.onclose = ws.onerror = null;
        ws.close();
        resolve(ok);
      };
      const timer = setTimeout(() => end(false), ms);
      ws.onopen = () => end(true);
      ws.onclose = ws.onerror = () => end(false);
    } catch {
      // No WebSocket here, or the browser would not make one.
      resolve(false);
    }
  });
}
