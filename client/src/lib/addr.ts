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

/** Extract the bare host (IP or DNS name) from anything shaped like
 *  `host`, `host:port`, or `host:port:port` (caused by the
 *  pre-2.12.0 `toMgmtAddr` footgun — accepted to defang any
 *  remaining legacy double-suffixed strings).
 *
 *  Returns the input unchanged if no colon is present. Empty string
 *  in / empty string out. */
export function hostOf(addr: string): string {
  if (!addr) return "";
  const i = addr.indexOf(":");
  return i < 0 ? addr : addr.slice(0, i);
}

/** Combine a host with a port number. `host` may include a port
 *  suffix already — we strip it first via `hostOf` so callers don't
 *  have to remember which shape they hold. */
export function withPort(host: string, port: number): string {
  const bare = hostOf(host);
  if (!bare) return "";
  return `${bare}:${port}`;
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
  const i = addr.lastIndexOf(":");
  if (i < 0) return null;
  const n = parseInt(addr.slice(i + 1), 10);
  return Number.isFinite(n) && n > 0 && n <= 65535 ? n : null;
}
