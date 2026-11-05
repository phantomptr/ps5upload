import { describe, expect, it } from "vitest";

import { toMeshState, type MeshState } from "../api/mesh";
import { busyText, replaceCheck } from "../lib/helperGuard";
import { deviceName, nearbyJoins, othersBusy, othersOnline, othersRemotePlaying, presenceItems, remotePlayPads, RP_ACTIVITY } from "./mesh";
import { mergeShared, type SharedProfile } from "./rosterShare";
import type { Task } from "./tasks";

const dev = (id: string, extra: Record<string, unknown> = {}) => ({
  id,
  name: `dev ${id}`,
  nick: "",
  kind: "desktop",
  os: "macOS",
  app: "8.0.0",
  first: 1,
  last: 2,
  ip: "1.2.3.4",
  online: true,
  blocked: false,
  activity: [],
  ...extra,
});

function state(devices: Record<string, unknown>[]): MeshState {
  const a = toMeshState({ supported: true, rev: 5, now: 10, you: "me", helper: "8.0.0", devices, locks: [], events: [], ev: 0 });
  if (!a.supported || a.same) throw new Error("expected a full state");
  return a as MeshState;
}

describe("toMeshState", () => {
  it("reads an older helper as unsupported and a quiet poll as same", () => {
    expect(toMeshState({ supported: false })).toEqual({ supported: false });
    expect(toMeshState({ supported: true, same: true, rev: 9 })).toEqual({ supported: true, same: true, rev: 9 });
  });
  it("drops malformed devices and activity", () => {
    const s = state([dev("a", { activity: [{ kind: "Upload", title: "X", pct: 4 }, { nope: 1 }, 7] }), { name: "no id" }]);
    expect(s.devices).toHaveLength(1);
    expect(s.devices[0].activity).toEqual([{ kind: "Upload", title: "X", pct: 4, stage: undefined, count: undefined }]);
  });
});

describe("who is busy", () => {
  it("leaves this device, offline devices and browser counts out", () => {
    const s = state([
      dev("me", { activity: [{ kind: "Upload", title: "Mine" }] }),
      dev("b", { activity: [{ kind: "Install", title: "Astro Bot", pct: 42 }] }),
      dev("c", { online: false, activity: [{ kind: "Upload", title: "Old" }] }),
      dev("d", { activity: [{ kind: "browsers", count: 2 }] }),
    ]);
    expect(othersOnline(s).map((d) => d.id)).toEqual(["b", "d"]);
    const busy = othersBusy(s);
    expect(busy.map((b) => b.device.id)).toEqual(["b"]);
    expect(busyText(busy)).toBe("dev b: Install Astro Bot 42%");
  });

  it("finds the other devices playing Remote Play on the console", () => {
    const s = state([
      dev("me", { activity: [{ kind: RP_ACTIVITY }] }),
      dev("phone", { kind: "android", activity: [{ kind: RP_ACTIVITY }, { kind: "Upload", title: "X" }] }),
      dev("pc", { activity: [{ kind: "Upload", title: "Y" }] }),
      dev("gone", { online: false, activity: [{ kind: RP_ACTIVITY }] }),
    ]);
    expect(othersRemotePlaying(s).map((d) => d.id)).toEqual(["phone"]);
    expect(othersRemotePlaying(null)).toEqual([]);
  });

  it("offers the devices that take this one as a controller, with where to join", () => {
    const s = state([
      dev("me", { activity: [{ kind: RP_ACTIVITY, pad: "ws://192.168.1.9:19113/api/pad/ws" }] }),
      dev("pc", { activity: [{ kind: RP_ACTIVITY, pad: "ws://192.168.1.20:19113/api/pad/ws" }] }),
      dev("old", { activity: [{ kind: RP_ACTIVITY }] }), // an app before 9.5.0
      dev("odd", { activity: [{ kind: RP_ACTIVITY, pad: "javascript:alert(1)" }] }),
    ]);
    expect(remotePlayPads(s).map((x) => [x.device.id, x.pad])).toEqual([["pc", "ws://192.168.1.20:19113/api/pad/ws"]]);
    // The address survives the engine's answer being checked field by field.
    const parsed = toMeshState({
      supported: true,
      you: "me",
      devices: [{ id: "pc", online: true, activity: [{ kind: RP_ACTIVITY, pad: "ws://192.168.1.20:19113/api/pad/ws" }] }],
    });
    expect(parsed.supported && !parsed.same && parsed.devices[0].activity[0].pad).toBe("ws://192.168.1.20:19113/api/pad/ws");
  });

  it("keeps a friend's Play Together through the engine's answer being checked", () => {
    const join = {
      code: "ABC123",
      players: 1,
      max: 4,
      host_name: "Sam",
      console_name: "PS5 Pro",
      game_title: "Astro",
      ws: "ws://192.168.1.20:19113/api/join/ws",
    };
    const parsed = toMeshState({
      supported: true,
      you: "me",
      devices: [
        { id: "me", online: true, activity: [] },
        { id: "pc", online: true, activity: [{ kind: RP_ACTIVITY, join }] },
        // Nothing to join with: a damaged or partial entry is not offered.
        { id: "odd", online: true, activity: [{ kind: RP_ACTIVITY, join: { code: "X" } }] },
      ],
    });
    expect(parsed.supported && !parsed.same).toBe(true);
    expect(nearbyJoins(parsed as MeshState)).toEqual([{ device: expect.objectContaining({ id: "pc" }), join }]);
  });

  it("names an app from before 8.0.0 plainly, and prefers the nickname", () => {
    expect(deviceName({ nick: "", name: "ps5upload", app: "" })).toBe("PS5Upload (older version)");
    expect(deviceName({ nick: "Office PC", name: "DESKTOP-1", app: "8.0.0" })).toBe("Office PC");
  });
});

describe("replaceCheck", () => {
  it("flags a downgrade only when the running helper is newer", () => {
    expect(replaceCheck(null, "8.1.0", "8.0.0").downgrade).toEqual({ running: "8.1.0", ours: "8.0.0" });
    expect(replaceCheck(null, "7.5.3", "8.0.0").downgrade).toBeNull();
    expect(replaceCheck(null, null, "8.0.0").downgrade).toBeNull();
  });
});

describe("presenceItems", () => {
  it("publishes this console's running work with a percentage", () => {
    const t = (over: Partial<Task>): Task =>
      ({
        id: "t",
        kind: "upload-file",
        origin: "x",
        createdAt: "",
        status: "running",
        consoleId: "192.0.2.99",
        payload: {},
        label: "Astro Bot",
        updatedAtMs: 0,
        progress: { current: 25, total: 100, unit: "bytes" },
        ...over,
      }) as Task;
    const items = presenceItems(
      [t({}), t({ id: "2", status: "done" }), t({ id: "3", consoleId: "10.0.0.1" }), t({ id: "4", status: "queued" })],
      "192.0.2.99:9120",
    );
    expect(items).toEqual([{ kind: "Upload", title: "Astro Bot", pct: 25 }]);
  });
});

describe("console list merge on first contact", () => {
  const p = (id: string, name = id): SharedProfile => ({ id, name, host: `${id}.lan` }) as SharedProfile;
  it("keeps the consoles each side knew (nothing dropped)", () => {
    const merged = mergeShared([], [p("phat"), p("pro")], [p("phat", "Phat (renamed)"), p("tv")]);
    expect(merged.map((x) => x.id).sort()).toEqual(["phat", "pro", "tv"]);
  });
});

describe("the same PS5 from two devices (8.0.0 listed it twice)", () => {
  it("collapses duplicates already saved, keeping the selected one and filling in what it lacks", async () => {
    const { collapseByHost } = await import("./roster");
    const list = [
      { id: "a", name: "PS5", host: "192.0.2.99", notes: null, wake_credential: null },
      { id: "b", name: "Phat", host: "192.0.2.99", notes: "fan loud", wake_credential: "cred" },
      { id: "c", name: "Pro", host: "192.0.2.100", notes: null, wake_credential: null },
    ];
    const out = collapseByHost(list, "b");
    expect(out.map((p) => p.id)).toEqual(["b", "c"]);
    const kept = collapseByHost(list, "c")[0];
    expect(kept).toMatchObject({ id: "a", name: "PS5", notes: "fan loud", wake_credential: "cred" });
  });
});
