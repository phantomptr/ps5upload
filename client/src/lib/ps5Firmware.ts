/**
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

/**
 * The firmware MAJOR number (9, 10, 11, 12, …) from the kernel string, or
 * null when it can't be parsed. Used for the FW-11 "authority cliff": at and
 * above FW 11, Sony gates the package content-copy behind the SYSTEM install
 * authid, which the standalone DPI daemon (Stream beta) can't acquire — so a
 * stream install there registers a hollow tile with no content. The reliable
 * path on FW 11+ is the normal upload-then-install (its in-process installer
 * DOES escalate). Callers use `firmwareMajor(kernel) >= 11` to steer users.
 */
export function firmwareMajor(kernel: string | null | undefined): number | null {
  const fw = parsePS5Firmware(kernel);
  if (!fw) return null;
  const major = Number(fw.split(".")[0]);
  return Number.isFinite(major) ? major : null;
}
