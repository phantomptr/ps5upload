import { beforeEach, describe, expect, it, vi } from "vitest";

vi.stubGlobal("window", { location: { origin: "http://127.0.0.1:19113" } });

const SHA_A = "a".repeat(64);

function respond(body: unknown, status = 200) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(JSON.stringify(body), { status })),
  );
}

const manifest = {
  schema: 4,
  libraries: [
    {
      name: "libSceAgc.sprx",
      builds: [
        {
          sha256: SHA_A,
          size: 10,
          code_id: "c".repeat(64),
          sdk: [1, 2],
          shipped_by: ["set-1"],
          path: "builds/libSceAgc/aaaaaaaa.sprx",
        },
      ],
    },
  ],
  sets: [
    {
      id: "set-1",
      label: "Red Dead Redemption",
      origin: { kind: "scan", title_id: "PPSA30528", console: "PS5-Pro" },
      libraries: { "libSceAgc.sprx": SHA_A },
    },
  ],
};

describe("fakelib corpus", () => {
  beforeEach(() => vi.restoreAllMocks());

  it("reads the engine's corpus into installable sets", async () => {
    respond({ manifest, summary: { sets: 1, builds: 1, bytes: 10 }, root: "/home/u/.ps5upload/fakelibs" });
    const { loadFakelibCorpus } = await import("./fakelibCorpus");
    const corpus = await loadFakelibCorpus();
    expect(corpus.summary).toEqual({ sets: 1, builds: 1, bytes: 10 });
    expect(corpus.root).toBe("/home/u/.ps5upload/fakelibs");
    expect(corpus.sets).toEqual([
      {
        id: "set-1",
        label: "Red Dead Redemption",
        origin: { kind: "scan", title_id: "PPSA30528", console: "PS5-Pro", at: undefined },
        libraries: [
          {
            name: "libSceAgc.sprx",
            size: 10,
            sha256: SHA_A,
            path: "builds/libSceAgc/aaaaaaaa.sprx",
            shippedBy: 1,
          },
        ],
      },
    ]);
  });

  it("treats an absent corpus as empty, not as a failure", async () => {
    // The ordinary state for a new user: we cannot ship Sony libraries, so
    // everyone starts with nothing and the UI must offer the two ways to fill
    // it rather than report an error they cannot act on.
    respond({ manifest: { schema: 4, libraries: [], sets: [] }, summary: { sets: 0, builds: 0, bytes: 0 }, root: "/x" });
    const { loadFakelibCorpus } = await import("./fakelibCorpus");
    const corpus = await loadFakelibCorpus();
    expect(corpus.sets).toEqual([]);
    expect(corpus.error).toBeNull();
  });

  it("surfaces the engine's message when the corpus cannot be read", async () => {
    respond({ error: "no home directory" }, 500);
    const { loadFakelibCorpus } = await import("./fakelibCorpus");
    expect((await loadFakelibCorpus()).error).toBe("no home directory");
  });

  it("never throws when the engine is unreachable", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new Error("connection refused"); }));
    const { loadFakelibCorpus } = await import("./fakelibCorpus");
    const corpus = await loadFakelibCorpus();
    expect(corpus.sets).toEqual([]);
    expect(corpus.error).toBe("connection refused");
  });

  it("rejects build paths that could escape the corpus directory", async () => {
    // `path` is concatenated into a filesystem path the engine copies from,
    // and a manifest is a file on disk that may be hand-edited.
    respond({
      manifest: {
        ...manifest,
        libraries: [{
          name: "libSceAgc.sprx",
          builds: [{ sha256: SHA_A, size: 10, code_id: "c".repeat(64), shipped_by: [], path: "../../etc/passwd" }],
        }],
      },
      summary: { sets: 1, builds: 1, bytes: 10 },
      root: "/x",
    });
    const { loadFakelibCorpus } = await import("./fakelibCorpus");
    expect((await loadFakelibCorpus()).sets).toEqual([]);
  });

  it("reports a duplicate import as success with nothing added", async () => {
    // Re-importing the same pack is what makes "add libraries any time" safe;
    // it must not read as a failure.
    respond({ ok: true, set_id: null, duplicate: true, ignored: ["._x.sprx"] });
    const { importFakelibSet } = await import("./fakelibCorpus");
    const outcome = await importFakelibSet("pack", "pack.zip", []);
    expect(outcome).toEqual({ setId: null, duplicate: true, ignored: ["._x.sprx"] });
  });

  it("raises the engine's reason when an import is refused", async () => {
    respond({ error: "no library files (.sprx/.prx) in this import" }, 422);
    const { importFakelibSet } = await import("./fakelibCorpus");
    await expect(importFakelibSet("junk", "", [])).rejects.toThrow(/no library files/);
  });
});
