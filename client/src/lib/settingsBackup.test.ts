import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { applyBundle, buildBundle, pickSlot, trimBundle, type Bundle } from "./settingsBackup";
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
