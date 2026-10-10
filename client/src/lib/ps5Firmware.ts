/**
 * The running console's firmware version, read from its kernel string.
 *
 * Distinct from sdkVersionHex.ts, which reads the BCD word a title
 * records in param.json as a version.
 *
 * Extract the user-visible PS5 firmware version ("9.60", "5.00", …) from
 * the kernel build string surfaced via the STATUS_ACK's `ps5_kernel`
 * field. The payload pulls that string from `sysctl kern.version`, and
 * PS5 firmware images embed their release number inside it.
 *
 * Observed shapes:
 *   FreeBSD 11.0-RELEASE-p0 #1 r218215/releases/09.60: Jul 18 2023
 *   FreeBSD 11.0-RELEASE-p0 #0 r218215/releases/10.00-00...
 *
 * We try a few patterns in order of specificity:
 *   1. "releases/XX.YY" — the canonical build-branch tag
 *   2. "r218215/…/XX.YY" — any NN.NN that looks like a version
 *   3. fallback: the first bare NN.NN substring
 *
 * Returns the trimmed version (e.g. "9.60" with leading zero removed),
 * or null if nothing matches. Leading-zero stripping matches how the
 * PS5 surfaces its own firmware on screen ("9.60", not "09.60").
 */
export function parsePS5Firmware(kernel: string | null | undefined): string | null {
  if (!kernel) return null;
  const patterns = [
    /releases\/(\d{1,2})\.(\d{2})/i,
    /\/(\d{1,2})\.(\d{2})(?:-|\s|:)/,
    /\b(\d{1,2})\.(\d{2})\b/,
  ];
  for (const pat of patterns) {
    const m = kernel.match(pat);
    if (m) {
      const major = Number(m[1]);
      const minor = m[2];
      if (!Number.isFinite(major)) continue;
      return `${major}.${minor}`;
    }
  }
  return null;
}

/** The newest firmware on which a PS5 fake (FPKG) GAME is playable. Above it
 *  the package still installs, but the game does not start. Measured by the
 *  maintainer on real hardware. PS4 fake packages and PS5 homebrew apps are
 *  not affected. */
export const LAST_PS5_FAKE_GAME_FIRMWARE = "11.60";

/** True when `id` (a content id such as `UP4433-PPSA17221_00-MINECRAFTPS50000`
 *  or a bare title id) names a PS5 GAME: it carries a `PPSAnnnnn` title id.
 *  PS4 titles (`CUSAnnnnn`) and PS5 homebrew (`IV0002-ITEM00001_00-…`, whose
 *  title id is `ITEMnnnnn` or similar) are not PS5 games. An empty or
 *  unrecognised id is not a game: the warning needs positive evidence. */
export function isPs5GameId(id: string | null | undefined): boolean {
  return !!id && /(?:^|[^A-Z0-9])PPSA\d{5}(?![0-9])/.test(id.toUpperCase());
}

/** The console's firmware ("13.60") when a PS5 fake GAME package with this id
 *  would install but not be playable there, else null. That needs all of:
 *  a PS5 game id (not PS4, not homebrew) AND a parseable firmware strictly
 *  above 11.60. An unparseable kernel string never produces a warning. */
export function ps5FakeGameUnplayableFirmware(
  kernel: string | null | undefined,
  contentOrTitleId: string | null | undefined,
): string | null {
  if (!isPs5GameId(contentOrTitleId)) return null;
  const fw = parsePS5Firmware(kernel);
  if (!fw) return null;
  const toNum = (v: string) => {
    const [maj, min] = v.split(".");
    return Number(maj) * 100 + Number(min);
  };
  return toNum(fw) > toNum(LAST_PS5_FAKE_GAME_FIRMWARE) ? fw : null;
}
