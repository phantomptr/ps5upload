/**
 * The largest fake package a PS5 opens, by firmware: the client's copy of the engine's
 * `ps5upload_fpkg::package_limit`, for the Workshop's check line. The engine checks again,
 * exactly, when it builds.
 *
 *  - 5.50 to 6.xx: about 242.55 GiB. Below 5.50 (5.10) there is no size check.
 *  - 7.xx to 10.xx: 256 GiB.
 *  - 11.xx and 12.xx: the game's own appSizeInGib when it is 256 to 320, else 256 GiB.
 *  - 13.x: the package plus the add-on space the game reserves (acSizeInGib) at most 450 GiB.
 */

export const GIB = 2 ** 30;

/** 242.551 GiB, the 5.50 to 6.50 ceiling (the engine's `FW550_CAP`). */
const FW550_CAP = 260_436_918_272;

/** The largest layout index measured on a Sony-built package (48.0 MiB); see the engine's
 *  `LARGEST_MEASURED_INDEX`. */
export const LARGEST_MEASURED_INDEX = 50_380_176;

export interface Firmware {
  major: number;
  minor: number;
}

export type Band = "fw550" | "fw700" | "fw1100" | "fw1300";

export interface PackageLimit {
  band: Band;
  /** The ceiling the firmware checks: 450 GiB on 13.x. */
  cap: number;
  /** The add-on space counted against it (13.x only). */
  reserved: number;
  /** The largest package that fits: cap - reserved. */
  maxPackage: number;
}

/** What the game's param.json reserves, in GiB (the inspection's `param_sizes`). */
export interface ParamSizes {
  ac_gib?: number | null;
  app_gib?: number | null;
}

/** "13.60", "5.1", "05.10" or a BCD word ("0x1360000000000000"). */
export function parseFirmware(text: string | null | undefined): Firmware | null {
  const v = (text ?? "").trim();
  const hex = /^0x([0-9a-f]{16})$/i.exec(v);
  if (hex) {
    const digits = hex[1];
    const major = Number.parseInt(digits.slice(0, 2), 10);
    const minor = Number.parseInt(digits.slice(2, 4), 10);
    return /^\d{4}/.test(digits) && major > 0 ? { major, minor } : null;
  }
  const m = /^(\d{1,2})\.(\d{1,2})$/.exec(v);
  if (!m) return null;
  const major = Number(m[1]);
  const minor = Number(m[2].length === 1 ? `${m[2]}0` : m[2]);
  return major > 0 ? { major, minor } : null;
}

function atLeast(fw: Firmware, major: number, minor: number): boolean {
  return fw.major > major || (fw.major === major && fw.minor >= minor);
}

/** The ceiling for a console on `fw`; null below 5.50, where no size check is known. */
export function limitFor(fw: Firmware, sizes: ParamSizes | null | undefined): PackageLimit | null {
  const at = (band: Band, cap: number, reserved: number): PackageLimit => ({
    band,
    cap,
    reserved,
    maxPackage: Math.max(0, cap - reserved),
  });
  if (!atLeast(fw, 5, 50)) return null;
  if (fw.major < 7) return at("fw550", FW550_CAP, 0);
  if (fw.major < 11) return at("fw700", 256 * GIB, 0);
  if (fw.major < 13) {
    const app = sizes?.app_gib;
    const profile = app != null && app >= 256 && app <= 320 ? app : 256;
    return at("fw1100", profile * GIB, 0);
  }
  return at("fw1300", 450 * GIB, (sizes?.ac_gib ?? 0) * GIB);
}

/** The firmware line in words, as the engine's messages name it. */
export function bandLabel(band: Band): string {
  switch (band) {
    case "fw550":
      return "5.50–6.xx";
    case "fw700":
      return "7.xx–10.xx";
    case "fw1100":
      return "11.xx–12.xx";
    case "fw1300":
      return "13.x";
  }
}

/** What the container adds per 64 KiB block (the engine's `container_allowance`). */
export function withContainer(imageBytes: number): number {
  return imageBytes + 72 * Math.ceil(imageBytes / 65536);
}

export type Basis = "target" | "console" | "game" | "assumed";

export interface PackageFit {
  /** The package's size in bytes. */
  size: number;
  /** From a sample (a compressed level), not exact. */
  estimated: boolean;
  firmware: Firmware;
  basis: Basis;
  /** Null when that firmware checks no size. */
  limit: PackageLimit | null;
  fits: boolean;
  /** No console known, and it is past what 7.xx to 12.xx open (256 GiB). */
  olderMiss: boolean;
  /** The layout index estimate, when the inspection carries what it needs. */
  layoutIndex: number | null;
}

/** Check a package of `size` bytes: against the user's `target` firmware, else the connected
 *  `console`'s, else the game's own when that is 13.00 or later, else 13.x (and say what older
 *  firmware won't open). */
export function packageFit(opts: {
  size: number;
  estimated: boolean;
  target?: string | null;
  console?: string | null;
  required?: string | null;
  sizes?: ParamSizes | null;
  /** The inspection's `layout_index_fixed`: add 8 bytes per 64 KiB of package. */
  layoutIndexFixed?: number | null;
}): PackageFit {
  const fw13: Firmware = { major: 13, minor: 0 };
  const target = parseFirmware(opts.target);
  const consoleFw = parseFirmware(opts.console);
  const required = parseFirmware(opts.required);
  let firmware: Firmware;
  let basis: Basis;
  if (target) [firmware, basis] = [target, "target"];
  else if (consoleFw) [firmware, basis] = [consoleFw, "console"];
  else if (required && atLeast(required, 13, 0)) [firmware, basis] = [required, "game"];
  else [firmware, basis] = [fw13, "assumed"];
  const limit = limitFor(firmware, opts.sizes);
  const fits = !limit || opts.size <= limit.maxPackage;
  return {
    size: opts.size,
    estimated: opts.estimated,
    firmware,
    basis,
    limit,
    fits,
    olderMiss: basis === "assumed" && fits && opts.size > 256 * GIB,
    layoutIndex: opts.layoutIndexFixed ? opts.layoutIndexFixed + 8 * Math.ceil(opts.size / 65536) : null,
  };
}

/** `n` bytes in GiB as the check line shows it: "580", "216.4". */
export function gib(n: number): string {
  const text = (n / GIB).toFixed(1);
  return text.endsWith(".0") ? text.slice(0, -2) : text;
}
