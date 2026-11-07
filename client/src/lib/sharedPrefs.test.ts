import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import settingsApiRs from "../../../engine/crates/ps5upload-engine/src/settings_api.rs?raw";

import { asDoc, MESH_KEYS, mergeEntries, plan, SHARED_KEYS, syncOnce, type Doc, type Entries, type PrefsApi } from "./sharedPrefs";

const PIN = "ps5upload.desktop-sidebar.pinned.v1";
const CAP = "ps5upload.bandwidth_cap_mbps";
const THEME = "ps5upload.theme";

/** An engine keeping one document, with the revision check the real one does. */
function fakeEngine(initial: Doc = { rev: 0, value: null }): PrefsApi & { doc: Doc; puts: number } {
  const e = {
    doc: initial,
    puts: 0,
    async get() {
      return e.doc;
    },
    async put(baseRev: number, keys: Entries) {
      e.puts++;
      if (baseRev !== e.doc.rev) return { conflict: e.doc };
      e.doc = { rev: e.doc.rev + 1, value: { keys } };
      return { rev: e.doc.rev };
    },
  };
  return e;
}

let store: Map<string, string>;
beforeEach(() => {
  store = new Map();
  vi.stubGlobal("window", {
    localStorage: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    },
  });
});
afterEach(() => vi.unstubAllGlobals());

describe("the engine's typed settings (settings_api.rs)", () => {
  it("know every shared key", () => {
    const missing = SHARED_KEYS.filter((k) => !settingsApiRs.includes(`"${k}"`));
    expect(missing).toEqual([]);
  });
});

describe("MESH_KEYS", () => {
  it("never shares the Minion switch for new consoles without the list of consoles it handled", () => {
    // Both are this device's: the switch with another device's (or no) list would turn Minion off
    // on consoles this device already used.
    expect(MESH_KEYS).not.toContain("ps5upload.minion_off_for_new");
    expect(MESH_KEYS).not.toContain("ps5upload.minionDefaultDone");
    expect(MESH_KEYS).not.toContain("ps5upload.minionOff");
    // The Docker engine's browsers still share all three, together.
    expect(SHARED_KEYS).toEqual(
      expect.arrayContaining(["ps5upload.minion_off_for_new", "ps5upload.minionDefaultDone", "ps5upload.minionOff"]),
    );
  });
});

describe("asDoc", () => {
  it("refuses an answer that is not a document, and drops malformed entries", () => {
    expect(() => asDoc({})).toThrow();
    expect(() => asDoc({ rev: -1, value: null })).toThrow();
    const d = asDoc({ rev: 3, value: { keys: { [PIN]: { v: '["/files"]', at: 5 }, bad: { v: 7, at: 1 }, worse: null } } });
    expect(d.value?.keys).toEqual({ [PIN]: { v: '["/files"]', at: 5 } });
  });
});

describe("mergeEntries", () => {
  it("keeps the newer change per key, and keys this build does not know", () => {
    expect(
      mergeEntries(
        { [PIN]: { v: "a", at: 1 }, [CAP]: { v: "10", at: 9 }, future: { v: "x", at: 1 } },
        { [PIN]: { v: "b", at: 2 }, [CAP]: { v: "5", at: 3 } },
      ),
    ).toEqual({ [PIN]: { v: "b", at: 2 }, [CAP]: { v: "10", at: 9 }, future: { v: "x", at: 1 } });
  });
});

describe("plan", () => {
  it("the engine wins over settings from before sharing; settings it lacks are offered", () => {
    const p = plan({ [PIN]: { v: "mine", at: 0 }, [CAP]: { v: "20", at: 0 } }, { [PIN]: { v: "theirs", at: 7 } }, 100);
    expect(p.apply).toEqual({ [PIN]: { v: "theirs", at: 7 } });
    expect(p.send).toEqual({ [PIN]: { v: "theirs", at: 7 }, [CAP]: { v: "20", at: 100 } });
  });
  it("a change made here after the engine's wins and is sent", () => {
    const p = plan({ [PIN]: { v: "new", at: 9 } }, { [PIN]: { v: "old", at: 7 } }, 100);
    expect(p.apply).toEqual({});
    expect(p.send).toEqual({ [PIN]: { v: "new", at: 9 } });
  });
  it("nothing to send when the engine has everything", () => {
    expect(plan({ [PIN]: { v: "x", at: 7 } }, { [PIN]: { v: "x", at: 7 } }, 100)).toEqual({ apply: {}, send: null });
  });
});

describe("syncOnce", () => {
  it("the first browser hands its settings over; a second one takes them, never its theme", async () => {
    const engine = fakeEngine();
    store.set(PIN, '["/files"]');
    store.set(THEME, "dark");
    await syncOnce(engine);
    expect(engine.doc.value?.keys?.[PIN]?.v).toBe('["/files"]');
    expect(engine.doc.value?.keys?.[THEME]).toBeUndefined();

    // Another browser: its own older pins, a light theme.
    store = new Map([
      [PIN, '["/games"]'],
      [THEME, "light"],
    ]);
    const written = await syncOnce(engine);
    expect(written).toEqual([PIN]);
    expect(store.get(PIN)).toBe('["/files"]');
    expect(store.get(THEME)).toBe("light");
  });

  it("a removal travels too", async () => {
    const engine = fakeEngine({ rev: 4, value: { keys: { [CAP]: { v: null, at: 50 } } } });
    store.set(CAP, "30");
    store.set("ps5upload.shared_prefs.meta.v1", JSON.stringify({ rev: 3, at: { [CAP]: 10 } }));
    await syncOnce(engine);
    expect(store.has(CAP)).toBe(false);
  });

  it("a save that meets another browser's merges per key and tries again", async () => {
    const engine = fakeEngine({ rev: 1, value: { keys: { [PIN]: { v: "a", at: 5 } } } });
    store.set("ps5upload.shared_prefs.meta.v1", JSON.stringify({ rev: 1, at: { [PIN]: 5, [CAP]: 20 } }));
    store.set(PIN, "a");
    store.set(CAP, "15");
    // Someone else saves in between our read and our write.
    const realGet = engine.get;
    let first = true;
    engine.get = async () => {
      const d = await realGet();
      if (first) {
        first = false;
        engine.doc = { rev: 2, value: { keys: { [PIN]: { v: "b", at: 30 } } } };
      }
      return d;
    };
    await syncOnce(engine);
    expect(engine.doc.value?.keys).toEqual({ [PIN]: { v: "b", at: 30 }, [CAP]: { v: "15", at: 20 } });
    expect(store.get(PIN)).toBe("b");
  });
});
