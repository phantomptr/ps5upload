// Where people can send crypto to support PS5Upload (Help-free: the Thanks & Support page).
// Public receiving addresses from the owner's hardware wallet, each verified on the device and
// checked here by its checksum (bech32, EIP-55, base58 with checksum; Solana's is a plain key).
// One source for every surface (13.0.0): support.json, which the engine also serves at
// GET /api/support for PS5Upload Play and Minion; scripts/check-support-data.mjs keeps README and
// the website the same.

import support from "./support.json";

export interface CryptoAddress {
  id: "btc" | "eth" | "sol" | "xrp";
  /** Ticker shown on the tab. */
  coin: string;
  /** The coin's name and the one network to send it on. */
  name: string;
  network: string;
  address: string;
  /** The coin's brand colour (the tab, the QR frame). */
  color: string;
}

export const CRYPTO: readonly CryptoAddress[] = support.crypto as CryptoAddress[];

/** Ko-fi, and the website's Thanks section (more ways, the name wall). */
export const KOFI_URL: string = support.kofi;
export const THANKS_PAGE_URL: string = support.thanks_page;
