import { describe, expect, it } from "vitest";

import { changelogSection, shouldShowWhatsNew } from "./whatsNew";

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
