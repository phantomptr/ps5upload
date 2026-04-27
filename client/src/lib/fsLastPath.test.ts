import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  FS_DEFAULT_PATH,
  loadFsLastPath,
  saveFsLastPath,
} from "./fsLastPath";

const KEY = "ps5upload.fs.lastPath";

// vitest's default node env doesn't ship with `window`. The fsLastPath
// module guards against that explicitly, but to exercise the real
// load/save paths we install a minimal in-memory localStorage stub
// before each test. Cheaper than pulling in jsdom + an
// `// @vitest-environment` directive for one helper.
function installLocalStorageStub() {
  const store = new globalThis.Map<string, string>();
  const stub = {
    getItem: (k: string) => (store.has(k) ? (store.get(k) as string) : null),
    setItem: (k: string, v: string) => {
      store.set(k, v);
    },
    removeItem: (k: string) => {
      store.delete(k);
    },
    clear: () => store.clear(),
    key: (i: number) => Array.from(store.keys())[i] ?? null,
    get length() {
      return store.size;
    },
  };
  vi.stubGlobal("window", { localStorage: stub });
  return stub;
}

describe("fsLastPath", () => {
  beforeEach(() => {
    installLocalStorageStub();
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
    const stub = installLocalStorageStub();
    stub.setItem(KEY, "not-json");
    expect(loadFsLastPath("192.168.1.50")).toBe(FS_DEFAULT_PATH);
  });

  it("returns the default when localStorage holds an array (wrong shape)", () => {
    const stub = installLocalStorageStub();
    stub.setItem(KEY, JSON.stringify(["/mnt/ext1"]));
    expect(loadFsLastPath("192.168.1.50")).toBe(FS_DEFAULT_PATH);
  });

  it("filters non-string entries from a partially-corrupted map", () => {
    const stub = installLocalStorageStub();
    stub.setItem(
      KEY,
      JSON.stringify({ "192.168.1.50": "/mnt/ext1", bogus: 42, also: null }),
    );
    expect(loadFsLastPath("192.168.1.50")).toBe("/mnt/ext1");
    expect(loadFsLastPath("bogus")).toBe(FS_DEFAULT_PATH);
  });
});
