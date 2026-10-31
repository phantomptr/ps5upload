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
