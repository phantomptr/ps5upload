import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  fetchTitleInfo,
  metaSourceForTitleId,
  parsePatchesHtml,
  patchesSiteName,
  patchesSiteUrl,
  platformForTitleId,
  readTitleCache,
  titleIdFromContentId,
  writeTitleCache,
} from "./titleDetails";

// Stub the Tauri `invoke` channel. The title-meta code path goes:
// titleDetails.ts -> invoke("title_meta_fetch", {url}) -> Rust. The
// Rust side returns the response body as a string; the stub mimics
// that contract by holding a function the individual tests install.
let invokeImpl: (cmd: string, args?: unknown) => Promise<unknown> = async () => {
  throw new Error("invoke stub not installed");
};
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeImpl(cmd, args),
}));
// invokeLogged branches on isTauriEnv() to route to browserInvoke instead of
// the mocked Tauri invoke above; force the Tauri path so this mock is used.
vi.mock("./tauriEnv", () => ({ isTauriEnv: () => true }));

function installInvokeStub(impl: typeof invokeImpl) {
  invokeImpl = impl;
}

function installLocalStorageStub() {
  const store = new globalThis.Map<string, string>();
  vi.stubGlobal("window", {
    localStorage: {
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
    },
  });
  return store;
}

/** Skeleton of what the patches sites return for a known title —
 *  only the two pieces we actually parse. PS5 (prosperopatches) has
 *  a bare `<title>`, PS4 (orbispatches) appends a " | sitename"
 *  suffix; the parser handles both. */
function makeProsperoHtml(titleId: string, displayName: string): string {
  return `<!doctype html><html><head>
    <title>${titleId}: ${displayName}</title>
    <meta name="twitter:image" content="https://cdn.prosperopatches.com/titles/${titleId}_abc/icon0.webp">
  </head><body></body></html>`;
}
function makeOrbisHtml(titleId: string, displayName: string): string {
  return `<!doctype html><html><head>
    <title>${titleId}: ${displayName} | ORBISPatches.com</title>
    <meta name="twitter:image" content="https://cdn.orbispatches.com/titles/${titleId}_xyz/icon0.webp">
  </head><body></body></html>`;
}

describe("metaSourceForTitleId", () => {
  it("routes PPSA##### to prosperopatches (PS5)", () => {
    const src = metaSourceForTitleId("PPSA01285");
    expect(src?.url).toBe("https://prosperopatches.com/PPSA01285");
    expect(src?.siteName).toBe("PROSPEROPatches");
  });

  it("routes CUSA##### to orbispatches (PS4)", () => {
    const src = metaSourceForTitleId("CUSA57609");
    expect(src?.url).toBe("https://orbispatches.com/CUSA57609");
    expect(src?.siteName).toBe("ORBISPatches");
  });

  it("returns null for prefixes that don't run on PS5 (PCSA/NPXS/etc.)", () => {
    expect(metaSourceForTitleId("PCSE00001")).toBeNull();
    expect(metaSourceForTitleId("NPXS40000")).toBeNull();
  });
});

describe("titleIdFromContentId", () => {
  it("extracts the title id from a full ContentID", () => {
    expect(titleIdFromContentId("EP4040-PPSA01342_00-DEADSPACEPS5BB00")).toBe(
      "PPSA01342",
    );
    expect(titleIdFromContentId("UP0006-CUSA57609_00-CARDEALERSIM0000")).toBe(
      "CUSA57609",
    );
  });
  it("returns null for headerless / unrecognizable ids", () => {
    expect(titleIdFromContentId("")).toBeNull();
    expect(titleIdFromContentId(null)).toBeNull();
    expect(titleIdFromContentId(undefined)).toBeNull();
    expect(titleIdFromContentId("EP4040-XX_00")).toBeNull();
  });
});

describe("platformForTitleId", () => {
  it("maps CUSA → ps4 and PPSA/PCSA → ps5", () => {
    expect(platformForTitleId("CUSA12345")).toBe("ps4");
    expect(platformForTitleId("PPSA01342")).toBe("ps5");
    expect(platformForTitleId("PCSA00001")).toBe("ps5");
  });
  it("returns null for unknown prefixes / empty", () => {
    expect(platformForTitleId("NPXS40000")).toBeNull();
    expect(platformForTitleId(null)).toBeNull();
  });
});

describe("title cache", () => {
  beforeEach(() => {
    installLocalStorageStub();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns undefined for a missing entry so caller knows to fetch", () => {
    expect(readTitleCache("PPSA00001")).toBeUndefined();
  });

  it("round-trips a positive cache hit", () => {
    writeTitleCache("PPSA00002", { title: "Cool Game" });
    expect(readTitleCache("PPSA00002")).toEqual({ title: "Cool Game" });
  });

  it("remembers a 'no data' miss as null so we don't re-fetch", () => {
    writeTitleCache("PPSA00003", null);
    expect(readTitleCache("PPSA00003")).toBeNull();
  });
});

describe("patchesSiteUrl / patchesSiteName", () => {
  it("returns the canonical PS5 url + site name for PPSA ids", () => {
    expect(patchesSiteUrl("PPSA01285")).toBe(
      "https://prosperopatches.com/PPSA01285",
    );
    expect(patchesSiteName("PPSA01285")).toBe("PROSPEROPatches");
  });

  it("returns the canonical PS4 url + site name for CUSA ids", () => {
    expect(patchesSiteUrl("CUSA57609")).toBe(
      "https://orbispatches.com/CUSA57609",
    );
    expect(patchesSiteName("CUSA57609")).toBe("ORBISPatches");
  });

  it("returns null when there's no upstream we'd query", () => {
    expect(patchesSiteUrl("PCSE12345")).toBeNull();
    expect(patchesSiteName("PCSE12345")).toBeNull();
  });
});

describe("parsePatchesHtml", () => {
  const PROSPERO_RE = /^https:\/\/cdn\.prosperopatches\.com\//;
  const ORBIS_RE = /^https:\/\/cdn\.orbispatches\.com\//;

  it("extracts title and cover image from a PS5 (prosperopatches) page", () => {
    const html = makeProsperoHtml("PPSA01285", "Returnal");
    const info = parsePatchesHtml(html, PROSPERO_RE);
    expect(info?.title).toBe("Returnal");
    expect(info?.coverImageUrl).toBe(
      "https://cdn.prosperopatches.com/titles/PPSA01285_abc/icon0.webp",
    );
  });

  it("strips the ' | ORBISPatches.com' suffix from PS4 page titles", () => {
    const html = makeOrbisHtml("CUSA57609", "Car Dealer Simulator");
    const info = parsePatchesHtml(html, ORBIS_RE);
    expect(info?.title).toBe("Car Dealer Simulator");
    expect(info?.coverImageUrl).toContain("cdn.orbispatches.com");
  });

  it("rejects cover URLs that don't match the platform's CDN", () => {
    // A prosperopatches page that somehow points at the orbis CDN
    // would be tampering — drop the cover URL, keep the title.
    const html = `<html><head>
      <title>PPSA00001: Wrong CDN</title>
      <meta name="twitter:image" content="https://cdn.orbispatches.com/sneaky.webp">
    </head></html>`;
    const info = parsePatchesHtml(html, PROSPERO_RE);
    expect(info?.title).toBe("Wrong CDN");
    expect(info?.coverImageUrl).toBeUndefined();
  });

  it("ignores cover-image meta values from totally unrelated hosts", () => {
    const html = `<html><head>
      <title>PPSA00099: Spoofy</title>
      <meta name="twitter:image" content="https://evil.example.com/sneaky.webp">
    </head></html>`;
    const info = parsePatchesHtml(html, PROSPERO_RE);
    expect(info?.title).toBe("Spoofy");
    expect(info?.coverImageUrl).toBeUndefined();
  });

  it("returns null when neither title nor cover are present", () => {
    expect(
      parsePatchesHtml("<html><head></head></html>", PROSPERO_RE),
    ).toBeNull();
  });

  it("falls back to og:image when twitter:image is absent", () => {
    const html = `<html><head>
      <title>PPSA00100: Fallback</title>
      <meta property="og:image" content="https://cdn.prosperopatches.com/titles/PPSA00100_x/icon0.webp">
    </head></html>`;
    const info = parsePatchesHtml(html, PROSPERO_RE);
    expect(info?.coverImageUrl).toContain("PPSA00100");
  });
});

describe("fetchTitleInfo", () => {
  beforeEach(() => {
    installLocalStorageStub();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    invokeImpl = async () => {
      throw new Error("invoke stub not installed");
    };
  });

  it("rejects malformed title ids without hitting the network", async () => {
    let calls = 0;
    installInvokeStub(async () => {
      calls += 1;
      return "";
    });
    expect(await fetchTitleInfo("not-an-id")).toBeNull();
    expect(await fetchTitleInfo("PPSA1")).toBeNull();
    expect(calls).toBe(0);
  });

  it("skips the network for prefixes we don't resolve (PCSE/NPXS/etc.)", async () => {
    let calls = 0;
    installInvokeStub(async () => {
      calls += 1;
      return "";
    });
    expect(await fetchTitleInfo("PCSE00123")).toBeNull();
    expect(calls).toBe(0);
  });

  it("hits prosperopatches for a PS5 title and caches the parsed result", async () => {
    let seenUrl = "";
    installInvokeStub(async (_cmd, args) => {
      seenUrl = (args as { url: string }).url;
      return makeProsperoHtml("PPSA00099", "Test PS5 Title");
    });
    const info = await fetchTitleInfo("PPSA00099");
    expect(info?.title).toBe("Test PS5 Title");
    expect(info?.coverImageUrl).toContain("cdn.prosperopatches.com");
    expect(seenUrl).toBe("https://prosperopatches.com/PPSA00099");
  });

  it("hits orbispatches for a PS4 title and strips the site suffix from the title", async () => {
    let seenUrl = "";
    installInvokeStub(async (_cmd, args) => {
      seenUrl = (args as { url: string }).url;
      return makeOrbisHtml("CUSA57609", "Car Dealer Simulator");
    });
    const info = await fetchTitleInfo("CUSA57609");
    expect(info?.title).toBe("Car Dealer Simulator");
    expect(info?.coverImageUrl).toContain("cdn.orbispatches.com");
    expect(seenUrl).toBe("https://orbispatches.com/CUSA57609");
  });

  it("caches a 404 / parse-failure as null so the modal doesn't re-fetch every open", async () => {
    let calls = 0;
    installInvokeStub(async () => {
      calls += 1;
      throw new Error("title-meta http 404");
    });
    expect(await fetchTitleInfo("PPSA99999")).toBeNull();
    const earlierCalls = calls;
    expect(await fetchTitleInfo("PPSA99999")).toBeNull();
    expect(calls).toBe(earlierCalls);
  });
});
