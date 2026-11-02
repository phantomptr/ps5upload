// "What's new" once, on the first launch of a new version: its section of the bundled
// CHANGELOG.md, the same text the release page and the update banner's Details show.

import { compareVersions } from "./semver";

/** The last version whose launch was seen (localStorage). */
export const LAST_SEEN_VERSION_KEY = "ps5upload.update.last-seen-version";

/** Keys only someone who has used the app before has: saved consoles, the address, an
 *  update notice. A first install has none, and gets no "what's new" for its first launch. */
export const HISTORY_KEYS = [
  "ps5upload.roster.v1",
  "ps5upload.host",
  "ps5upload.update.notified-version",
];

/** The body of `## <version>` in a changelog: up to the next `## ` heading or a `---` rule,
 *  as the release workflow cuts it (publish.yml, "Extract release notes"). A pre-release
 *  (`10.2.0-beta.1`) falls back to its release's section. Empty when there is none. */
export function changelogSection(changelog: string, version: string): string {
  const v = version.replace(/^v/, "");
  const find = (want: string): string => {
    const out: string[] = [];
    let found = false;
    for (const line of changelog.split(/\r?\n/)) {
      if (!found) {
        if (line.trim() === `## ${want}`) found = true;
        continue;
      }
      if (line.startsWith("## ") || line.trim() === "---") break;
      out.push(line);
    }
    return out.join("\n").trim();
  };
  const exact = find(v);
  if (exact) return exact;
  const core = v.split(/[-+]/)[0];
  return core !== v ? find(core) : "";
}

/** Whether to show `current`'s notes, given the last version seen. Never for the version
 *  already seen, an older one (a downgrade), an unparsable one, or a first install. */
export function shouldShowWhatsNew(
  lastSeen: string | null,
  current: string,
  usedBefore: boolean,
): boolean {
  if (!current) return false;
  if (!lastSeen) return usedBefore;
  if (lastSeen === current) return false;
  return compareVersions(current, lastSeen) === 1;
}

/** How many releases "what's new" shows at most after skipping several; the rest are a link. */
export const WHATS_NEW_MAX_SECTIONS = 8;

/** The whole changelog on GitHub, for more than `WHATS_NEW_MAX_SECTIONS` releases. */
export const CHANGELOG_URL =
  "https://github.com/phantomptr/ps5upload/blob/main/CHANGELOG.md";

export interface ChangelogSince {
  /** Newest first: each release's version (as its heading says) and its notes. */
  sections: { version: string; notes: string }[];
  /** More releases than `max` came in between; the oldest were left out. */
  truncated: boolean;
}

/** What changed since `lastSeen`, up to `current`: every release section newer than the last
 *  version seen, newest first, so updating 10.7.1 → 10.7.3 also shows 10.7.2. `current` is
 *  always first (a pre-release falls back to its release's section, as `changelogSection`).
 *  With no usable `lastSeen` (none, unparsable, or not older than `current`), only `current`.
 *  Releases without a section, and sections with nothing in them, are left out. */
export function changelogSince(
  changelog: string,
  lastSeen: string | null,
  current: string,
  max = WHATS_NEW_MAX_SECTIONS,
): ChangelogSince {
  const cur = current.replace(/^v/, "");
  const core = (v: string) => v.split(/[-+]/)[0];
  const sections: { version: string; notes: string }[] = [];
  const own = changelogSection(changelog, cur);
  // The heading `current`'s notes came from: its own, or its release's for a pre-release.
  const ownHeading = hasHeading(changelog, cur) ? cur : core(cur);
  if (own) sections.push({ version: ownHeading, notes: own });
  const seen = lastSeen ? core(lastSeen.replace(/^v/, "")) : null;
  if (!seen || compareVersions(seen, cur) !== -1)
    return { sections, truncated: false };
  const between: string[] = [];
  for (const line of changelog.split(/\r?\n/)) {
    const m = /^## (\S+)\s*$/.exec(line);
    if (!m || m[1] === ownHeading || between.includes(m[1])) continue;
    // Newer than the last version seen, older than this one.
    if (
      compareVersions(m[1], seen) === 1 &&
      compareVersions(m[1], ownHeading) === -1
    ) {
      between.push(m[1]);
    }
  }
  between.sort((a, b) => compareVersions(b, a) ?? 0);
  let truncated = false;
  for (const v of between) {
    const notes = changelogSection(changelog, v);
    if (!notes) continue;
    if (sections.length >= max) {
      truncated = true;
      break;
    }
    sections.push({ version: v, notes });
  }
  return { sections, truncated };
}

function hasHeading(changelog: string, version: string): boolean {
  return changelog.split(/\r?\n/).some((l) => l.trim() === `## ${version}`);
}

/** The sections as one Markdown text, each under its own `## x.y.z` heading. */
export function changelogSinceMarkdown(since: ChangelogSince): string {
  return since.sections
    .map((s) => `## ${s.version}\n\n${s.notes}`)
    .join("\n\n");
}
