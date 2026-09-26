import { describe, expect, it } from "vitest";

import { isImagePath, payloadCanMountImage, useUploadStore } from "./upload";
import { useConnectionStore } from "./connection";
import type { PickedSource } from "./upload";

const fileSrc = (path: string): PickedSource => ({
  kind: "file",
  path,
  meta: null,
  wrappedHint: null,
  zipInfo: null,
});

describe("isImagePath — all four PS5 disk-image formats", () => {
  it("recognizes exFAT, UFS, PFS, and compressed/nested PFS", () => {
    expect(isImagePath("/x/game.exfat")).toBe(true);
    expect(isImagePath("/x/game.ffpkg")).toBe(true);
    expect(isImagePath("/x/game.ffpfs")).toBe(true);
    expect(isImagePath("/x/game.ffpfsc")).toBe(true);
  });
  it("is case-insensitive", () => {
    expect(isImagePath("/X/GAME.FFPKG")).toBe(true);
    expect(isImagePath("/X/GAME.Ffpfsc")).toBe(true);
  });
  it("rejects non-images", () => {
    expect(isImagePath("/x/game.pkg")).toBe(false);
    expect(isImagePath("/x/game.zip")).toBe(false);
    expect(isImagePath("/x/folder")).toBe(false);
    expect(isImagePath("/x/game.ffp")).toBe(false);
  });
});

describe("payloadCanMountImage — ps5upload's own mount vs SMP-only", () => {
  it("ps5upload can attach exFAT / UFS / PFS directly", () => {
    expect(payloadCanMountImage("/x/g.exfat")).toBe(true);
    expect(payloadCanMountImage("/x/g.ffpkg")).toBe(true);
    expect(payloadCanMountImage("/x/g.ffpfs")).toBe(true);
  });
  it("a .ffpfsc container is NOT directly mountable (ShadowMount+ only)", () => {
    expect(isImagePath("/x/g.ffpfsc")).toBe(true);
    expect(payloadCanMountImage("/x/g.ffpfsc")).toBe(false);
  });
});

describe("switchToHost — per-console upload draft is preserved", () => {
  const up = () => useUploadStore.getState();
  // switchToHost reads the OLD host from the connection store (it runs before
  // connection.setHost), so each switch sets the connection host first to
  // mirror the real roster call order.
  const setActiveHost = (h: string) => useConnectionStore.setState({ host: h });

  it("stashes the leaving console's draft and restores the target's", () => {
    // On console A: pick a file + set a password and destination.
    setActiveHost("192.168.50.10");
    useUploadStore.setState({
      source: fileSrc("/a/game.bin"),
      destinationVolume: "/data",
      rarPassword: "secret",
    });

    // Switch to B (connection still on A — matches roster order).
    up().switchToHost("192.168.50.20");
    expect(up().source).toBeNull();
    expect(up().destinationVolume).toBeNull();
    expect(up().rarPassword).toBeNull();

    // Connection follows to B (what the roster's setHost does next); configure B.
    setActiveHost("192.168.50.20");
    useUploadStore.setState({ source: fileSrc("/b/other.bin") });

    // Back to A → A's draft (file + password + destination) restored intact.
    up().switchToHost("192.168.50.10");
    expect(up().source?.path).toBe("/a/game.bin");
    expect(up().destinationVolume).toBe("/data");
    expect(up().rarPassword).toBe("secret");

    // A → B again → B's own draft restored (not lost).
    setActiveHost("192.168.50.10");
    up().switchToHost("192.168.50.20");
    expect(up().source?.path).toBe("/b/other.bin");
  });

  it("is a no-op when the target is the same console (port-stripped)", () => {
    setActiveHost("10.9.9.9");
    useUploadStore.setState({ source: fileSrc("/keep.bin") });
    up().switchToHost("10.9.9.9:9113"); // same bare host → no swap
    expect(up().source?.path).toBe("/keep.bin");
  });
});
