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
// The PS5 helper now speaks ONE protocol (AVA1) on ONE port, so the
// app addresses a console by its bare host: `consoleAddr(host)`. The
// engine owns the port and ignores any port a caller still sends.
// This module is the single source of truth.

/** The PS5 ELF loader port. Bound by every common PS5 homebrew
 *  loader (kstuff, ps5-payload-dev, EchoStretch). Accepts a raw
 *  ELF dump — no protocol framing. */
export const PS5_LOADER_PORT = 9021;

/** The port the ps5upload helper's AVA1 listener binds. Shown in copy and
 *  used for the firewall hint; never put in an address the app sends (the
 *  engine owns it: see `consoleAddr`). */
export const PS5_AVA1_PORT = 9120;

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

/** The console address the app sends to the engine: the bare host, with
 *  any port (`:9113`, `:9114`, `:9120`, or a persisted `host:port:port`)
 *  stripped. Accepts every shape `hostOf` does. */
export function consoleAddr(host: string): string {
  const bare = hostOf(host);
  // An IPv6 literal keeps a bracketed port so the engine can still split
  // host from port (it ignores the number): `fe80::1` alone is ambiguous.
  return bare.includes(":") ? withPort(bare, PS5_AVA1_PORT) : bare;
}

/** Alias of `consoleAddr`, kept so the ~350 call sites move in one step.
 *  New code calls `consoleAddr`. */
export const mgmtAddr = consoleAddr;

/** Alias of `consoleAddr` (see `mgmtAddr`). */
export const transferAddr = consoleAddr;
