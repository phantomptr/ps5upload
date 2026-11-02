import { describe, expect, it } from "vitest";

import {
  changelogSection,
  changelogSince,
  changelogSinceMarkdown,
  shouldShowWhatsNew,
} from "./whatsNew";

const CHANGELOG = `# Changelog

What's new in ps5upload.

---

## Unreleased

- Not out yet.

## 10.2.0

- **Checked updates.** Every download is checked.
- Skip a version.

## 10.1.0

- Older.

---

## 1.0.0
`;

describe("changelogSection", () => {
  it("cuts one version's section", () => {
    expect(changelogSection(CHANGELOG, "10.2.0")).toBe(
      "- **Checked updates.** Every download is checked.\n- Skip a version.",
    );
    expect(changelogSection(CHANGELOG, "v10.1.0")).toBe("- Older.");
  });

  it("falls back to the release's section for a pre-release", () => {
    expect(changelogSection(CHANGELOG, "10.2.0-beta.1")).toContain("Skip a version.");
  });

  it("is empty for a version it does not have", () => {
    expect(changelogSection(CHANGELOG, "9.9.9")).toBe("");
    expect(changelogSection("", "10.2.0")).toBe("");
  });
});

describe("shouldShowWhatsNew", () => {
  it("shows once, on the first launch of a newer version", () => {
    expect(shouldShowWhatsNew("10.1.0", "10.2.0", true)).toBe(true);
    expect(shouldShowWhatsNew("10.2.0", "10.2.0", true)).toBe(false);
  });

  it("not after a downgrade, nor for nonsense", () => {
    expect(shouldShowWhatsNew("10.2.0", "10.1.0", true)).toBe(false);
    expect(shouldShowWhatsNew("garbage", "10.2.0", true)).toBe(false);
    expect(shouldShowWhatsNew("10.1.0", "", true)).toBe(false);
  });

  it("an update from before this was remembered shows; a first install does not", () => {
    expect(shouldShowWhatsNew(null, "10.2.0", true)).toBe(true);
    expect(shouldShowWhatsNew(null, "10.2.0", false)).toBe(false);
  });
});

describe("changelogSince", () => {
  const LOG = `# Changelog

## Unreleased

- Soon.

## 10.7.3

- Three.

## 10.7.2

- Two.

## 10.7.1

- One.

## 10.7.0

## 10.6.0

- Six.
`;

  it("shows every release since the last one seen, newest first", () => {
    const since = changelogSince(LOG, "10.7.1", "10.7.3");
    expect(since.sections.map((s) => s.version)).toEqual(["10.7.3", "10.7.2"]);
    expect(since.truncated).toBe(false);
    expect(changelogSinceMarkdown(since)).toBe("## 10.7.3\n\n- Three.\n\n## 10.7.2\n\n- Two.");
  });

  it("leaves out releases with no notes and the unreleased section", () => {
    const since = changelogSince(LOG, "10.5.0", "10.7.3");
    expect(since.sections.map((s) => s.version)).toEqual(["10.7.3", "10.7.2", "10.7.1", "10.6.0"]);
  });

  it("only the current release without an older last-seen version", () => {
    for (const lastSeen of [null, "10.7.3", "10.9.0", "garbage"]) {
      expect(changelogSince(LOG, lastSeen, "10.7.3").sections.map((s) => s.version)).toEqual([
        "10.7.3",
      ]);
    }
  });

  it("a pre-release uses its release's section and stops below it", () => {
    expect(changelogSince(LOG, "10.7.1", "10.7.3-beta.1").sections.map((s) => s.version)).toEqual([
      "10.7.3",
      "10.7.2",
    ]);
    // Seen as a pre-release: that release's notes were already shown.
    expect(changelogSince(LOG, "10.7.2-beta.1", "10.7.3").sections.map((s) => s.version)).toEqual([
      "10.7.3",
    ]);
  });

  it("a release missing from the changelog still shows the ones in between", () => {
    expect(changelogSince(LOG, "10.7.1", "10.7.4").sections.map((s) => s.version)).toEqual([
      "10.7.3",
      "10.7.2",
    ]);
  });

  it("caps the number of releases and says more were left out", () => {
    const many = Array.from({ length: 12 }, (_, i) => `## 1.0.${12 - i}\n\n- n${12 - i}.\n`).join("\n");
    const since = changelogSince(many, "1.0.0", "1.0.12", 8);
    expect(since.sections).toHaveLength(8);
    expect(since.sections[0].version).toBe("1.0.12");
    expect(since.sections[7].version).toBe("1.0.5");
    expect(since.truncated).toBe(true);
  });
});
