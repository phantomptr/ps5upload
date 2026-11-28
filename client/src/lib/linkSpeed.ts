// A network link's speed in words, for the USB Ethernet adapter (the app and the console page).
// No imports: the console page bundles it too.

function trimNum(n: number): string {
  return String(Math.round(n * 10) / 10);
}

/** Short form for a transfer row: 2500 → "2.5G", 10000 → "10G", 100 → "100M"; null when unknown. */
export function linkShort(mbps: number | null | undefined): string | null {
  const n = Number(mbps);
  if (!Number.isFinite(n) || n <= 0) return null;
  if (n >= 1000) return `${trimNum(n / 1000)}G`;
  return `${trimNum(n)}M`;
}

/** In full: 2500 → "2.5 Gbit/s", 100 → "100 Mbit/s"; null when unknown. */
export function linkLong(mbps: number | null | undefined): string | null {
  const n = Number(mbps);
  if (!Number.isFinite(n) || n <= 0) return null;
  if (n >= 1000) return `${trimNum(n / 1000)} Gbit/s`;
  return `${trimNum(n)} Mbit/s`;
}
