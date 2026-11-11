import { describe, expect, it } from "vitest";

import { extractLeftSomething, extractOpensDest, stillAsking } from "./askGuard";

describe("stillAsking: a late answer and the dialog that asked", () => {
  it("is true while the same dialog is open", () => {
    const eject = { kind: "eject", path: "/mnt/usb0" };
    expect(stillAsking(eject, eject)).toBe(true);
  });

  it("is false once that dialog was closed", () => {
    expect(stillAsking(null, { kind: "eject" })).toBe(false);
  });

  it("is false for another dialog, even one that looks the same", () => {
    // Eject was dismissed and Delete opened: the eject's answer must not close Delete.
    expect(stillAsking({ kind: "delete", names: ["a"] }, { kind: "eject" })).toBe(false);
    expect(stillAsking({ kind: "eject", path: "/mnt/usb0" }, { kind: "eject", path: "/mnt/usb0" })).toBe(false);
  });

  it("is false when nothing was open at the time of asking", () => {
    expect(stillAsking(null, null)).toBe(false);
  });
});

describe("extractOpensDest: a finished extract opening its folder", () => {
  it("opens another folder when the user stayed where the extract began", () => {
    expect(extractOpensDest("/mnt/usb0/out", "/data/x", "/data/x", false)).toBe(true);
  });

  it("stays when it extracted next to the archive", () => {
    expect(extractOpensDest("/data/x", "/data/x", "/data/x", false)).toBe(false);
  });

  it("stays when the user browsed elsewhere meanwhile", () => {
    expect(extractOpensDest("/mnt/usb0/out", "/data/x", "/data/other", false)).toBe(false);
    expect(extractOpensDest("/mnt/usb0/out", "/data/x", null, false)).toBe(false);
  });

  it("stays while a dialog is open (it would act on the folder opened under it)", () => {
    expect(extractOpensDest("/mnt/usb0/out", "/data/x", "/data/x", true)).toBe(false);
  });
});

describe("extractLeftSomething: delete the archive after extracting", () => {
  it("is false only when every entry came out", () => {
    expect(extractLeftSomething({ skipped: 0, unsafe: 0, links: 0 })).toBe(false);
  });

  it("keeps the archive when files already there were skipped", () => {
    expect(extractLeftSomething({ skipped: 2, unsafe: 0, links: 0 })).toBe(true);
  });

  it("keeps the archive for unsafe paths and links", () => {
    expect(extractLeftSomething({ skipped: 0, unsafe: 1, links: 0 })).toBe(true);
    expect(extractLeftSomething({ skipped: 0, unsafe: 0, links: 1 })).toBe(true);
  });
});
