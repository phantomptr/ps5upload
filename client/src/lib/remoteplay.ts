/**
 * Derive the Chiaki/pxplay numeric console ID from a base64 PSN
 * account_id (the 8-byte raw id encoded as standard base64).
 *
 * The PSN account_id is an 8-byte big-endian integer. Chiaki expects
 * it as a decimal string of the full 64-bit value. We decode the
 * base64, then reconstruct the number from the 8 bytes using
 * BigInt (the value can exceed 2^53, so Number would lose precision).
 *
 * Ported from elf-arsenal's RemotePlayCard (app.js ~line 4111).
 */
export function accountIdToChiakiNumeric(accountId: string): string {
  if (!accountId) return "";
  try {
    const raw = accountId.replace(/[^A-Za-z0-9+/=]/g, "").replace(/=+$/, "");
    let padded = raw;
    while (padded.length % 4 !== 0) padded += "=";
    const bin = atob(padded);
    if (bin.length < 8) return "";
    let result = BigInt(0);
    for (let i = 0; i < 8; i++) {
      result = result * BigInt(256) + BigInt(bin.charCodeAt(i));
    }
    return result.toString();
  } catch {
    return "";
  }
}
