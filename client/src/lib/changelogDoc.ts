/**
 * CHANGELOG.md for the What's new screen. The file is large (hundreds of versions) and few read
 * past the first screens, so only the newest versions go into the page until more are asked for.
 */

/** How many versions What's new shows before "Show older". */
export const RECENT_VERSIONS = 20;

/** The start of the file through its `limit` newest `## ` sections. `more`: anything was left out. */
export function recentChangelog(md: string, limit: number = RECENT_VERSIONS): { text: string; more: boolean } {
  const lines = md.split("\n");
  let sections = 0;
  let fenced = false;
  for (let i = 0; i < lines.length; i++) {
    if (/^\s*```/.test(lines[i])) fenced = !fenced;
    if (!fenced && lines[i].startsWith("## ") && ++sections > limit) {
      // Without the `---` that closes the last section shown.
      return { text: lines.slice(0, i).join("\n").replace(/\n-{3,}\s*$/, "").trimEnd(), more: true };
    }
  }
  return { text: md, more: false };
}
