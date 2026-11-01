/**
 * PSN account id conversions for Remote Play clients.
 *
 * One id, three spellings, all of the same 8 registry bytes:
 *   hex     0x7ed3586971779f76     what the console, offact and Profile show
 *   base64  dp93cWlY034=           what Chiaki wants and the payload reports
 *   decimal 9138745278763343734    what pxplay wants
 *
 * The console reads the bytes straight into a uint64_t on a little-endian
 * CPU, so the base64 is over the bytes LOW byte first; the hex and decimal
 * are that uint64. Reading the base64 high byte first gave pxplay a
 * different account's number, and it answered 403.
 *
 * No BigInt (it breaks the bundle on old WebViews — see Accounts/index.tsx)
 * and no Number (it rounds above 2^53): every conversion is on digit strings.
 */

/** Lower-case hex digits (1-16, no prefix) → decimal string. */
function hexToDecimal(hex: string): string {
  let digits = hex.split("").map((c) => parseInt(c, 16));
  const out: number[] = [];
  while (digits.length > 0) {
    let remainder = 0;
    const next: number[] = [];
    for (const d of digits) {
      const cur = remainder * 16 + d;
      const q = Math.floor(cur / 10);
      remainder = cur % 10;
      if (next.length > 0 || q > 0) next.push(q);
    }
    out.push(remainder);
    digits = next;
  }
  return out.length > 0 ? out.reverse().join("") : "0";
}

/** Decimal string → lower-case hex digits, or null past 64 bits. */
function decimalToHex(dec: string): string | null {
  let digits = dec.split("").map((c) => c.charCodeAt(0) - 48);
  const out: string[] = [];
  while (digits.length > 0) {
    let remainder = 0;
    const next: number[] = [];
    for (const d of digits) {
      const cur = remainder * 10 + d;
      const q = Math.floor(cur / 16);
      remainder = cur % 16;
      if (next.length > 0 || q > 0) next.push(q);
    }
    out.push("0123456789abcdef"[remainder]);
    digits = next;
  }
  const hex = out.length > 0 ? out.reverse().join("") : "0";
  return hex.length > 16 ? null : hex;
}

/** Any of the three spellings → 16 hex digits (the uint64), or null.
 *
 *  Which spelling it is, never by length alone: 12 characters ending in "=" is
 *  base64 (8 bytes); "0x" or any of the letters a-f is hex (the app always shows
 *  the hex with 0x); only digits is decimal (pxplay's number), however short —
 *  reading a short decimal as hex gave a different account. */
function accountIdHex(raw: string): string | null {
  const t = raw.trim();
  if (/^[A-Za-z0-9+/]{11}=$/.test(t)) {
    let bin: string;
    try {
      bin = atob(t);
    } catch {
      return null;
    }
    if (bin.length !== 8) return null;
    let hex = "";
    for (let i = 7; i >= 0; i--) hex += bin.charCodeAt(i).toString(16).padStart(2, "0");
    return hex;
  }
  if (/^\d+$/.test(t)) {
    const hex = decimalToHex(t);
    return hex === null ? null : hex.padStart(16, "0");
  }
  const h = t.replace(/^0x/i, "").toLowerCase();
  if (!/^[0-9a-f]{1,16}$/.test(h)) return null;
  return h.padStart(16, "0");
}

/** pxplay's number for an account id in any spelling; "" if unreadable. */
export function accountIdToChiakiNumeric(accountId: string): string {
  if (!accountId) return "";
  const hex = accountIdHex(accountId);
  return hex === null ? "" : hexToDecimal(hex.replace(/^0+(?=.)/, ""));
}

/** The console's base64 for an account id in any spelling, or null when it
 *  is not one (zero is "no account", not an id). This is what the payload
 *  is sent, so the screen shows the same thing whichever spelling was typed. */
export function accountIdToPairingB64(raw: string): string | null {
  const hex = accountIdHex(raw);
  if (hex === null || /^0+$/.test(hex)) return null;
  let bin = "";
  for (let i = 14; i >= 0; i -= 2) bin += String.fromCharCode(parseInt(hex.slice(i, i + 2), 16));
  return btoa(bin);
}

/** The console's own spelling of an account id in any spelling (0x + 16 hex digits), or "" when
 *  it is not one: what the pairing form shows as the id it will use. */
export function accountIdToHex(raw: string): string {
  const hex = accountIdHex(raw);
  return hex === null || /^0+$/.test(hex) ? "" : `0x${hex}`;
}
