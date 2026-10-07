import { describe, expect, it } from "vitest";

import { smpHandoffNote } from "./smpHandoffNote";

describe("what Mount says for an image ShadowMount+ manages", () => {
  it("says the image mounts when the game starts, so nothing shows as mounted yet", () => {
    const s = smpHandoffNote("G.exfat", { added: true, chosePath: false });
    expect(s).toContain('Handed "G.exfat" to ShadowMount+');
    expect(s).toContain("when the game starts");
    expect(s).toContain("Ready to play");
    // The old text promised a mount under /mnt/shadowmnt that 1.7 no longer does up front.
    expect(s).not.toContain("mounts it under");
  });

  it("says there is nothing to do when the image is already managed", () => {
    const s = smpHandoffNote("G.exfat", { added: false, chosePath: false });
    expect(s).toContain("already managed by ShadowMount+");
    expect(s).toContain("nothing to do");
    expect(s).toContain("when the game starts");
  });

  it("says a chosen mount point was not used, and how to use one", () => {
    const s = smpHandoffNote("G.exfat", { added: true, chosePath: true });
    expect(s).toContain("mount point wasn't used");
    expect(s).toContain("move it out of that folder");
  });
});
