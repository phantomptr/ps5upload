import type { InstallStatus } from "../api/ps5";
import { formatBytes } from "./format";
import { trStatic } from "./trStatic";

/** trStatic with {name} placeholders filled in. */
function trVars(key: string, fallback: string, vars: Record<string, string>): string {
  return trStatic(key, fallback).replace(/\{(\w+)\}/g, (m, k: string) => vars[k] ?? m);
}

/** What a Sony install code means, as [i18n key, English]. Only codes whose meaning is
 *  established: Sony's installer error names (the same table the installer daemon uses in
 *  payload/installer/hints.c) and codes measured on our consoles or in bug reports. */
const SONY_CODES: Record<number, [string, string]> = {
  0x80a30002: ["pkg.code.nospace", "not enough free space on the PS5"],
  0x80a30003: ["pkg.code.url_too_long", "the install link was too long for the PS5"],
  0x80a30004: ["pkg.code.no_base", "the base game is not installed"],
  0x80a3000a: ["pkg.code.no_base", "the base game is not installed"],
  0x80a30008: ["pkg.code.broken", "the package is broken or of the wrong type"],
  0x80a30009: ["pkg.code.broken", "the package is broken or of the wrong type"],
  0x80a3000c: ["pkg.code.running", "the game is running; close it first"],
  0x80a3000d: ["pkg.code.firmware", "the console's firmware is too old for this package"],
  0x80a3000f: ["pkg.code.other_game", "this update is for a different game"],
  0x80a30011: ["pkg.code.base_version", "the installed game is the wrong version for this update"],
  0x80a30017: ["pkg.code.dlc_base", "this DLC needs its base content installed first"],
  0x80a30019: ["pkg.code.bad_patch", "the update package is invalid or damaged"],
  0x80b21104: ["pkg.code.not_accepted", "the PS5 does not accept this package"],
  0x80b21106: ["pkg.code.name_mismatch", "the file name does not match the package inside it"],
  0x80b2116f: ["pkg.code.staged", "the PS5 refused its own copy of the package"],
  0x80b2150f: ["pkg.code.staged", "the PS5 refused its own copy of the package"],
  0x80b211cd: ["pkg.code.bad_read", "the PS5 got data it did not expect while reading the package"],
  0x80b22404: ["pkg.code.fetch_failed", "the PS5 could not keep fetching the package"],
  0x80b22416: ["pkg.code.bad_range", "the PS5 was refused a part of the package it asked for"],
  0x80431064: ["pkg.code.unreachable", "the PS5 could not reach this computer"],
  0x80431068: ["pkg.code.unreachable", "the PS5 could not reach this computer"],
  0x80431084: ["pkg.code.proxy", "the PS5's proxy setting blocked it"],
};

function hex(code: number): string {
  return `0x${(code >>> 0).toString(16).toUpperCase().padStart(8, "0")}`;
}

/** One line naming Sony's code and, when known, what it means. */
export function sonyCodeLine(code: number): string {
  if (!code) {
    return trStatic(
      "pkg.decline.no_code",
      "The PS5 gave no error code. Its Notifications may show one.",
    );
  }
  const known = SONY_CODES[code >>> 0];
  return known
    ? trVars("pkg.decline.code_known", "Error {code}: {meaning}.", {
        code: hex(code),
        meaning: trStatic(known[0], known[1]),
      })
    : trVars("pkg.decline.code_unknown", "Error {code}.", { code: hex(code) });
}

/** One line saying how far the PS5 got, which points at the cause: refused before it read
 *  anything (the package or the request), part-way (the connection), or after reading it all
 *  (space, or the content failed Sony's checks). Empty when the engine didn't measure it. */
export function progressLine(served: number, total: number): string {
  if (!total) return "";
  if (served <= 0) {
    return trStatic(
      "pkg.decline.at_once",
      "It refused before downloading anything, so it rejected the package itself.",
    );
  }
  if (served >= total) {
    return trVars(
      "pkg.decline.after_all",
      "It downloaded all {total}, then refused. Check free space; otherwise the content failed the PS5's checks.",
      { total: formatBytes(total) },
    );
  }
  return trVars(
    "pkg.decline.part_way",
    "It stopped after {served} of {total}.",
    { served: formatBytes(served), total: formatBytes(total) },
  );
}

/** The details appended under "The PS5 declined the install". */
export function declineDetails(st: Pick<InstallStatus, "code" | "metrics">): string {
  const code = st.code || st.metrics?.sony_rc || 0;
  return [sonyCodeLine(code), progressLine(st.metrics?.served_bytes ?? 0, st.metrics?.total_bytes ?? 0)]
    .filter(Boolean)
    .join("\n");
}
