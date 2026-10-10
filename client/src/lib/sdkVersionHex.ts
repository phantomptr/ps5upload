/**
 * The BCD word a title records -> its firmware version.
 *
 * Distinct from ps5Firmware.ts, which reads the running console's version
 * out of its kernel build string. This one reads the 0x09600000 that
 * param.json and the SDK offset table use as "9.60" — the two
 * were easy to confuse when both were named for "firmware".
 *
 * Versions are stored BCD-style — the decimal digits become hex digits,
 * so 9.60 is `0x09600000`, not `0x09060000`. The console agrees: FW 9.60
 * reports `system_sw_raw: "0x09600004"`, and the payload SDK's offset
 * table switches on `0x09600000`, `0x12700000`, `0x13600000`.
 *
 * Backport uses it to name the firmware a title's SDK version requires.
 */

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
