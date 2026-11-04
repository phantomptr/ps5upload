import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  FS_DEFAULT_PATH,
  loadFsLastPath,
  saveFsLastPath,
} from "./fsLastPath";
import { installWindowStub } from "../test-utils/storage";

const KEY = "ps5upload.fs.lastPath";

describe("fsLastPath", () => {
  beforeEach(() => {
    installWindowStub();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns the default path when no entry exists", () => {
    expect(loadFsLastPath("192.168.1.50")).toBe(FS_DEFAULT_PATH);
  });

  it("returns the default path for empty / whitespace host", () => {
    saveFsLastPath("192.168.1.50", "/mnt/ext1");
    expect(loadFsLastPath("")).toBe(FS_DEFAULT_PATH);
    expect(loadFsLastPath("   ")).toBe(FS_DEFAULT_PATH);
  });

  it("round-trips a saved path for the same host", () => {
    saveFsLastPath("192.168.1.50", "/mnt/ext1");
    expect(loadFsLastPath("192.168.1.50")).toBe("/mnt/ext1");
  });

  it("keeps separate paths per host", () => {
    saveFsLastPath("192.168.1.50", "/mnt/ext1");
    saveFsLastPath("192.168.137.2", "/data/ps5upload");
    expect(loadFsLastPath("192.168.1.50")).toBe("/mnt/ext1");
    expect(loadFsLastPath("192.168.137.2")).toBe("/data/ps5upload");
  });

  it("trims whitespace from the host key so 'IP ' and 'IP' share state", () => {
    saveFsLastPath("192.168.1.50", "/mnt/ext1");
    expect(loadFsLastPath("192.168.1.50  ")).toBe("/mnt/ext1");
    expect(loadFsLastPath("  192.168.1.50")).toBe("/mnt/ext1");
  });

  it("ignores empty paths so a mid-edit setPath('') doesn't wipe the entry", () => {
    saveFsLastPath("192.168.1.50", "/mnt/ext1");
    saveFsLastPath("192.168.1.50", "");
    expect(loadFsLastPath("192.168.1.50")).toBe("/mnt/ext1");
  });

  it("returns the default when localStorage holds garbage", () => {
    const stub = installWindowStub();
    stub.setItem(KEY, "not-json");
    expect(loadFsLastPath("192.168.1.50")).toBe(FS_DEFAULT_PATH);
  });

  it("returns the default when localStorage holds an array (wrong shape)", () => {
    const stub = installWindowStub();
    stub.setItem(KEY, JSON.stringify(["/mnt/ext1"]));
    expect(loadFsLastPath("192.168.1.50")).toBe(FS_DEFAULT_PATH);
  });

  it("filters non-string entries from a partially-corrupted map", () => {
    const stub = installWindowStub();
    stub.setItem(
      KEY,
      JSON.stringify({ "192.168.1.50": "/mnt/ext1", bogus: 42, also: null }),
    );
    expect(loadFsLastPath("192.168.1.50")).toBe("/mnt/ext1");
    expect(loadFsLastPath("bogus")).toBe(FS_DEFAULT_PATH);
  });
});
