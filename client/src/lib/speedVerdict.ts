/**
 * What a speed test's two figures look like, judged by the slower direction (a copy is only
 * as fast as its slow side). Bytes per second in, a verdict the screen words out.
 *
 * The bands are what the link types deliver in practice: gigabit Ethernet to a PS5 runs
 * 90–115 MB/s, a 100 Mbit link (an old cable, a 10/100 switch or port) tops out near
 * 11–12 MB/s, and Wi-Fi or a busy network lands anywhere in between.
 */
export type SpeedVerdict =
  "gigabit" | "fast_wifi_or_busy" | "hundred_mbit" | "slow";

const MB = 1024 * 1024;

export function speedVerdict(
  uploadBps: number | null,
  downloadBps: number | null,
): SpeedVerdict | null {
  if (!uploadBps || !downloadBps) return null;
  const slower = Math.min(uploadBps, downloadBps) / MB;
  if (slower >= 75) return "gigabit";
  if (slower >= 13) return "fast_wifi_or_busy";
  if (slower >= 9) return "hundred_mbit";
  return "slow";
}
