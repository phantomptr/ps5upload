import { describe, expect, it } from "vitest";
import { iconTitleId } from "./titleId";

describe("the title id a folder name's icon belongs to", () => {
  it("takes the leading title id of a suffixed folder, and nothing from a non-title name", () => {
    expect(iconTitleId("PPSA17221")).toBe("PPSA17221");
    expect(iconTitleId("PPSA17221.bak")).toBe("PPSA17221");
    expect(iconTitleId("CUSA07842_00")).toBe("CUSA07842");
    expect(iconTitleId("ppsa17221-old")).toBe("PPSA17221");
    expect(iconTitleId("PPSA172210")).toBeNull();
    expect(iconTitleId("sce_sdmemory")).toBeNull();
    expect(iconTitleId("")).toBeNull();
  });
});
