import { describe, expect, it } from "vitest";

import { imageUploadItem } from "./imageUpload";

describe("a built image as an Upload item", () => {
  it("goes into the chosen folder on the PS5, named after the image", () => {
    const it1 = imageUploadItem(
      "/Users/me/out/Worms.ffpfsc",
      {
        host: "192.168.1.5",
        volume: "/mnt/ext0",
        subpath: "homebrew",
        deleteAfter: true,
      },
      2_400_000_000,
    );
    expect(it1.sourceKind).toBe("image");
    expect(it1.resolvedDest).toBe("/mnt/ext0/homebrew/Worms.ffpfsc");
    expect(it1.addr).toBe("192.168.1.5");
    expect(it1.deleteSourceAfterUpload).toBe(true);
    expect(it1.estimatedBytes).toBe(2_400_000_000);
    expect(it1.mountAfterUpload).toBe(false);
  });

  it("defaults to /data when no drive was picked, and keeps the image unless asked", () => {
    const it1 = imageUploadItem("C:\\out\\Game.exfat", {
      host: "10.0.0.2:9120",
      volume: null,
      subpath: "/homebrew/",
      deleteAfter: false,
    });
    expect(it1.resolvedDest).toBe("/data/homebrew/Game.exfat");
    expect(it1.deleteSourceAfterUpload).toBe(false);
  });
});
