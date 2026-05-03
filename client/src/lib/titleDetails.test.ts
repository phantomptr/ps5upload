import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  fetchTitleInfo,
  parseProsperoPatchesHtml,
  prosperoPatchesUrl,
  readTitleCache,
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

/** Skeleton of what prosperopatches returns for a known title — only
 *  the two pieces we actually parse (title tag + twitter:image meta). */
function makeProsperoHtml(titleId: string, displayName: string): string {
  return `<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <title>${titleId}: ${displayName}</title>
  <meta name="twitter:image" content="https://cdn.prosperopatches.com/titles/${titleId}_abc123/icon0.webp">
</head>
<body><h1>${displayName}</h1></body>
</html>`;
}

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

describe("prosperoPatchesUrl", () => {
  it("builds the canonical title page url", () => {
    expect(prosperoPatchesUrl("PPSA01285")).toBe(
      "https://prosperopatches.com/PPSA01285",
    );
  });
  it("URL-encodes pathologically-shaped ids defensively", () => {
    // We validate format earlier, but the URL builder should not
    // produce something the upstream rejects on weird input.
    expect(prosperoPatchesUrl("a/b")).toBe(
      "https://prosperopatches.com/a%2Fb",
    );
  });
});

describe("parseProsperoPatchesHtml", () => {
  it("extracts title and cover image from a known-shape page", () => {
    const html = makeProsperoHtml("PPSA01285", "Returnal");
    const info = parseProsperoPatchesHtml(html);
    expect(info?.title).toBe("Returnal");
    expect(info?.coverImageUrl).toBe(
      "https://cdn.prosperopatches.com/titles/PPSA01285_abc123/icon0.webp",
    );
  });

  it("ignores cover-image meta values that don't point at the cdn", () => {
    const html = `<html><head>
      <title>PPSA00099: Spoofy</title>
      <meta name="twitter:image" content="https://evil.example.com/sneaky.webp">
    </head></html>`;
    const info = parseProsperoPatchesHtml(html);
    expect(info?.title).toBe("Spoofy");
    expect(info?.coverImageUrl).toBeUndefined();
  });

  it("returns null when neither title nor cover are present", () => {
    expect(parseProsperoPatchesHtml("<html><head></head></html>")).toBeNull();
  });

  it("strips the TITLEID prefix in `<title>` even when title id has different shape", () => {
    const html = "<html><head><title>CUSA12345: Some Old Game</title></head></html>";
    const info = parseProsperoPatchesHtml(html);
    expect(info?.title).toBe("Some Old Game");
  });

  it("falls back to og:image when twitter:image is absent", () => {
    const html = `<html><head>
      <title>PPSA00100: Fallback</title>
      <meta property="og:image" content="https://cdn.prosperopatches.com/titles/PPSA00100_x/icon0.webp">
    </head></html>`;
    const info = parseProsperoPatchesHtml(html);
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

  it("returns parsed info for a known title and caches it", async () => {
    const html = makeProsperoHtml("PPSA00099", "Test Title");
    let calls = 0;
    installInvokeStub(async () => {
      calls += 1;
      return html;
    });
    const info = await fetchTitleInfo("PPSA00099");
    expect(info?.title).toBe("Test Title");
    expect(info?.coverImageUrl).toContain("cdn.prosperopatches.com");
    expect(calls).toBe(1);

    // Second call hits the cache, not the network.
    const cached = await fetchTitleInfo("PPSA00099");
    expect(cached?.title).toBe("Test Title");
    expect(calls).toBe(1);
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

  it("hits the canonical prosperopatches url path", async () => {
    let seenUrl = "";
    installInvokeStub(async (_cmd, args) => {
      seenUrl = (args as { url: string }).url;
      return makeProsperoHtml("PPSA12345", "Whatever");
    });
    await fetchTitleInfo("PPSA12345");
    expect(seenUrl).toBe("https://prosperopatches.com/PPSA12345");
  });
});
