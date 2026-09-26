// Canonical address helpers for talking to a PS5.
//
// Before 2.12.0 this lived as four ad-hoc functions scattered across
// `state/installQueue.ts::bareIp`, `state/transfer.ts::hostFromAddr`,
// `screens/InstallPackage/index.tsx::toMgmtAddr` (takes BARE host),
// and `screens/FileSystem/index.tsx::toMgmtAddr` (takes TRANSFER
// addr — same name, different signature!). A refactor that swapped
// the two `toMgmtAddr` calls would silently produce
// `"192.168.1.2:9113:9114"` and connect to the wrong port.
//
// The PS5 listens on three different ports for three different
// protocols. Conflating them by accident is a real silent footgun.
// This module is the single source of truth.

/** The PS5 ELF loader port. Bound by every common PS5 homebrew
 *  loader (kstuff, ps5-payload-dev, EchoStretch). Accepts a raw
 *  ELF dump — no protocol framing. */
export const PS5_LOADER_PORT = 9021;

/** ps5upload payload's bulk-transfer port. FTX2 protocol. Single-
 *  client (concurrent FTX2 connections serialize at the socket). */
export const PS5_TRANSFER_PORT = 9113;

/** ps5upload payload's management port. FS_* and miscellaneous
 *  RPCs. Multi-client. */
export const PS5_MGMT_PORT = 9114;

/** True for a bare (un-bracketed) IPv6 literal such as `fe80::1` or
 *  `2001:db8::5`: 2+ colons and no dotted-quad. We use the absence of
 *  a `.` to disambiguate from IPv4/hostname forms (incl. the legacy
 *  `host:port:port` footgun, which always carries dots in the host). */
function isBareIpv6(addr: string): boolean {
  return !addr.includes(".") && (addr.match(/:/g)?.length ?? 0) >= 2;
}

/** Extract the bare host (IP or DNS name) from anything shaped like
 *  `host`, `host:port`, `host:port:port` (the pre-2.12.0 `toMgmtAddr`
 *  footgun), a bracketed IPv6 `[host]` / `[host]:port`, or a bare
 *  IPv6 literal `fe80::1`.
 *
 *  Returns the input unchanged if there's nothing to strip. Empty
 *  string in / empty string out. */
export function hostOf(addr: string): string {
  if (!addr) return "";
  // Bracketed IPv6: `[host]` or `[host]:port` → the inner host.
  if (addr.startsWith("[")) {
    const end = addr.indexOf("]");
    return end > 0 ? addr.slice(1, end) : addr.slice(1);
  }
  // Bare IPv6 literal: can't separate a port from an un-bracketed
  // literal, and by convention it carries none — return it whole.
  // (A naive indexOf(":") here would truncate `fe80::1` to `fe80`.)
  if (isBareIpv6(addr)) return addr;
  // IPv4 / hostname, optionally with a :port (or legacy :port:port) —
  // the host is everything before the first colon.
  const i = addr.indexOf(":");
  return i < 0 ? addr : addr.slice(0, i);
}

/** Combine a host with a port number. `host` may include a port
 *  suffix already — we strip it first via `hostOf` so callers don't
 *  have to remember which shape they hold. IPv6 literals are
 *  bracketed so `[fe80::1]:9114` parses unambiguously. */
export function withPort(host: string, port: number): string {
  const bare = hostOf(host);
  if (!bare) return "";
  // An IPv6 literal must be bracketed before a :port is appended.
  const needsBrackets = bare.includes(":") && !bare.startsWith("[");
  return needsBrackets ? `[${bare}]:${port}` : `${bare}:${port}`;
}

/** Address for the management port (`host:9114`). Accepts any of
 *  the three shapes. Equivalent to `withPort(host, PS5_MGMT_PORT)`
 *  but the named helper makes intent obvious at call sites. */
export function mgmtAddr(host: string): string {
  return withPort(host, PS5_MGMT_PORT);
}

/** Address for the bulk-transfer port (`host:9113`). */
export function transferAddr(host: string): string {
  return withPort(host, PS5_TRANSFER_PORT);
}

/** Address for the loader port (`host:9021`). */
export function loaderAddr(host: string): string {
  return withPort(host, PS5_LOADER_PORT);
}

/** Extract the port from a `host:port` string. Returns `null` when
 *  there's no port suffix. Useful for the few diagnostic call sites
 *  that need to surface "which port were we talking to?" in error
 *  text. */
export function portOf(addr: string): number | null {
  if (!addr) return null;
  // Bracketed IPv6: the port (if any) follows `]:`.
  if (addr.startsWith("[")) {
    const sep = addr.indexOf("]:");
    if (sep < 0) return null;
    const n = parseInt(addr.slice(sep + 2), 10);
    return Number.isFinite(n) && n > 0 && n <= 65535 ? n : null;
  }
  // Bare IPv6 literal carries no port (every colon is part of the host).
  if (isBareIpv6(addr)) return null;
  const i = addr.lastIndexOf(":");
  if (i < 0) return null;
  const n = parseInt(addr.slice(i + 1), 10);
  return Number.isFinite(n) && n > 0 && n <= 65535 ? n : null;
}
