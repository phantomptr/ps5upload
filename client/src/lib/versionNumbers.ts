/** Version strings compared by their numbers, so `01.000.016` equals `1.0.16` and `01.05` sorts
 *  below `01.10`. Non-numeric parts compare as 0. (lib/semver is for the app's own versions.) */
export function versionParts(v: string | null | undefined): number[] {
  return (v ?? "")
    .trim()
    .split(".")
    .filter((p) => p.length > 0)
    .map((p) => {
      const n = parseInt(p, 10);
      return Number.isFinite(n) ? n : 0;
    });
}

export function compareVersions(a: string, b: string): number {
  const x = versionParts(a);
  const y = versionParts(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

export function sameVersion(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return false;
  if (versionParts(a).length === 0 || versionParts(b).length === 0) return false;
  return compareVersions(a, b) === 0;
}
