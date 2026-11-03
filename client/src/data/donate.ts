// Where people can send crypto to support PS5Upload (Help-free: the Thanks & Support page).
// Public receiving addresses from the owner's hardware wallet, each verified on the device and
// checked here by its checksum (bech32, EIP-55, base58 with checksum; Solana's is a plain key).

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

export const CRYPTO: readonly CryptoAddress[] = [
  {
    id: "btc",
    coin: "BTC",
    name: "Bitcoin",
    network: "Bitcoin",
    address: "bc1q36gxkvd3wfxy9azm8hft3rru9m6m9hjvn7u20n",
    color: "#f7931a",
  },
  {
    id: "eth",
    coin: "ETH",
    name: "Ethereum",
    network: "Ethereum",
    address: "0x2e787654db543B69FB011E16995C274dFAfc2286",
    color: "#627eea",
  },
  {
    id: "sol",
    coin: "SOL",
    name: "Solana",
    network: "Solana",
    address: "G1q5XCCn3mePYpcx7uY17CwDsUaAZTcty56huUDvVqjG",
    color: "#9945ff",
  },
  {
    id: "xrp",
    coin: "XRP",
    name: "XRP",
    network: "XRP Ledger",
    address: "rEVq3si3sm9NuWNuZiUZFKsUtwC3w9SrjR",
    color: "#2a9df4",
  },
];
