import { describe, expect, it } from "vitest";
import { releasePageUrl } from "./UpdateToast";

describe("releasePageUrl", () => {
  it("links the release's own GitHub page", () => {
    expect(releasePageUrl("10.0.4")).toBe("https://github.com/phantomptr/ps5upload/releases/tag/v10.0.4");
    expect(releasePageUrl("v10.0.4")).toBe("https://github.com/phantomptr/ps5upload/releases/tag/v10.0.4");
  });
});
