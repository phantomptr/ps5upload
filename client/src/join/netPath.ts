// Which way a guest's connection to the host goes, as the guest sees it: "lan" (the home
// network), "tailscale" (both on one tailnet, through the Tailscale app) or "internet" (straight
// through both routers). The same rule as the engine's (join_net/rtc.rs reach_rank / path_of), so
// the guest's page and the host's panel say the same thing.

export type NetPath = "lan" | "tailscale" | "internet";

/** 0 the home network (RFC 1918, link-local, loopback, IPv6 ULA / link-local), 1 Tailscale
 *  (100.64.0.0/10, fd7a:115c:a1e0::/48), 2 anything else; null when `addr` is no IP address (an
 *  mDNS name a browser hides its own address behind, or nothing). */
export function reachRank(addr: string | null | undefined): 0 | 1 | 2 | null {
  const a = (addr ?? "").trim().replace(/^\[|\]$/g, "").toLowerCase();
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(a);
  if (v4) {
    const o = v4.slice(1).map(Number);
    if (o.some((n) => n > 255)) return null;
    const [x, y] = o;
    if (x === 10 || (x === 172 && y >= 16 && y < 32) || (x === 192 && y === 168) || (x === 169 && y === 254) || x === 127) return 0;
    if (x === 100 && y >= 64 && y < 128) return 1;
    return 2;
  }
  if (!a.includes(":") || !/^[0-9a-f:.]+$/.test(a)) return null;
  if (a.startsWith("fd7a:115c:a1e0:")) return 1;
  const first = parseInt(a.split(":")[0] || "0", 16);
  if ((first & 0xfe00) === 0xfc00 || (first & 0xffc0) === 0xfe80 || a === "::1") return 0;
  return 2;
}

/** The path of a selected pair: this device's address and the host's. Tailscale when either end
 *  is on the tailnet, the home network when the host's address is, else the internet; null when
 *  the host's address is not known. */
export function pathOf(local: string | null | undefined, remote: string | null | undefined): NetPath | null {
  const l = reachRank(local);
  const r = reachRank(remote);
  if (l === 1 || r === 1) return "tailscale";
  if (r === 0) return "lan";
  if (r === 2) return "internet";
  return null;
}

/** The path of a plain socket to `host` (the host's page on their Wi-Fi, or a typed address): a
 *  private address or a `.local` name is the home network, a 100.64/10 one Tailscale; a public
 *  name says nothing for sure (null). */
export function hostPath(host: string | null | undefined): NetPath | null {
  const h = (host ?? "").toLowerCase();
  if (h.endsWith(".local")) return "lan";
  if (h.endsWith(".ts.net")) return "tailscale";
  const r = reachRank(h);
  return r === 0 ? "lan" : r === 1 ? "tailscale" : null;
}

/** `hostPath` of a URL's host (null for what is no URL). */
export function urlPath(url: string): NetPath | null {
  try {
    return hostPath(new URL(url).hostname);
  } catch {
    return null;
  }
}

type Stat = Record<string, unknown> & { type?: string; id?: string };

/** The path of the candidate pair ICE selected, from `RTCPeerConnection.getStats()`: the
 *  transport's selected pair (Chrome, Safari), else the pair marked selected (Firefox), else a
 *  nominated pair that succeeded. */
export function selectedPath(report: Iterable<Stat> | { forEach(cb: (s: Stat) => void): void }): NetPath | null {
  const all: Stat[] = [];
  if (typeof (report as { forEach?: unknown }).forEach === "function") (report as { forEach(cb: (s: Stat) => void): void }).forEach((s) => all.push(s));
  else for (const s of report as Iterable<Stat>) all.push(s);
  const byId = new Map(all.map((s) => [String(s.id), s]));
  const pairs = all.filter((s) => s.type === "candidate-pair");
  const fromTransport = all
    .filter((s) => s.type === "transport" && typeof s.selectedCandidatePairId === "string")
    .map((s) => byId.get(String(s.selectedCandidatePairId)))
    .find(Boolean);
  const pair =
    fromTransport ??
    pairs.find((p) => p.selected === true) ??
    pairs.find((p) => p.nominated === true && p.state === "succeeded");
  if (!pair) return null;
  const addr = (id: unknown) => {
    const c = byId.get(String(id));
    if (!c) return null;
    const a = c.address ?? c.ip;
    return typeof a === "string" ? a : null;
  };
  return pathOf(addr(pair.localCandidateId), addr(pair.remoteCandidateId));
}
