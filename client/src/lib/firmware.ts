// Firmware versions as the Convert screen compares them.

/** A packed kernel firmware word (0x05100000…) as "5.10"; null when the payload could not read it. */
export function consoleFirmware(word: number): string | null {
  if (!word || word <= 0) return null;
  const major = (word >>> 24) & 0xff;
  const minor = (word >>> 16) & 0xff;
  return `${Number.parseInt(major.toString(16), 10)}.${minor.toString(16).padStart(2, "0")}`;
}

/** "10.20" or "05.10" as [major, minor], for comparing versions; null if it is not one. */
export function firmwareParts(v: string | null | undefined): [number, number] | null {
  const m = /^\s*(\d{1,2})\.(\d{1,2})\s*$/.exec(v ?? "");
  return m ? [Number(m[1]), Number(m[2].padEnd(2, "0"))] : null;
}
