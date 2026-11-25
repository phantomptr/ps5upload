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

/** The appinfo.db keys a title's installed version is read from (installedAppVersion). */
export const APPINFO_VERSION_KEYS = "CONTENT_VERSION,VERSION,APP_VER";

/** A title's installed version from its appinfo.db values (`get` returns a key's trimmed value,
 *  or null/undefined/"" when absent): CONTENT_VERSION (a PS5 title's, `01.000.016`), else the
 *  higher of VERSION and APP_VER (a PS4 title's: an installed update raises VERSION, `01.32`,
 *  while APP_VER stays at the base game's `01.00`, #934). The helper and the engine pick the same
 *  way. Null when none is there. */
export function installedAppVersion(get: (key: string) => string | null | undefined): string | null {
  const content = get("CONTENT_VERSION");
  if (content) return content;
  const version = get("VERSION");
  const app = get("APP_VER");
  if (version && app) return compareVersions(version, app) >= 0 ? version : app;
  return version || app || null;
}

export function sameVersion(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return false;
  if (versionParts(a).length === 0 || versionParts(b).length === 0) return false;
  return compareVersions(a, b) === 0;
}
