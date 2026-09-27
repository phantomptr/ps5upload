import { describe, expect, it } from "vitest";
import { queueRowName } from "./QueuePanel";

describe("queueRowName", () => {
  it("keeps the game's title after install instead of the raw content id", () => {
    // Seen on Android: "Star Wars…" while uploading became "UP1082-…" once the
    // install finished, because the finisher stored the content id.
    expect(
      queueRowName(
        { sourceKind: "pkg", installedTitle: "UP1082-CUSA03474_00-SLUS202680000001", displayName: "StarWars-BASE.pkg" },
        "Star Wars™: Racer Revenge™",
      ),
    ).toBe("Star Wars™: Racer Revenge™");
  });

  it("uses the installed title when no online title is known", () => {
    expect(
      queueRowName({ sourceKind: "pkg", installedTitle: "Jak X", displayName: "x.pkg" }, undefined),
    ).toBe("Jak X");
  });

  it("falls back to the file name, and never renames non-pkg items", () => {
    expect(queueRowName({ sourceKind: "pkg", installedTitle: null, displayName: "x.pkg" }, undefined)).toBe("x.pkg");
    expect(queueRowName({ sourceKind: "file", installedTitle: "T", displayName: "a.bin" }, "Online")).toBe("a.bin");
  });
});
