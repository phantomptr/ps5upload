// The version the website's screenshots show (the sidebar's "v…", and what the made-up console and
// devices report). The pictures are taken before a release bumps the version, so the repo's own
// version is the one before the release they are for: by default they show the next patch
// release. SHOTS_VERSION=12.7.0 shows another (a minor or major release).

/** `version` one patch level up: 12.6.31 → 12.6.32. A pre-release or build suffix is dropped. */
export function nextPatch(version: string): string {
  const m = /^\s*v?(\d+)\.(\d+)\.(\d+)/.exec(version);
  if (!m) throw new Error(`not a version: ${JSON.stringify(version)}`);
  return `${m[1]}.${m[2]}.${Number(m[3]) + 1}`;
}

/** What the pictures show: `fromEnv` (SHOTS_VERSION) when set, else the repo's VERSION file's
 *  version plus one patch level. */
export function shotsVersion(fromEnv: string | undefined, versionFile: string): string {
  const asked = fromEnv?.trim().replace(/^v/, "");
  if (asked) {
    if (!/^\d+\.\d+\.\d+$/.test(asked)) throw new Error(`SHOTS_VERSION is not a version: ${JSON.stringify(fromEnv)}`);
    return asked;
  }
  return nextPatch(versionFile);
}
