import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  fetchPsnGameInfo,
  psnStoreSearchUrl,
  readPsnCache,
  writePsnCache,
} from "./psnDetails";

// Reasoned-through stub for the Tauri `invoke` channel. The PSN code
// path goes: psnDetails.ts -> invoke("psn_fetch", ...) -> Rust. The
// Rust side returns the response body as a string; our stub mimics
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

describe("psn cache", () => {
  beforeEach(() => {
    installLocalStorageStub();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns undefined for a missing entry so caller knows to fetch", () => {
    expect(readPsnCache("PPSA00001")).toBeUndefined();
  });

  it("round-trips a positive cache hit", () => {
    writePsnCache("PPSA00002", { title: "Cool Game" });
    expect(readPsnCache("PPSA00002")).toEqual({ title: "Cool Game" });
  });

  it("remembers a 'PSN had nothing' miss as null so we don't re-fetch", () => {
    writePsnCache("PPSA00003", null);
    expect(readPsnCache("PPSA00003")).toBeNull();
  });
});

describe("psnStoreSearchUrl", () => {
  it("URL-encodes the query so spaces and apostrophes survive the redirect", () => {
    const url = psnStoreSearchUrl("Demon's Souls");
    expect(url).toBe(
      "https://store.playstation.com/en-gb/search/Demon's%20Souls",
    );
  });
});

describe("fetchPsnGameInfo", () => {
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
      return "{}";
    });
    expect(await fetchPsnGameInfo("not-an-id")).toBeNull();
    expect(await fetchPsnGameInfo("PPSA1")).toBeNull();
    expect(calls).toBe(0);
  });

  it("returns the parsed valkyrie hit and caches it", async () => {
    const body = {
      data: {},
      included: [
        {
          id: "PPSA00099_00",
          type: "game",
          attributes: {
            name: "Test Title",
            "long-description": "Long\n\ndescription with whitespace.",
            "thumbnail-url-base": "https://image.api.playstation.com/foo",
            genres: ["Action"],
            "publisher-name": "Acme",
            "primary-classification": "Game",
            "content-rating": {
              url: "https://example.com",
              description: "ESRB MATURE 17+",
            },
          },
        },
      ],
    };
    installInvokeStub(async () => JSON.stringify(body));
    const info = await fetchPsnGameInfo("PPSA00099");
    expect(info?.title).toBe("Test Title");
    expect(info?.description).toBe("Long description with whitespace.");
    expect(info?.coverImageUrl).toContain("image.api.playstation.com");
    expect(info?.publisher).toBe("Acme");
    // Second call should hit the cache, not re-fetch.
    let secondCalls = 0;
    installInvokeStub(async () => {
      secondCalls += 1;
      return "{}";
    });
    const cached = await fetchPsnGameInfo("PPSA00099");
    expect(cached?.title).toBe("Test Title");
    expect(secondCalls).toBe(0);
  });

  it("falls back to titlecontainer when valkyrie returns nothing usable", async () => {
    const containerBody = {
      name: "Old Game",
      long_desc: "Vintage classic.",
      images: [{ url: "https://psn.example.com/cover.jpg", type: 1 }],
      publisher_name: "RetroCo",
    };
    const urlsSeen: string[] = [];
    installInvokeStub(async (_cmd, args) => {
      const url = (args as { url: string }).url;
      urlsSeen.push(url);
      if (url.includes("/valkyrie-api/")) {
        // valkyrie returns empty-included
        return JSON.stringify({ included: [] });
      }
      return JSON.stringify(containerBody);
    });
    const info = await fetchPsnGameInfo("PPSA12345");
    expect(info?.title).toBe("Old Game");
    expect(info?.coverImageUrl).toBe("https://psn.example.com/cover.jpg");
    expect(urlsSeen.some((u) => u.includes("titlecontainer"))).toBe(true);
  });

  it("caches a definitive 'no data' result so we don't re-fetch on every modal open", async () => {
    let calls = 0;
    installInvokeStub(async () => {
      calls += 1;
      throw new Error("psn http 404");
    });
    const info = await fetchPsnGameInfo("PPSA99999");
    expect(info).toBeNull();
    const earlierCalls = calls;
    expect(await fetchPsnGameInfo("PPSA99999")).toBeNull();
    expect(calls).toBe(earlierCalls);
  });
});
