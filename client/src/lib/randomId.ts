/**
 * Mint a 128-bit hexadecimal id in every client environment.
 *
 * `crypto.randomUUID()` is a secure-context API: Chromium intentionally hides
 * it when the self-hosted UI is opened over plain HTTP from a LAN address
 * (for example `http://192.168.1.20:19113`). `crypto.getRandomValues()` is
 * still available there, so use it as the primary compatibility fallback.
 * The Math.random branch is only for old test/webview environments that expose
 * neither API; these ids provide uniqueness, not authentication.
 */
export function randomHexId(cryptoImpl: Crypto | undefined = globalThis.crypto): string {
  if (typeof cryptoImpl?.randomUUID === "function") {
    return cryptoImpl.randomUUID().replace(/-/g, "");
  }

  const bytes = new Uint8Array(16);
  if (typeof cryptoImpl?.getRandomValues === "function") {
    cryptoImpl.getRandomValues(bytes);
  } else {
    for (let i = 0; i < bytes.length; i += 1) {
      bytes[i] = Math.floor(Math.random() * 256);
    }
  }
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}
