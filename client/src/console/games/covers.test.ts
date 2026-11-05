import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, callBlob: vi.fn() };
});

type Covers = typeof import("./covers");

/** A stand-in Image: setting src decodes "well" or fails, as the test says. */
function stubImage(decodes: () => boolean) {
  class FakeImage {
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    set src(_: string) {
      queueMicrotask(() => (decodes() ? this.onload?.() : this.onerror?.()));
    }
  }
  vi.stubGlobal("Image", FakeImage);
}

let covers: Covers;
let callBlob: ReturnType<typeof vi.fn>;
let made: string[];
let revoked: string[];

beforeEach(async () => {
  vi.resetModules();
  covers = await import("./covers");
  callBlob = vi.mocked((await import("../api")).callBlob) as unknown as ReturnType<typeof vi.fn>;
  callBlob.mockReset();
  callBlob.mockResolvedValue(new Blob(["x"], { type: "image/png" }));
  made = [];
  revoked = [];
  vi.spyOn(URL, "createObjectURL").mockImplementation(() => {
    const u = `blob:cover-${made.length}`;
    made.push(u);
    return u;
  });
  vi.spyOn(URL, "revokeObjectURL").mockImplementation((u: string) => {
    revoked.push(u);
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("cover shrinking gives its object URLs back", () => {
  it("bytes that are no picture: no cover, the URL revoked, not asked for again", async () => {
    stubImage(() => false);
    expect(await covers.loadImage("PPSA00001", ["/a/icon0.png"])).toBeNull();
    expect(revoked).toEqual(made);
    expect(made).toHaveLength(1);
    expect(await covers.loadImage("PPSA00001", ["/a/icon0.png"])).toBeNull();
    expect(callBlob).toHaveBeenCalledTimes(1);
  });

  it("a drawn cover is a data URL and its object URL is revoked", async () => {
    stubImage(() => true);
    const canvas = { width: 0, height: 0, getContext: () => ({ drawImage: vi.fn() }), toDataURL: () => "data:image/jpeg;base64,AA" };
    vi.stubGlobal("document", { createElement: () => canvas });
    expect(await covers.loadImage("PPSA00002", ["/a/icon0.png"])).toBe("data:image/jpeg;base64,AA");
    expect(revoked).toEqual(made);
  });

  it("a picture that cannot be drawn stays its object URL until the cache lets it go", async () => {
    stubImage(() => true);
    vi.stubGlobal("document", { createElement: () => ({ getContext: () => null }) });
    const first = await covers.loadImage("KEY0", ["/a/icon0.png"]);
    expect(first).toBe("blob:cover-0");
    expect(revoked).toEqual([]);
    // Filling the cache evicts the oldest entry: the URL is given back, not leaked.
    for (let i = 1; i <= 600; i++) await covers.loadImage(`KEY${i}`, ["/a/icon0.png"]);
    expect(revoked).toEqual(["blob:cover-0"]);
  });
});
