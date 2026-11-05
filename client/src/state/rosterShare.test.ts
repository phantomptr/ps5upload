import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { applyBundle } from "../lib/settingsBackup";
import { safeGetItem } from "../lib/safeStorage";
import {
  asDoc,
  mergeShared,
  RosterShare,
  startRosterShare,
  seedDefaultConsole,
  sharedForm,
  UNSAVED_KEY,
  PULL_MS,
  type ShareApi,
  type SharedProfile,
} from "./rosterShare";
import { useRosterStore, type PS5Profile } from "./roster";

const ROSTER_KEY = "ps5upload.roster.v1";

const p = (id: string, extra: Partial<PS5Profile> = {}): SharedProfile => ({
  id,
  name: `PS5 ${id}`,
  host: `10.0.0.${id.charCodeAt(0) % 200}`,
  ...extra,
});

describe("mergeShared", () => {
  const a = p("a");
  const b = p("b");
  const c = p("c");

  it("keeps both sides' additions", () => {
    expect(mergeShared([a], [a, b], [a, c]).map((x) => x.id)).toEqual(["a", "c", "b"]);
  });

  it("drops what I removed and keeps what they removed removed", () => {
    expect(mergeShared([a, b, c], [a, c], [a, b]).map((x) => x.id)).toEqual(["a"]);
  });

  it("takes my edit, and theirs where I did not edit", () => {
    const mine = [{ ...a, name: "Living room" }, b];
    const theirs = [a, { ...b, notes: "fan loud" }];
    expect(mergeShared([a, b], mine, theirs)).toEqual([
      { ...a, name: "Living room" },
      { ...b, notes: "fan loud" },
    ]);
  });

  it("my reorder wins for the consoles we both have", () => {
    expect(mergeShared([a, b, c], [c, a, b], [a, b, c, p("d")]).map((x) => x.id)).toEqual([
      "c",
      "a",
      "b",
      "d",
    ]);
  });
});

describe("sharedForm", () => {
  it("leaves out this browser's last-seen cache", () => {
    const full: PS5Profile = { ...p("a"), last_seen_at: 5, last_seen_kernel: "k", last_seen_payload: "v" };
    expect(sharedForm([full])).toEqual([p("a")]);
  });
});

/** An engine copy with revisions, like shared_state.rs. */
function fakeEngine(start: SharedProfile[] | null = null) {
  const doc = { rev: start ? 1 : 0, value: start ? { profiles: start } : null };
  const api: ShareApi = {
    get: async () => JSON.parse(JSON.stringify(doc)),
    put: async (baseRev, profiles) => {
      if (baseRev !== doc.rev) return { conflict: JSON.parse(JSON.stringify(doc)) };
      doc.rev += 1;
      doc.value = { profiles };
      return { rev: doc.rev };
    },
  };
  /** Another browser saving directly. */
  const otherSaves = (profiles: SharedProfile[]) => {
    doc.rev += 1;
    doc.value = { profiles };
  };
  return { api, doc, otherSaves };
}

const ids = () => useRosterStore.getState().profiles.map((x) => x.id);

/** vitest's node environment has no localStorage, and the unsaved mark lives there. */
function stubStorage() {
  const store = new Map<string, string>();
  vi.stubGlobal("window", {
    localStorage: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    },
  });
}

describe("RosterShare", () => {
  beforeEach(() => {
    useRosterStore.setState({ profiles: [], active_id: null });
  });

  it("hands this browser's consoles to an engine that has none", async () => {
    useRosterStore.setState({ profiles: [p("a") as PS5Profile], active_id: "a" });
    const e = fakeEngine();
    await new RosterShare(e.api).start();
    expect(e.doc.rev).toBe(1);
    expect(e.doc.value?.profiles?.map((x) => x.id)).toEqual(["a"]);
  });

  it("an empty browser takes the engine's consoles", async () => {
    const e = fakeEngine([p("a"), p("b")]);
    await new RosterShare(e.api).start();
    expect(ids()).toEqual(["a", "b"]);
    expect(useRosterStore.getState().active_id).toBe("a");
    expect(e.doc.rev).toBe(1);
  });

  it("keeps this browser's last-seen cache when taking the engine's list", async () => {
    useRosterStore.setState({
      profiles: [{ ...p("a"), name: "old", last_seen_kernel: "13.60" }],
      active_id: "a",
    });
    const e = fakeEngine([p("a", { name: "Living room" })]);
    await new RosterShare(e.api).start();
    const [a] = useRosterStore.getState().profiles;
    expect(a.name).toBe("Living room");
    expect(a.last_seen_kernel).toBe("13.60");
  });

  it("saves a local change, and merges when another browser saved first", async () => {
    const e = fakeEngine([p("a")]);
    const share = new RosterShare(e.api);
    await share.start();
    e.otherSaves([p("a"), p("b")]);
    useRosterStore.setState({ profiles: [...useRosterStore.getState().profiles, p("c") as PS5Profile] });
    await share.push();
    expect(e.doc.value?.profiles?.map((x) => x.id)).toEqual(["a", "b", "c"]);
    expect(ids()).toEqual(["a", "b", "c"]);
  });

  it("pull takes another browser's removal and moves the selection off it", async () => {
    const e = fakeEngine([p("a"), p("b")]);
    const share = new RosterShare(e.api);
    await share.start();
    useRosterStore.setState({ active_id: "b" });
    e.otherSaves([p("a")]);
    await share.pull();
    expect(ids()).toEqual(["a"]);
    expect(useRosterStore.getState().active_id).toBe("a");
  });

  it("a pull with nothing new changes nothing and saves nothing", async () => {
    const e = fakeEngine([p("a")]);
    const share = new RosterShare(e.api);
    await share.start();
    const before = useRosterStore.getState().profiles;
    await share.pull();
    await share.push();
    expect(useRosterStore.getState().profiles).toBe(before);
    expect(e.doc.rev).toBe(1);
  });
});

describe("RosterShare when the engine's store goes away", () => {
  beforeEach(() => {
    stubStorage();
    useRosterStore.setState({ profiles: [], active_id: null });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("a page already open keeps its consoles and hands them to an engine that lost its store", async () => {
    const e = fakeEngine([p("a"), p("b")]);
    const share = new RosterShare(e.api);
    await share.start();
    Object.assign(e.doc, { rev: 0, value: null });
    await share.pull();
    expect(ids()).toEqual(["a", "b"]);
    expect(e.doc.rev).toBe(1);
    expect(e.doc.value?.profiles?.map((x) => x.id)).toEqual(["a", "b"]);
  });

  it("a store that went back to an older list loses nothing on either side", async () => {
    const e = fakeEngine([p("a")]);
    const share = new RosterShare(e.api);
    await share.start();
    e.otherSaves([p("a"), p("b")]);
    await share.pull();
    e.doc.rev = 1;
    e.doc.value = { profiles: [p("c")] };
    await share.pull();
    expect(ids().sort()).toEqual(["a", "b", "c"]);
    expect(e.doc.value?.profiles?.map((x) => x.id).sort()).toEqual(["a", "b", "c"]);
  });

  it("a store replaced at the same revision is not read as removals", async () => {
    const e = fakeEngine([p("a")]);
    const share = new RosterShare(e.api);
    await share.start();
    e.doc.value = { profiles: [p("c")] };
    await share.pull();
    expect(ids().sort()).toEqual(["a", "c"]);
  });

  it("an answer with no list in it removes nothing", async () => {
    const e = fakeEngine([p("a")]);
    const share = new RosterShare(e.api);
    await share.start();
    e.doc.rev = 2;
    e.doc.value = {} as { profiles: SharedProfile[] };
    await share.pull();
    expect(ids()).toEqual(["a"]);
  });

  it("another browser really removing every console still applies", async () => {
    const e = fakeEngine([p("a")]);
    const share = new RosterShare(e.api);
    await share.start();
    e.otherSaves([]);
    await share.pull();
    expect(ids()).toEqual([]);
    expect(e.doc.rev).toBe(2);
  });
});

describe("RosterShare with a save that failed", () => {
  beforeEach(() => {
    stubStorage();
    useRosterStore.setState({ profiles: [], active_id: null });
  });
  afterEach(() => vi.unstubAllGlobals());

  const downFor = (e: ReturnType<typeof fakeEngine>) => {
    const state = { down: true };
    const api: ShareApi = {
      get: e.api.get,
      put: async (rev, profiles) => {
        if (state.down) throw new Error("HTTP 502");
        return e.api.put(rev, profiles);
      },
    };
    return { api, state };
  };
  const add = (id: string) =>
    useRosterStore.setState({ profiles: [...useRosterStore.getState().profiles, p(id) as PS5Profile] });

  it("the next pull saves it, with no further change", async () => {
    const e = fakeEngine([p("a")]);
    const d = downFor(e);
    const share = new RosterShare(d.api);
    await share.start();
    add("b");
    await expect(share.push()).rejects.toThrow();
    expect(safeGetItem(UNSAVED_KEY)).not.toBeNull();
    d.state.down = false;
    await share.pull();
    expect(e.doc.value?.profiles?.map((x) => x.id)).toEqual(["a", "b"]);
    expect(safeGetItem(UNSAVED_KEY)).toBeNull();
  });

  it("a reload merges it instead of the engine's list replacing it", async () => {
    const e = fakeEngine([p("a")]);
    const d = downFor(e);
    const share = new RosterShare(d.api);
    await share.start();
    add("b");
    await expect(share.push()).rejects.toThrow();
    e.otherSaves([p("a"), p("c")]);
    d.state.down = false;
    await new RosterShare(d.api).start();
    expect(ids().sort()).toEqual(["a", "b", "c"]);
    expect(e.doc.value?.profiles?.map((x) => x.id).sort()).toEqual(["a", "b", "c"]);
    expect(safeGetItem(UNSAVED_KEY)).toBeNull();
  });

  it("a restored backup goes back to the engine instead of being overwritten", async () => {
    const e = fakeEngine([p("a")]);
    useRosterStore.setState({ profiles: [p("r") as PS5Profile], active_id: "r" });
    applyBundle({ local_storage: { [ROSTER_KEY]: "{}" } });
    await new RosterShare(e.api).start();
    expect(ids()).toEqual(["r"]);
    expect(e.doc.value?.profiles?.map((x) => x.id)).toEqual(["r"]);
    expect(safeGetItem(UNSAVED_KEY)).toBeNull();
  });
});

describe("seedDefaultConsole", () => {
  beforeEach(() => {
    stubStorage();
    useRosterStore.setState({ profiles: [], active_id: null });
  });
  afterEach(() => vi.unstubAllGlobals());
  const empty = () => useRosterStore.getState().profiles.length === 0;

  it("lists the console the setup named, for a first browser", async () => {
    await seedDefaultConsole(empty, async () => ({ default_console: "192.168.1.50" }));
    expect(useRosterStore.getState().profiles.map((x) => x.host)).toEqual(["192.168.1.50"]);
    expect(useRosterStore.getState().active_id).toBe(useRosterStore.getState().profiles[0].id);
  });

  it("adds nothing when none was named, or the engine does not say", async () => {
    await seedDefaultConsole(empty, async () => ({}));
    await seedDefaultConsole(empty, async () => ({ default_console: "  " }));
    expect(ids()).toEqual([]);
  });

  it("does not ask, and does not bring a console back, once there is a list", async () => {
    let asked = false;
    await seedDefaultConsole(
      () => false,
      async () => {
        asked = true;
        return { default_console: "192.168.1.50" };
      },
    );
    expect(asked).toBe(false);
    expect(ids()).toEqual([]);
  });
});

describe("an app's list and its engine's", () => {
  beforeEach(() => {
    useRosterStore.setState({ profiles: [], active_id: null });
  });

  it("on first contact keeps both lists, so nothing is dropped", async () => {
    useRosterStore.setState({ profiles: [p("mine")], active_id: null });
    const e = fakeEngine([p("theirs")]);
    await new RosterShare(e.api, useRosterStore, "union").start();
    expect(ids().sort()).toEqual(["mine", "theirs"]);
    expect(e.doc.value!.profiles.map((x) => x.id).sort()).toEqual(["mine", "theirs"]);
  });

  it("the same console in the engine's key order and without its empty keys is no change", async () => {
    useRosterStore.setState({ profiles: [{ ...p("a"), wake_credential: null }], active_id: null });
    const { id, name, host } = p("a");
    // The engine's document: keys in its order, nothing that was cleared.
    const e = fakeEngine([{ host, id, name }]);
    const share = new RosterShare(e.api, useRosterStore, "union");
    await share.start();
    await share.push();
    await share.pull();
    expect(e.doc.rev).toBe(1);
  });
});

describe("asDoc", () => {
  it("takes only a well-formed answer, so a stray reply can never erase the list", () => {
    expect(asDoc({ rev: 0, value: null })).toEqual({ rev: 0, value: null });
    expect(asDoc({ rev: 3, value: { profiles: [] } })).toEqual({ rev: 3, value: { profiles: [] } });
    for (const bad of [{}, null, [], { rev: "1" }, { rev: -1, value: null }, { rev: 1, value: { profiles: "x" } }]) {
      expect(() => asDoc(bad)).toThrow();
    }
  });

  it("a browser whose engine answers something else keeps its consoles", async () => {
    useRosterStore.setState({ profiles: [p("a") as PS5Profile], active_id: "a" });
    const api: ShareApi = {
      get: async () => asDoc({}),
      put: async () => ({ rev: 1 }),
    };
    await expect(new RosterShare(api).start()).rejects.toThrow();
    expect(ids()).toEqual(["a"]);
  });
});

describe("a change made before the engine answered", () => {
  beforeEach(() => {
    stubStorage();
    vi.useFakeTimers();
    vi.stubGlobal("document", { visibilityState: "visible", addEventListener: () => {}, removeEventListener: () => {} });
    Object.assign(window, { setInterval, clearInterval, addEventListener: () => {}, removeEventListener: () => {} });
    useRosterStore.setState({ profiles: [p("a") as PS5Profile, p("b") as PS5Profile], active_id: null });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  const downThenUp = (e: ReturnType<typeof fakeEngine>) => {
    const state = { down: true };
    const api: ShareApi = {
      get: async () => {
        if (state.down) throw new Error("engine not up");
        return e.api.get();
      },
      put: e.api.put,
    };
    return { api, state };
  };

  it("a console removed while the engine is down stays removed", async () => {
    const e = fakeEngine([p("a"), p("b")]);
    const d = downThenUp(e);
    const stop = startRosterShare(d.api);
    await vi.advanceTimersByTimeAsync(0);
    useRosterStore.setState({ profiles: [p("a") as PS5Profile] });
    expect(safeGetItem(UNSAVED_KEY)).not.toBeNull();
    d.state.down = false;
    await vi.advanceTimersByTimeAsync(PULL_MS);
    expect(ids()).toEqual(["a"]);
    expect(e.doc.value?.profiles?.map((x) => x.id)).toEqual(["a"]);
    expect(safeGetItem(UNSAVED_KEY)).toBeNull();
    stop();
  });

  it("a console added during a failed start is kept and saved", async () => {
    const e = fakeEngine([p("a"), p("b")]);
    const d = downThenUp(e);
    const stop = startRosterShare(d.api);
    await vi.advanceTimersByTimeAsync(0);
    useRosterStore.setState({ profiles: [p("a") as PS5Profile, p("b") as PS5Profile, p("c") as PS5Profile] });
    d.state.down = false;
    await vi.advanceTimersByTimeAsync(PULL_MS);
    expect(ids().sort()).toEqual(["a", "b", "c"]);
    expect(e.doc.value?.profiles?.map((x) => x.id).sort()).toEqual(["a", "b", "c"]);
    stop();
  });
});

describe("keys the engine holds and this app lacks", () => {
  beforeEach(() => {
    stubStorage();
    useRosterStore.setState({ profiles: [p("a") as PS5Profile], active_id: null });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("come to the app, and are no reason to save again", async () => {
    const e = fakeEngine([p("a", { wake_rp_key: "held" })]);
    const share = new RosterShare(e.api, useRosterStore, "union");
    await share.start();
    const rev = e.doc.rev;
    expect(useRosterStore.getState().profiles[0].wake_rp_key).toBe("held");
    await share.pull();
    await share.pull();
    expect(e.doc.rev).toBe(rev);
  });

  it("appearing at the same revision are not read as a lost store", async () => {
    const e = fakeEngine([p("a")]);
    const share = new RosterShare(e.api, useRosterStore, "union");
    await share.start();
    const rev = e.doc.rev;
    e.doc.value = { profiles: [p("a", { wake_rp_key: "held" })] };
    await share.pull();
    await share.pull();
    expect(e.doc.rev).toBe(rev);
    expect(useRosterStore.getState().profiles[0].wake_rp_key).toBe("held");
  });

  it("removed here stay removed", async () => {
    const e = fakeEngine([p("a", { wake_rp_key: "old" })]);
    useRosterStore.setState({ profiles: [p("a", { wake_rp_key: "old" }) as PS5Profile], active_id: null });
    const share = new RosterShare(e.api, useRosterStore, "union");
    await share.start();
    useRosterStore.setState({ profiles: [p("a") as PS5Profile] });
    await share.pull();
    expect(useRosterStore.getState().profiles[0].wake_rp_key).toBeFalsy();
  });
});
