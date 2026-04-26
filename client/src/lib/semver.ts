// Tiny semver compare for the Connection screen's "your payload is
// older than the bundled one" banner.
//
// We only need ordering — a < b, a === b, a > b — between two strings
// like "2.2.0" or "2.2.3-rc1". The npm `semver` package is overkill
// (~25 KB minified, dependency tree of its own); this 30-line helper
// covers the version shapes our own VERSION file produces.
//
// Pre-release tags (anything after a `-`) are compared by the part
// before the dash; ties on numeric triple are broken by string
// compare on the suffix. Good enough for "is the payload behind the
// app" — we ship versions like "2.2.3" or occasionally "2.2.3-rc.1".
//
// Returns:
//   -1 when a is older than b
//    0 when equal (numerically + suffix-string)
//    1 when a is newer than b
//   null when either input doesn't parse — caller should treat as
//        "unknown" and skip the comparison rather than guess.

export function compareVersions(a: string, b: string): -1 | 0 | 1 | null {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  if (!pa || !pb) return null;
  for (let i = 0; i < 3; i++) {
    if (pa.parts[i] < pb.parts[i]) return -1;
    if (pa.parts[i] > pb.parts[i]) return 1;
  }
  // Numeric triple equal — pre-release tag breaks ties. SemVer says
  // a build with a pre-release tag is *less than* one without. We
  // implement that correctly so "2.2.3-rc1" < "2.2.3".
  if (pa.pre && !pb.pre) return -1;
  if (!pa.pre && pb.pre) return 1;
  if (pa.pre === pb.pre) return 0;
  return (pa.pre ?? "") < (pb.pre ?? "") ? -1 : 1;
}

interface ParsedVersion {
  parts: [number, number, number];
  pre: string | null;
}

function parseVersion(v: string): ParsedVersion | null {
  const m = v.trim().match(/^v?(\d+)\.(\d+)\.(\d+)(?:[-+]([A-Za-z0-9._-]+))?$/);
  if (!m) return null;
  return {
    parts: [Number(m[1]), Number(m[2]), Number(m[3])],
    pre: m[4] ?? null,
  };
}
