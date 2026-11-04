import { beforeEach, describe, expect, it } from "vitest";

import { asDoc, mergeShared, RosterShare, sharedForm, type ShareApi, type SharedProfile } from "./rosterShare";
import { useRosterStore, type PS5Profile } from "./roster";

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
