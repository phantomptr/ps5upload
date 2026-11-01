import { describe, expect, it } from "vitest";
import { isConsoleBackupImage, partLabel } from "../../api/saveShare";
import { saveImagesOf } from "./saveImages";

describe("a save's images", () => {
  const files = ["sdimg_STARBOARD", "sdimg_sce_sdmemory", "sdimg_sce_bu_sce_sdmemory", "sdimg_sce_bu_STARBOARD", "STARBOARD.bin"].map(
    (name) => ({ name, kind: "file" }),
  );

  it("never offers the console's own backup copies or the keys", () => {
    expect(saveImagesOf({ path: "/s/PPSA01650", kind: "ps5" }, [...files, { name: "sce_sys", kind: "dir" }])).toEqual([
      "/s/PPSA01650/sdimg_STARBOARD",
      "/s/PPSA01650/sdimg_sce_sdmemory",
    ]);
    expect(isConsoleBackupImage("sdimg_sce_bu_STARBOARD")).toBe(true);
    expect(isConsoleBackupImage("sdimg_STARBOARD")).toBe(false);
  });

  it("a PS4 save's images are its sdimg_ files", () => {
    expect(saveImagesOf({ path: "/s/CUSA1/", kind: "ps4" }, [{ name: "sdimg_A", kind: "file" }, { name: "A.bin", kind: "file" }, { name: "x", kind: "file" }])).toEqual([
      "/s/CUSA1/sdimg_A",
    ]);
  });

  it("names parts for people", () => {
    expect(partLabel("sdimg_STARBOARD", "main save")).toBe("STARBOARD");
    expect(partLabel("sdimg_sce_sdmemory", "main save")).toBe("main save");
  });
});
