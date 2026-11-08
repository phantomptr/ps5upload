import { decimalToHex } from "../../lib/remoteplay";
// Account ids as text: the console's 64-bit id in the forms people and tools use.

/* An account id is 64 bits, so BigInt is the obvious tool and the wrong one:
 * the Vite build targets safari13 for old Android WebViews, and BigInt
 * literals are ES2020 that CANNOT be down-levelled — rolldown emits them
 * as-is with a TOLERATED_TRANSFORM warning, and on a WebView without BigInt
 * that is a parse-time SyntaxError for the whole chunk, taking out the entire
 * bundle rather than just this screen. (Same trap as lib/wakeState.ts; there
 * is now a lint rule so it cannot be walked into a third time.)
 *
 * These are string-to-string conversions, which are exact at any width.
 */

export { decimalToHex };

/** Validate what the user typed and return it canonically as "0x…".
 *
 *  Accepts an optional 0x prefix and any case; rejects anything that is not
 *  1-16 hex digits, and rejects zero (zero means "no account", which is what
 *  clearing a slot is for). Returns a string rather than a number so a full
 *  64-bit id survives — see the note above. */
export function parseAccountId(raw: string): string | null {
  const t = raw.trim().replace(/^0x/i, "").toLowerCase();
  if (!/^[0-9a-f]{1,16}$/.test(t)) return null;
  const significant = t.replace(/^0+/, "");
  if (significant === "") return null;
  return "0x" + significant;
}

/** Normalise whatever the API sent into bare lower-case hex digits, or null.
 *
 *  The payload sends the id as 0x-prefixed hex ("0x%016llx" — see
 *  runtime.c), which is also how offact and every other tool talk about it.
 *  Decimal is accepted too: older builds sent it that way, and favourites
 *  and saved values outlive the build that wrote them.
 *
 *  Never goes through Number or BigInt — an account id is 64-bit, so the
 *  first would round it and the second does not exist on the WebViews we
 *  target. Hex is handled as text and decimal by long division. */
export function accountIdHexDigits(id: string | null | undefined): string | null {
  if (!id) return null;
  const t = id.trim().toLowerCase();
  if (/^0x[0-9a-f]{1,16}$/.test(t)) return t.slice(2);
  if (/^\d+$/.test(t)) return decimalToHex(t);
  return null;
}

/** Render an id for display: 0x-prefixed lower-case hex, no padding.
 *
 *  "—" when the slot has no id, which is a real state and not an error. */
export function formatAccountId(id: string | null | undefined): string {
  const hex = accountIdHexDigits(id);
  if (hex === null) return "—";
  const significant = hex.replace(/^0+/, "");
  return significant === "" ? "—" : "0x" + significant;
}

/** Convert an account id to the base64 form Remote Play pairing wants.
 *
 *  Both are renderings of the same 8 registry bytes. offact reads those
 *  bytes straight into a `uint64_t` (offact.c:63) — on the console's x86-64
 *  that IS the little-endian reading — and the base64 is taken over the
 *  bytes in memory order, so the hex digits are emitted low byte first.
 *  Verified against both consoles: 0x1122334455667788 -> "iHdmVUQzIhE=".
 *
 *  Bytes only, never a number: pairing rejects anything that does not
 *  decode to exactly 8 bytes, and a rounded id would decode to 8 perfectly
 *  valid bytes of the WRONG account. Returns "" when there is no id. */
export function accountIdToB64(id: string | null | undefined): string {
  const hex = accountIdHexDigits(id);
  if (hex === null || hex.replace(/^0+/, "") === "") return "";
  const padded = hex.padStart(16, "0");
  let bin = "";
  for (let i = 14; i >= 0; i -= 2) {
    bin += String.fromCharCode(parseInt(padded.slice(i, i + 2), 16));
  }
  try {
    return btoa(bin);
  } catch {
    return "";
  }
}

/** An account id as decimal (how some tools and sites show it), or "" when there is none.
 *  Schoolbook: each hex digit multiplies the decimal digits so far by 16 and adds itself. */
export function accountIdToDecimal(id: string | null | undefined): string {
  const hex = accountIdHexDigits(id);
  if (hex === null || hex.replace(/^0+/, "") === "") return "";
  let dec = [0]; // least significant digit first
  for (const c of hex) {
    let carry = parseInt(c, 16);
    dec = dec.map((d) => {
      const v = d * 16 + carry;
      carry = Math.floor(v / 10);
      return v % 10;
    });
    while (carry > 0) {
      dec.push(carry % 10);
      carry = Math.floor(carry / 10);
    }
  }
  return dec.reverse().join("").replace(/^0+(?=\d)/, "");
}
