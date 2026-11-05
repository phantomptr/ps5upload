import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  applyBundle,
  buildBackupBundle,
  buildBundle,
  forConsole,
  keepLiveWakeKeys,
  pickSlot,
  trimBundle,
  type Bundle,
} from "./settingsBackup";
import { installWindowStub } from "../test-utils/storage";

describe("settings bundles", () => {
  beforeEach(installWindowStub);
  afterEach(() => vi.unstubAllGlobals());

  it("take only the app's keys, and never their own bookkeeping", () => {
    window.localStorage.setItem("ps5upload.theme", "dark");
    window.localStorage.setItem("other.app", "x");
    window.localStorage.setItem("ps5upload.settings_backup.last_auto.v1", "1");
    const b = buildBundle("daily", "Mac");
    expect(b.local_storage).toEqual({ "ps5upload.theme": "dark" });
    expect(b.meta).toMatchObject({ schema: 1, reason: "daily", device: "Mac" });
  });

  describe("pairing keys", () => {
    const paired = {
      id: "p1",
      host: "10.0.0.2",
      name: "Pro",
      wake_credential: "cred",
      wake_regist_key: "rk",
      wake_rp_key: "rp",
    };
    const KEY = "ps5upload.roster.v1";
    const roster = (...profiles: object[]) => JSON.stringify({ profiles, active_id: "p1" });

    it("never go into a bundle; the rest of the roster does", () => {
      window.localStorage.setItem(KEY, roster(paired));
      const b = buildBundle();
      const text = JSON.stringify(b);
      for (const secret of ["cred", "rk", "rp"]) expect(text).not.toContain(`\\"${secret}\\"`);
      expect(JSON.parse(b.local_storage[KEY]!)).toEqual({
        profiles: [{ id: "p1", host: "10.0.0.2", name: "Pro" }],
        active_id: "p1",
      });
    });

    it("a roster that cannot be read stays out of the bundle", () => {
      window.localStorage.setItem(KEY, "not json wake_rp_key");
      expect(buildBundle().local_storage).toEqual({});
    });

    it("the backup copies keep them: the engine seals them and a console gets only its own", () => {
      window.localStorage.setItem(KEY, roster(paired, { ...paired, id: "p2", host: "10.0.0.3" }));
      const b = buildBackupBundle("daily");
      expect(JSON.parse(b.local_storage[KEY]!).profiles[0].wake_rp_key).toBe("rp");
      const onConsole = JSON.parse(forConsole(b, "10.0.0.2:9113").local_storage[KEY]!);
      expect(onConsole.profiles[0].wake_rp_key).toBe("rp");
      expect(onConsole.profiles[1].wake_rp_key).toBeUndefined();
    });

    it("a roster that cannot be read stays out of the backup copies too", () => {
      window.localStorage.setItem(KEY, "not json wake_rp_key");
      expect(buildBackupBundle().local_storage).toEqual({});
    });

    it("a profile with no id takes keys by address only", () => {
      const out = JSON.parse(
        keepLiveWakeKeys(
          roster({ host: "10.0.0.9" }),
          roster({ host: "10.0.0.2", wake_rp_key: "rp" }),
        ),
      );
      expect(out.profiles[0].wake_rp_key).toBeUndefined();
    });

    it("a restore keeps the keys this app holds, by id or by address", () => {
      window.localStorage.setItem(KEY, roster(paired));
      applyBundle({
        meta: {},
        local_storage: {
          [KEY]: roster(
            { id: "p1", host: "10.0.0.2", name: "Renamed" },
            { id: "other", host: "10.0.0.2:9113", name: "Same console" },
            { id: "new", host: "10.0.0.3", name: "Never paired here" },
          ),
        },
      });
      const { profiles } = JSON.parse(window.localStorage.getItem(KEY)!) as { profiles: Array<Record<string, string>> };
      expect(profiles[0]).toMatchObject({ name: "Renamed", wake_regist_key: "rk", wake_rp_key: "rp" });
      expect(profiles[1]).toMatchObject({ name: "Same console", wake_credential: "cred" });
      expect(profiles[2].wake_rp_key).toBeUndefined();
    });

    it("keys in a bundle from an older version stand", () => {
      window.localStorage.setItem(KEY, roster(paired));
      applyBundle({ meta: {}, local_storage: { [KEY]: roster({ ...paired, wake_rp_key: "older" }) } });
      expect(JSON.parse(window.localStorage.getItem(KEY)!).profiles[0].wake_rp_key).toBe("older");
    });
  });

  it("restore writes our namespace only and refuses what is not a bundle", () => {
    const n = applyBundle({
      meta: {},
      local_storage: { "ps5upload.a": "1", "ps5upload.b": null, "evil.key": "x" },
    });
    expect(n).toBe(2);
    expect(window.localStorage.getItem("ps5upload.a")).toBe("1");
    expect(window.localStorage.getItem("evil.key")).toBeNull();
    expect(() => applyBundle({ nope: 1 })).toThrow();
    expect(() => applyBundle({ local_storage: { "ps5upload.c": 5 } })).toThrow();
    // Nothing was written by the refused one.
    expect(window.localStorage.getItem("ps5upload.c")).toBeNull();
  });

  describe("wake keys", () => {
    const ROSTER = "ps5upload.roster.v1";
    const roster = (profiles: Record<string, unknown>[]) => JSON.stringify({ profiles, active_id: "a" });
    const keys = { wake_credential: "111", wake_regist_key: "aa", wake_rp_key: "bb" };
    const read = (text: string | null) => JSON.parse(text ?? "{}").profiles as Record<string, unknown>[];

    it("a restore without the keys keeps the ones this device has", () => {
      window.localStorage.setItem(ROSTER, roster([{ id: "a", host: "10.0.0.2", ...keys }]));
      applyBundle({
        meta: { secrets_withheld: true },
        local_storage: { [ROSTER]: roster([{ id: "a", host: "10.0.0.2", name: "Restored" }]) },
      });
      const [p] = read(window.localStorage.getItem(ROSTER));
      expect(p).toMatchObject({ name: "Restored", ...keys });
    });

    it("a restore that has the keys brings them to a wiped device", () => {
      applyBundle({
        meta: {},
        local_storage: { [ROSTER]: roster([{ id: "a", host: "10.0.0.2", ...keys }]) },
      });
      expect(read(window.localStorage.getItem(ROSTER))[0]).toMatchObject(keys);
    });

    it("matches a console by id, then by address, and never overrides what the backup holds", () => {
      const live = roster([{ id: "other", host: "10.0.0.2", wake_rp_key: "live" }]);
      const out = read(
        keepLiveWakeKeys(roster([{ id: "a", host: "10.0.0.2", wake_regist_key: "kept" }]), live),
      );
      expect(out[0]).toMatchObject({ wake_regist_key: "kept", wake_rp_key: "live" });
      const unrelated = read(keepLiveWakeKeys(roster([{ id: "z", host: "10.0.0.9" }]), live));
      expect(unrelated[0].wake_rp_key).toBeUndefined();
    });

    it("leaves text that is no roster as it is", () => {
      expect(keepLiveWakeKeys("not json", roster([]))).toBe("not json");
      expect(keepLiveWakeKeys(roster([]), "not json")).toBe(roster([]));
    });

    it("a console's copy holds only that console's keys", () => {
      const b: Bundle = {
        meta: { generated_at_ms: 1, schema: 1 },
        local_storage: {
          [ROSTER]: roster([
            { id: "a", host: "10.0.0.2", ...keys },
            { id: "b", host: "10.0.0.3", ...keys },
          ]),
          "ps5upload.theme": "dark",
        },
      };
      const out = forConsole(b, " 10.0.0.2 ");
      const [mine, other] = read(out.local_storage[ROSTER]);
      expect(mine).toMatchObject(keys);
      expect(other.wake_regist_key).toBeUndefined();
      expect(other.wake_credential).toBeUndefined();
      expect(out.local_storage["ps5upload.theme"]).toBe("dark");
      // The bundle it was made from is not touched: the engine's copy needs it whole.
      expect(read(b.local_storage[ROSTER])[1]).toMatchObject(keys);
    });
  });

  it("the console copy drops the largest values first to fit", () => {
    const b: Bundle = {
      meta: { generated_at_ms: 1, schema: 1 },
      local_storage: { "ps5upload.small": "s", "ps5upload.big": "x".repeat(5000), "ps5upload.mid": "y".repeat(500) },
    };
    const t = trimBundle(b, 2000);
    expect(Object.keys(t.local_storage).sort()).toEqual(["ps5upload.mid", "ps5upload.small"]);
    expect(t.meta.dropped).toEqual(["ps5upload.big"]);
  });

  it("never backs up or restores this app's own sync identity", () => {
    window.localStorage.setItem("ps5upload.mesh_state.v2", '{"device":"me"}');
    expect(buildBundle().local_storage).toEqual({});
    applyBundle({ local_storage: { "ps5upload.mesh_state.v2": '{"device":"other"}' } });
    expect(window.localStorage.getItem("ps5upload.mesh_state.v2")).toBe('{"device":"me"}');
  });

  it("an app keeps its console slot; a new one takes a free slot, then the oldest", () => {
    const e = (slot: number, app: string, at: number) => ({ slot, app, name: app, at, keys: 1 });
    expect(pickSlot([e(1, "a", 5)], "a")).toBe(1);
    expect(pickSlot([e(1, "a", 5)], "b")).toBe(2);
    const full = Array.from({ length: 16 }, (_, i) => e(i + 1, `x${i}`, 100 - i));
    expect(pickSlot(full, "new")).toBe(16);
  });
});
