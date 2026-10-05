import { describe, expect, it } from "vitest";

import { dropTarget, usePackageViewer } from "./packageViewer";

describe("what a drop opens", () => {
  it("opens the viewer for a package, image or folder dropped on a screen without its own drop", () => {
    expect(dropTarget(["/dl/game.pkg"], "/")).toEqual({ path: "/dl/game.pkg", kind: "package" });
    expect(dropTarget(["/dl/notes.txt", "/games/G.exfat"], "/installed")).toEqual({ path: "/games/G.exfat", kind: "game" });
    expect(dropTarget(["/games/My Game"], "/")).toEqual({ path: "/games/My Game", kind: "game" });
  });

  it("leaves drops to the screens that take them, and ignores other files", () => {
    for (const screen of ["/install-package", "/payloads", "/upload", "/convert", "/files"]) {
      expect(dropTarget(["/dl/game.pkg"], screen)).toBeNull();
    }
    expect(dropTarget(["/dl/notes.txt", "/dl/payload.elf"], "/")).toBeNull();
    expect(dropTarget([], "/")).toBeNull();
  });
});

describe("the app-wide viewer", () => {
  it("opens on a path with its actions and closes", () => {
    const act = { label: "Install", onClick: () => {} };
    usePackageViewer.getState().open("/dl/a.pkg", [act]);
    expect(usePackageViewer.getState().request).toEqual({ path: "/dl/a.pkg", actions: [act] });
    usePackageViewer.getState().close();
    expect(usePackageViewer.getState().request).toBeNull();
  });
});
