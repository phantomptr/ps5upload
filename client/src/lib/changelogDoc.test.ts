import { describe, expect, it } from "vitest";

import changelog from "virtual:doc/changelog";

import { recentChangelog, RECENT_VERSIONS } from "./changelogDoc";

describe("recentChangelog", () => {
  const md = "# Changelog\n\nIntro\n\n## 3\n- c\n\n---\n\n## 2\n- b\n\n## 1\n- a\n";

  it("cuts after the newest versions and says there is more", () => {
    const { text, more } = recentChangelog(md, 1);
    expect(more).toBe(true);
    expect(text).toContain("## 3");
    expect(text).not.toContain("## 2");
    expect(text.trimEnd().endsWith("---")).toBe(false);
  });

  it("returns the whole file when it is short enough", () => {
    expect(recentChangelog(md, 3)).toEqual({ text: md, more: false });
  });

  it("is not fooled by a heading inside a code block", () => {
    const fenced = "## 2\n```\n## not a version\n```\n## 1\n- a\n";
    expect(recentChangelog(fenced, 1).text).toContain("## not a version");
    expect(recentChangelog(fenced, 1).text).not.toContain("## 1");
  });

  it("the real file: a page of versions, far smaller than all of it", () => {
    const { text, more } = recentChangelog(changelog);
    expect(more).toBe(true);
    expect(text.match(/^## /gm)).toHaveLength(RECENT_VERSIONS);
    expect(text.length).toBeLessThan(changelog.length / 2);
  });
});
