/**
 * Firmware version <-> the BCD word a title records.
 *
 * Distinct from ps5Firmware.ts, which reads the running console's version
 * out of its kernel build string. This one converts between "9.60" and
 * the 0x09600000 that param.json and the SDK offset table use — the two
 * were easy to confuse when both were named for "firmware".
 *
 * Versions are stored BCD-style — the decimal digits become hex digits,
 * so 9.60 is `0x09600000`, not `0x09060000`. The console agrees: FW 9.60
 * reports `system_sw_raw: "0x09600004"`, and the payload SDK's offset
 * table switches on `0x09600000`, `0x12700000`, `0x13600000`.
 *
 * This exists so the SDK Version Changer can offer real firmware
 * versions instead of asking the user to hand-encode hex.
 */

/** Firmware versions the payload SDK has kernel offsets for, newest first. */
export const PS5_FIRMWARES = [
  "13.60",
  "13.40",
  "13.20",
  "13.00",
  "12.70",
  "12.60",
  "12.40",
  "12.20",
  "12.00",
  "11.60",
  "11.40",
  "11.20",
  "11.00",
  "10.60",
  "10.40",
  "10.20",
  "10.00",
  "9.60",
  "9.40",
  "9.20",
  "9.05",
  "9.00",
  "8.60",
  "8.40",
  "8.20",
  "8.00",
  "7.61",
  "7.60",
  "7.40",
  "7.20",
  "7.01",
  "7.00",
  "6.50",
  "6.02",
  "6.00",
  "5.50",
  "5.10",
  "5.02",
  "5.00",
  "4.51",
  "4.50",
  "4.03",
  "4.02",
  "4.00",
  "3.21",
  "3.20",
  "3.10",
  "3.00",
  "2.70",
  "2.50",
  "2.30",
  "2.26",
  "2.25",
  "2.20",
  "2.00",
  "1.14",
  "1.13",
  "1.12",
  "1.11",
  "1.10",
  "1.05",
  "1.02",
  "1.01",
  "1.00",
] as const;

/** "9.60" -> "0x09600000". Returns null if the input isn't a version. */
export function fwToSdkHex(version: string): string | null {
  const m = /^(\d{1,2})\.(\d{1,2})$/.exec(version.trim());
  if (!m) return null;
  const major = m[1].padStart(2, "0");
  const minor = m[2].padEnd(2, "0");
  return `0x${major}${minor}0000`;
}

/**
 * "0x09600000" -> "9.60". Also accepts the padded form param.json stores
 * (`0x0960000000000000`) and the console's raw value, whose low bits
 * carry a build number (`0x09600004`).
 */
export function sdkHexToFw(hex: string): string | null {
  const m = /^0x([0-9a-fA-F]{4})/.exec(hex.trim());
  if (!m) return null;
  const major = parseInt(m[1].slice(0, 2), 10);
  const minor = m[1].slice(2, 4);
  if (Number.isNaN(major) || !/^\d{2}$/.test(minor)) return null;
  return `${major}.${minor}`;
}
