import { describe, expect, it } from "vitest";

import {
  imageBasename,
  isRemovableMount,
  removableMountRoot,
  smpMountImageBasename,
} from "./mountPaths";

describe("isRemovableMount", () => {
  it("is true for /mnt/usb* and /mnt/ext*", () => {
    expect(isRemovableMount("/mnt/usb0")).toBe(true);
    expect(isRemovableMount("/mnt/usb0/games/x.pkg")).toBe(true);
    expect(isRemovableMount("/mnt/ext1/foo")).toBe(true);
  });

  it("is false for internal + other mounts", () => {
    expect(isRemovableMount("/data/x")).toBe(false);
    expect(isRemovableMount("/user/app")).toBe(false);
    // ShadowMount disc images are internal mounts, NOT removable drives.
    expect(isRemovableMount("/mnt/shadowmnt/PPSA01289/eboot.bin")).toBe(false);
    expect(isRemovableMount("/mnt/ps5upload/img")).toBe(false);
  });
});

describe("removableMountRoot", () => {
  it("extracts the drive root from a deeper path", () => {
    expect(removableMountRoot("/mnt/usb0/games/x.pkg")).toBe("/mnt/usb0");
    expect(removableMountRoot("/mnt/ext1/a/b/c")).toBe("/mnt/ext1");
    expect(removableMountRoot("/mnt/usb0")).toBe("/mnt/usb0");
  });

  it("returns null for non-removable paths", () => {
    expect(removableMountRoot("/data/x")).toBeNull();
    expect(removableMountRoot("/mnt/shadowmnt/g/eboot.bin")).toBeNull();
  });
});

describe("smpMountImageBasename", () => {
  it("recovers the image basename from an SMP mount point", () => {
    expect(smpMountImageBasename("/mnt/shadowmnt/PPSA09016_ca51a0d7")).toBe(
      "PPSA09016",
    );
    expect(smpMountImageBasename("/mnt/shadowmnt/My Game_a3c3fd8b")).toBe(
      "My Game",
    );
  });

  it("handles the nested backend layout", () => {
    expect(
      smpMountImageBasename("/mnt/shadowmnt/pfsc/PPSA21567_e7a36f16"),
    ).toBe("PPSA21567");
  });

  it("keeps an underscore that belongs to the name", () => {
    expect(smpMountImageBasename("/mnt/shadowmnt/foo_bar_deadbeef")).toBe(
      "foo_bar",
    );
  });

  it("returns null for anything that isn't an SMP mount in that shape", () => {
    // Our own mounts, and folder games, must not be mistaken for SMP mounts —
    // their `source` is already the real path and needs no basename recovery.
    expect(
      smpMountImageBasename("/mnt/ext1/homebrew/PPSA19534-app"),
    ).toBeNull();
    expect(smpMountImageBasename("/mnt/ps5upload/Foo")).toBeNull();
    expect(smpMountImageBasename("/mnt/shadowmnt/nohash")).toBeNull();
    // Uppercase hex is not what SMP emits; refuse rather than guess.
    expect(smpMountImageBasename("/mnt/shadowmnt/Foo_DEADBEEF")).toBeNull();
    expect(smpMountImageBasename("/mnt/shadowmnt/")).toBeNull();
  });
});

describe("imageBasename", () => {
  it("drops one trailing extension", () => {
    expect(imageBasename("/mnt/usb0/homebrew/PPSA09016.exfat")).toBe(
      "PPSA09016",
    );
    expect(imageBasename("/data/homebrew/PPSA01285.ffpkg")).toBe("PPSA01285");
  });

  it("leaves a dotted name's earlier dots alone", () => {
    expect(imageBasename("/data/homebrew/v1.2.exfat")).toBe("v1.2");
  });
});
