import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  resumeForget,
  resumeLookup,
  resumeRemember,
} from "./resumeTxidBrowser";

const k = { host: "10.0.0.2", src: "/s/a.pkg", dest: "/data/a.pkg" };
const DAY = 24 * 60 * 60 * 1000;

describe("the browser's resume store", () => {
  beforeEach(() => {
    const store = new Map<string, string>();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (key: string) => store.get(key) ?? null,
        setItem: (key: string, v: string) => void store.set(key, v),
        removeItem: (key: string) => void store.delete(key),
      },
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("gives back the job id a file ran under, for the same file and place only", () => {
    expect(resumeLookup(k).tx_id_hex).toBeNull();
    resumeRemember({ ...k, tx_id_hex: "abc" });
    expect(resumeLookup(k).tx_id_hex).toBe("abc");
    expect(resumeLookup({ ...k, dest: "/data/b.pkg" }).tx_id_hex).toBeNull();
    resumeForget(k);
    expect(resumeLookup(k).tx_id_hex).toBeNull();
  });

  it("lets go of a record after a week, as the desktop app does", () => {
    resumeRemember({ ...k, tx_id_hex: "abc" }, 1000);
    expect(resumeLookup(k, 1000 + 6 * DAY).tx_id_hex).toBe("abc");
    expect(resumeLookup(k, 1000 + 8 * DAY).tx_id_hex).toBeNull();
  });
});
