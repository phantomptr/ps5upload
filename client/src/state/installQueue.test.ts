import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useInstallQueue } from "./installQueue";

// installQueue uses bare `localStorage.{getItem,setItem}` (not the
// `window.localStorage` shape the mountDest tests stub), so we have
// to stub the global the module reads from. Vitest's default node env
// has no localStorage at all; jsdom-style globals would also work but
// pulling in jsdom for one suite isn't worth it.
function installLocalStorageStub() {
  const store = new Map<string, string>();
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
  vi.stubGlobal("localStorage", stub);
  return stub;
}

const STORAGE_KEY = "ps5upload.install_queue.v1";

describe("installQueue load() back-compat", () => {
  beforeEach(() => {
    installLocalStorageStub();
    // Zustand stores hold global state across tests in the same
    // module — reset before each test so a leaked items[] from one
    // case doesn't poison the next.
    useInstallQueue.setState({ items: [], runId: 0, isRunning: false });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("hydrates a fresh install (no persisted state) to an empty queue", () => {
    useInstallQueue.getState().hydrate();
    expect(useInstallQueue.getState().items).toEqual([]);
  });

  it("strips the legacy localPs5Path field on load (file:// flow retired in 2.2.52)", () => {
    // Persisted shape from a 2.2.50 / 2.2.51 install, before file://
    // was retired. The field should silently disappear on hydrate
    // and the row should still load with all other fields intact.
    const legacyRow = {
      id: "abc123",
      pkgPath: "/Users/test/foo.pkg",
      isSplit: false,
      displayName: "foo",
      contentId: "UP1234-CUSA56789_00-XXXXXXXXXXXXXXXX",
      totalBytes: 1234567,
      packageType: "PS4GD",
      addr: "192.168.1.50:9114",
      status: "pending",
      phase: "idle",
      bytesDownloaded: 0,
      errCode: 0,
      errMessage: null,
      sessionId: null,
      taskId: null,
      addedAt: 1700000000000,
      startedAt: null,
      finishedAt: null,
      warnings: [],
      // The retired field — set to a non-null value to prove it
      // disappears rather than being passed through silently.
      localPs5Path: "/data/pkg/foo.pkg",
    };
    localStorage.setItem(STORAGE_KEY, JSON.stringify([legacyRow]));

    useInstallQueue.getState().hydrate();
    const loaded = useInstallQueue.getState().items;

    expect(loaded.length).toBe(1);
    const row = loaded[0];
    expect(row.id).toBe("abc123");
    expect(row.pkgPath).toBe("/Users/test/foo.pkg");
    expect(row.contentId).toBe("UP1234-CUSA56789_00-XXXXXXXXXXXXXXXX");
    // The field is gone — accessing it via an as-cast shouldn't find
    // it on the runtime object.
    expect((row as unknown as { localPs5Path?: string }).localPs5Path).toBeUndefined();
  });

  it("back-fills 2.2.52 diag defaults for rows persisted by older builds", () => {
    // A legitimately-old row (no diag field) should hydrate with
    // empty/false defaults so the InstallPackage row's <details>
    // expander renders without a runtime crash on `diag.registerPath`.
    const oldRow = {
      id: "old1",
      pkgPath: "/x.pkg",
      isSplit: false,
      displayName: "x",
      contentId: "UP0000-CUSA00000_00-XXXXXXXXXXXXXXXX",
      totalBytes: 0,
      packageType: "PS4GD",
      addr: "10.0.0.1:9114",
      status: "pending",
      phase: "idle",
      bytesDownloaded: 0,
      errCode: 0,
      errMessage: null,
      sessionId: null,
      taskId: null,
      addedAt: 1,
      startedAt: null,
      finishedAt: null,
      warnings: [],
    };
    localStorage.setItem(STORAGE_KEY, JSON.stringify([oldRow]));

    useInstallQueue.getState().hydrate();
    const row = useInstallQueue.getState().items[0];
    expect(row.diag).toEqual({
      registerPath: "",
      intdebugAvail: false,
      kernelRw: false,
      shelluiErr: null,
      appinstErr: null,
    });
  });

  it("replaces a malformed diag block with the empty default", () => {
    // Corruption-resistant: persisted localStorage could contain a
    // diag of the wrong shape (string, number, array) from an older
    // bug or hand-edited state. Pre-fix the `?? defaults` check let
    // these slip through, then InstallRow's <details> expander tried
    // to access `.registerPath` on a string and rendered weirdly.
    // Validation must replace any non-plain-object diag with the
    // empty default.
    const malformedShapes: unknown[] = ["oops", 42, ["a", "b"], null];
    for (const badDiag of malformedShapes) {
      const row = {
        id: "bad-diag",
        pkgPath: "/z.pkg",
        isSplit: false,
        displayName: "z",
        contentId: "X",
        totalBytes: 0,
        packageType: "PS4GD",
        addr: "1:9114",
        status: "pending",
        phase: "idle",
        bytesDownloaded: 0,
        errCode: 0,
        errMessage: null,
        sessionId: null,
        taskId: null,
        addedAt: 1,
        startedAt: null,
        finishedAt: null,
        warnings: [],
        diag: badDiag,
      };
      localStorage.setItem(STORAGE_KEY, JSON.stringify([row]));
      useInstallQueue.setState({ items: [], runId: 0, isRunning: false });
      useInstallQueue.getState().hydrate();
      const got = useInstallQueue.getState().items[0];
      expect(got.diag).toEqual({
        registerPath: "",
        intdebugAvail: false,
        kernelRw: false,
        shelluiErr: null,
        appinstErr: null,
      });
    }
  });

  it("preserves an existing diag block when hydrating", () => {
    // A row written by a 2.2.52+ payload run carries diag data; the
    // back-fill must not stomp it.
    const richRow = {
      id: "new1",
      pkgPath: "/y.pkg",
      isSplit: false,
      displayName: "y",
      contentId: "UP0000-CUSA00001_00-XXXXXXXXXXXXXXXX",
      totalBytes: 0,
      packageType: "PS4GD",
      addr: "10.0.0.1:9114",
      status: "failed",
      phase: "error",
      bytesDownloaded: 0,
      errCode: 0x80990038,
      errMessage: "BGFT register failed",
      sessionId: null,
      taskId: null,
      addedAt: 2,
      startedAt: null,
      finishedAt: 3,
      warnings: [],
      diag: {
        registerPath: "regular",
        intdebugAvail: false,
        kernelRw: true,
      },
    };
    localStorage.setItem(STORAGE_KEY, JSON.stringify([richRow]));

    useInstallQueue.getState().hydrate();
    const row = useInstallQueue.getState().items[0];
    expect(row.diag.registerPath).toBe("regular");
    expect(row.diag.intdebugAvail).toBe(false);
    expect(row.diag.kernelRw).toBe(true);
  });

  it("drops rows missing required string fields", () => {
    const malformed = [
      { id: 42, pkgPath: "/a.pkg", addr: "x:9114" }, // id wrong type
      { pkgPath: "/b.pkg", addr: "x:9114" }, // missing id
      // good one
      {
        id: "ok",
        pkgPath: "/c.pkg",
        isSplit: false,
        displayName: "c",
        contentId: "X",
        totalBytes: 0,
        packageType: "PS4GD",
        addr: "x:9114",
        status: "pending",
        phase: "idle",
        bytesDownloaded: 0,
        errCode: 0,
        errMessage: null,
        sessionId: null,
        taskId: null,
        addedAt: 1,
        startedAt: null,
        finishedAt: null,
        warnings: [],
      },
    ];
    localStorage.setItem(STORAGE_KEY, JSON.stringify(malformed));

    useInstallQueue.getState().hydrate();
    const items = useInstallQueue.getState().items;
    expect(items.length).toBe(1);
    expect(items[0].id).toBe("ok");
  });
});
