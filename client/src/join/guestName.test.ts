import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanName, defaultName, lastName, NAME_KEY, NAME_MAX, rememberName } from "./guestName";

describe("guest name", () => {
  let store: Map<string, string>;
  beforeEach(() => {
    store = new Map();
    const local = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    };
    vi.stubGlobal("window", { localStorage: local });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("cleans a name: trimmed, at most NAME_MAX characters", () => {
    expect(cleanName("  Ben  ")).toBe("Ben");
    expect(cleanName("   ")).toBe("");
    expect(cleanName("x".repeat(NAME_MAX + 10))).toHaveLength(NAME_MAX);
  });

  it("remembers the name joined with, and fills it in next time", () => {
    expect(lastName()).toBe("");
    expect(lastName("Android")).toBe("Android");
    expect(rememberName("  Ben ")).toBe("Ben");
    expect(store.get(NAME_KEY)).toBe("Ben");
    expect(lastName("Android")).toBe("Ben");
  });

  it("never saves an empty name over the last one", () => {
    rememberName("Ben");
    expect(rememberName("   ")).toBe("");
    expect(lastName()).toBe("Ben");
  });

  it("falls back when the saved name is blank or storage throws", () => {
    store.set(NAME_KEY, "  ");
    expect(lastName("iPhone")).toBe("iPhone");
    vi.stubGlobal("window", {
      localStorage: {
        getItem: () => {
          throw new Error("blocked");
        },
      },
    });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    expect(lastName("iPhone")).toBe("iPhone");
    warn.mockRestore();
  });

  it("names a phone by its kind", () => {
    expect(defaultName("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0)")).toBe("iPhone");
    expect(defaultName("Mozilla/5.0 (Linux; Android 15)")).toBe("Android");
    expect(defaultName("Mozilla/5.0 (Macintosh)")).toBe("");
  });
});
