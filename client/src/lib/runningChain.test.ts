import { describe, expect, it } from "vitest";

import { runningChainPayloads } from "./runningChain";

describe("runningChainPayloads", () => {
  it("finds kstuff and ShadowMount+ under the names consoles really report", () => {
    // From a bug report's process list: the user's own autoloader had
    // already loaded both before the wizard ran.
    const found = runningChainPayloads([
      { name: "payload.elf", comm: "payload.elf" },
      { name: "kstuff.elf", comm: "kstuff.elf" },
      { name: "shadowmountplus.elf", comm: "shadowmountplus.elf" },
      { name: "SceShellUI", comm: "SceShellUI" },
    ]);
    expect([...found].sort()).toEqual(["kstuff", "shadowmount"]);
  });

  it("recognises other kstuff builds and ignores unrelated payloads", () => {
    expect(runningChainPayloads([{ name: "Kstuff-NG_v1.00.elf" }]).has("kstuff")).toBe(true);
    expect(runningChainPayloads([{ name: "x", comm: "kstuff-lite" }]).has("kstuff")).toBe(true);
    expect(
      runningChainPayloads([
        { name: "ps5upload.elf" },
        { name: "elfldr.elf" },
        { name: "pldmgr.elf" },
      ]).size,
    ).toBe(0);
  });
});
