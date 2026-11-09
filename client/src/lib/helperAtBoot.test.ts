import { beforeEach, describe, expect, it, vi } from "vitest";

const data = new Map<string, string>();
vi.stubGlobal("window", {
  localStorage: {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
    removeItem: (k: string) => void data.delete(k),
  },
});

import type { PmAutoload, PmOverview, PmStoredPayload } from "../api/payloadManager";
import { autoloadKeys } from "../api/payloadManager";
import { LOADER_SETTLE_MS } from "./autoRedeploy";
import {
  BOOT_PAUSE,
  bootVerdict,
  disableHelperAtBoot,
  enableHelperAtBoot,
  helperAtBootSettleMs,
  isBootCopy,
  listsHelper,
  loadBootRecord,
  saveBootRecord,
  storedHelper,
  syncHelperAtBoot,
  withHelperAtBoot,
  withoutHelperAtBoot,
  type BootRecord,
  type SyncDeps,
} from "./helperAtBoot";

const H = "192.168.0.9";
const PATH = "/data/pldmgr/payloads/local/ps5upload/ps5upload.elf";

const helper: PmStoredPayload = {
  path: PATH,
  slot: "local/ps5upload",
  filename: "ps5upload.elf",
  folder: "ps5upload",
  name: "",
  version: "",
  source: "",
  install_source: "",
};
const kstuff: PmStoredPayload = { ...helper, path: "/data/pldmgr/payloads/local/kstuff/kstuff.elf", slot: "local/kstuff", filename: "kstuff.elf", folder: "kstuff" };

function pm(list: string[], payloads = [kstuff, helper], enabled = true): PmOverview {
  return {
    present: true,
    version: "0.6",
    payloads,
    payloadsError: null,
    autoload: { enabled, delay_s: 10, list },
    autoloadError: null,
  };
}

const rec = (over: Partial<BootRecord> = {}): BootRecord => ({
  path: PATH,
  version: "11.5.0",
  prevVersion: "11.4.0",
  storedAt: 1_000_000,
  pending: false,
  badVersion: "",
  pauseAdded: true,
  delayS: 10,
  ...over,
});

beforeEach(() => data.clear());

describe("the boot list", () => {
  it("puts the helper last, behind a pause when something loads before it", () => {
    expect(withHelperAtBoot(["local/kstuff"], "local/ps5upload")).toEqual({
      list: ["local/kstuff", BOOT_PAUSE, "local/ps5upload"],
      pauseAdded: true,
    });
    expect(withHelperAtBoot([], "local/ps5upload")).toEqual({ list: ["local/ps5upload"], pauseAdded: false });
    // The user's own pause at the end stays the only one.
    expect(withHelperAtBoot(["local/kstuff", "!5000"], "k").list).toEqual(["local/kstuff", "!5000", "k"]);
    // Already listed (perhaps moved by the user): left as it is.
    expect(withHelperAtBoot(["k", "local/kstuff"], "k").list).toEqual(["k", "local/kstuff"]);
  });

  it("takes out the helper and only the pause this app added", () => {
    const list = ["local/kstuff", BOOT_PAUSE, "k", "x"];
    expect(withoutHelperAtBoot(list, "k", true)).toEqual(["local/kstuff", "x"]);
    expect(withoutHelperAtBoot(list, "k", false)).toEqual(["local/kstuff", BOOT_PAUSE, "x"]);
  });

  it("counts the helper as listed under its slot, its file name or its path (#781)", () => {
    // Payload Manager 0.6 starts a bare file name or a path as well as the slot: a list written
    // under any of them (by hand, by 0.5, by another tool) already starts the helper.
    for (const entry of ["local/ps5upload", "ps5upload.elf", PATH]) {
      expect(listsHelper({ enabled: true, delay_s: 0, list: ["local/kstuff", entry] }, helper)).toBe(true);
      expect(withHelperAtBoot(["local/kstuff", entry], autoloadKeys(helper)).list).toEqual(["local/kstuff", entry]);
      expect(withoutHelperAtBoot(["local/kstuff", BOOT_PAUSE, entry], autoloadKeys(helper), true)).toEqual([
        "local/kstuff",
      ]);
    }
    expect(listsHelper({ enabled: true, delay_s: 0, list: ["local/kstuff", "PS5Upload.elf"] }, helper)).toBe(false);
    expect(listsHelper({ enabled: true, delay_s: 0, list: ["local/ps5upload"] }, null)).toBe(false);
    // Added under the slot on 0.6, the file name on 0.5.
    expect(withHelperAtBoot([], autoloadKeys(helper)).list).toEqual(["local/ps5upload"]);
    expect(withHelperAtBoot([], autoloadKeys({ ...helper, slot: "" })).list).toEqual(["ps5upload.elf"]);
  });

  it("finds Payload Manager's own copy of the helper, never a USB one or the installer", () => {
    const usb = { ...helper, path: "/mnt/usb0/ps5upload.elf" };
    const installer = { ...helper, filename: "ps5upload-installer.elf", path: "/data/pldmgr/payloads/local/x/ps5upload-installer.elf" };
    expect(storedHelper([usb, installer, kstuff])).toBeNull();
    expect(storedHelper([usb, helper])?.path).toBe(PATH);
  });

  it("waits longer before the reconnect loop sends a helper on a console that loads one at boot", () => {
    expect(helperAtBootSettleMs(null)).toBe(LOADER_SETTLE_MS);
    expect(helperAtBootSettleMs(rec({ delayS: 10 }))).toBe(LOADER_SETTLE_MS + 70_000);
  });
});

describe("judging a new copy after a boot", () => {
  const pending = rec({ pending: true, storedAt: 1_000_000 });
  it("waits for a boot after the copy was written", () => {
    expect(bootVerdict(pending, { bootEpochMs: 900_000, appSentAt: null, runningVersion: "11.5.0" })).toBe("wait");
    expect(bootVerdict(pending, { bootEpochMs: null, appSentAt: null, runningVersion: "11.5.0" })).toBe("wait");
    expect(bootVerdict(rec(), { bootEpochMs: 5_000_000, appSentAt: null, runningVersion: "x" })).toBe("none");
  });
  it("came up: the helper answering is that version and this app sent none since the boot", () => {
    expect(bootVerdict(pending, { bootEpochMs: 2_000_000, appSentAt: 1_500_000, runningVersion: "11.5.0" })).toBe("came-up");
  });
  it("failed: this app had to send the helper after that boot", () => {
    expect(bootVerdict(pending, { bootEpochMs: 2_000_000, appSentAt: 2_100_000, runningVersion: "11.5.0" })).toBe("failed");
  });
  it("another helper answering is no verdict", () => {
    expect(bootVerdict(pending, { bootEpochMs: 2_000_000, appSentAt: null, runningVersion: "11.0.0" })).toBe("wait");
  });
});

/** A Payload Manager whose boot list a write changes; `keep: false` answers OK and keeps nothing. */
function fakePm(list: string[], opts: { payloads?: PmStoredPayload[]; enabled?: boolean; keep?: boolean } = {}) {
  const state = { list: [...list], enabled: opts.enabled ?? true, writes: [] as { enabled: boolean; list: string[] }[] };
  const payloads = opts.payloads ?? [kstuff, helper];
  return {
    state,
    pmOverview: async () => pm(state.list, payloads, state.enabled),
    setAutoload: async (_h: string, enabled: boolean, l: string[]): Promise<PmAutoload> => {
      state.writes.push({ enabled, list: l });
      if (opts.keep !== false) {
        state.list = [...l];
        state.enabled = enabled;
      }
      return { enabled: state.enabled, delay_s: 10, list: [...state.list] };
    },
  };
}

function syncDeps(over: Partial<SyncDeps> = {}) {
  const calls = { replace: 0, restore: 0 };
  const deps: SyncDeps = {
    pmOverview: async () => pm(["local/kstuff", BOOT_PAUSE, "local/ps5upload"]),
    setAutoload: async () => {
      throw new Error("no write expected");
    },
    replace: async () => {
      calls.replace++;
      return "updated";
    },
    restore: async () => {
      calls.restore++;
      return "restored";
    },
    bootEpochMs: async () => 2_000_000,
    appSentAt: () => null,
    runningVersion: () => "11.5.0",
    appVersion: async () => "11.6.0",
    now: () => 3_000_000,
    load: loadBootRecord,
    save: saveBootRecord,
    ...over,
  };
  return { deps, calls };
}

describe("syncing on each connection", () => {
  it("does nothing for a console it was never turned on for", async () => {
    const { deps, calls } = syncDeps();
    expect(await syncHelperAtBoot(H, deps)).toEqual({ kind: "off" });
    expect(calls.replace).toBe(0);
  });

  it("writes nothing when Payload Manager is gone or its payloads could not be read", async () => {
    saveBootRecord(H, rec());
    const gone = syncDeps({ pmOverview: async () => ({ present: false, error: "refused" }) });
    expect(await syncHelperAtBoot(H, gone.deps)).toEqual({ kind: "no-pm" });
    // An unread payload list is not "the helper was deleted".
    const unread = syncDeps({
      pmOverview: async () => ({ ...pm(["local/kstuff"], []), payloadsError: "HTTP 500" }),
    });
    expect(await syncHelperAtBoot(H, unread.deps)).toEqual({ kind: "error", error: "HTTP 500" });
    expect(loadBootRecord(H)).not.toBeNull();
    expect(gone.calls.replace + unread.calls.replace).toBe(0);
  });

  it("puts the helper back when it went missing from the boot list while the switch is on (#781)", async () => {
    saveBootRecord(H, rec({ pauseAdded: false }));
    const lines: string[] = [];
    const con = fakePm(["local/kstuff"]);
    const { deps, calls } = syncDeps({ ...con, log: (l) => void lines.push(l) });
    expect(await syncHelperAtBoot(H, deps)).toEqual({ kind: "updated", version: "11.6.0" });
    expect(con.state.list).toEqual(["local/kstuff", BOOT_PAUSE, "local/ps5upload"]);
    expect(con.state.enabled).toBe(true);
    expect(calls.replace).toBe(1);
    expect(loadBootRecord(H)?.pauseAdded).toBe(true);
    // What was read, written and read back is in the log.
    expect(lines.join("\n")).toMatch(/not in the boot list/);
    expect(lines.join("\n")).toMatch(/read back enabled=true \[local\/kstuff,!3000,local\/ps5upload\]/);
  });

  it("leaves a list that names the helper by file name or path as it is", async () => {
    saveBootRecord(H, rec());
    const con = fakePm(["local/kstuff", "ps5upload.elf"]);
    const { deps } = syncDeps({ ...con });
    expect(await syncHelperAtBoot(H, deps)).toEqual({ kind: "updated", version: "11.6.0" });
    expect(con.state.writes).toEqual([]);
  });

  it("puts it back on a boot list that is off, keeping it off", async () => {
    saveBootRecord(H, rec());
    const con = fakePm([], { enabled: false });
    const { deps } = syncDeps({ ...con });
    await syncHelperAtBoot(H, deps);
    expect(con.state.writes).toEqual([{ enabled: false, list: ["local/ps5upload"] }]);
  });

  it("is an error, not a silent success, when Payload Manager does not keep the list", async () => {
    saveBootRecord(H, rec());
    const con = fakePm(["local/kstuff"], { keep: false });
    const { deps, calls } = syncDeps({ ...con });
    const r = await syncHelperAtBoot(H, deps);
    expect(r.kind).toBe("error");
    expect(r.kind === "error" && r.error).toMatch(/does not include the helper.*local\/kstuff/);
    expect(calls.replace).toBe(0);
    // Still on: the next connection tries again.
    expect(loadBootRecord(H)).not.toBeNull();
  });

  it("turns itself off when the stored copy was deleted in Payload Manager", async () => {
    saveBootRecord(H, rec());
    const con = fakePm(["local/kstuff"], { payloads: [kstuff] });
    const { deps, calls } = syncDeps({ ...con });
    expect(await syncHelperAtBoot(H, deps)).toEqual({ kind: "missing" });
    expect(con.state.writes).toEqual([]);
    expect(calls.replace).toBe(0);
    expect(loadBootRecord(H)).toBeNull();
  });

  it("brings the copy up to this app's helper, keeping the previous, and marks it pending", async () => {
    saveBootRecord(H, rec());
    const { deps, calls } = syncDeps();
    expect(await syncHelperAtBoot(H, deps)).toEqual({ kind: "updated", version: "11.6.0" });
    expect(calls.replace).toBe(1);
    expect(loadBootRecord(H)).toMatchObject({ version: "11.6.0", prevVersion: "11.5.0", pending: true, storedAt: 3_000_000 });
    expect(isBootCopy(`${H}:9113`, PATH)).toBe(true);
  });

  it("writes nothing while a new copy is not judged yet", async () => {
    saveBootRecord(H, rec({ pending: true, storedAt: 2_500_000 }));
    const { deps, calls } = syncDeps();
    expect(await syncHelperAtBoot(H, deps)).toEqual({ kind: "waiting" });
    expect(calls.replace + calls.restore).toBe(0);
  });

  it("clears a copy that came up", async () => {
    saveBootRecord(H, rec({ pending: true, storedAt: 1_000_000 }));
    const { deps } = syncDeps();
    expect(await syncHelperAtBoot(H, deps)).toEqual({ kind: "came-up", version: "11.5.0" });
    expect(loadBootRecord(H)?.pending).toBe(false);
  });

  it("puts the previous copy back when the new one did not come up, and never stores that version again", async () => {
    saveBootRecord(H, rec({ pending: true, version: "11.6.0", prevVersion: "11.5.0", storedAt: 1_000_000 }));
    const failed = syncDeps({ appSentAt: () => 2_050_000, appVersion: async () => "11.6.0" });
    expect(await syncHelperAtBoot(H, failed.deps)).toEqual({ kind: "restored", failed: "11.6.0", back: "11.5.0" });
    expect(failed.calls.restore).toBe(1);
    expect(loadBootRecord(H)).toMatchObject({ pending: false, badVersion: "11.6.0", version: "11.5.0" });

    const again = syncDeps({ appVersion: async () => "11.6.0" });
    expect(await syncHelperAtBoot(H, again.deps)).toEqual({ kind: "skipped-bad", version: "11.6.0" });
    expect(again.calls.replace).toBe(0);

    // The next app version tries again.
    const next = syncDeps({ appVersion: async () => "11.7.0" });
    expect(await syncHelperAtBoot(H, next.deps)).toEqual({ kind: "updated", version: "11.7.0" });
    expect(loadBootRecord(H)?.badVersion).toBe("");
  });
});

describe("turning it on and off", () => {
  it("stores the helper when Payload Manager has none and lists it last", async () => {
    let stored = false;
    const lists: string[][] = [];
    const ok = await enableHelperAtBoot(H, {
      pmOverview: async () => (stored ? pm(["local/kstuff"]) : pm(["local/kstuff"], [kstuff])),
      store: async () => {
        stored = true;
        return true;
      },
      replace: async () => "unchanged",
      setAutoload: async (_h, enabled, list) => {
        lists.push(list);
        return { enabled, delay_s: 10, list };
      },
      appVersion: async () => "11.5.0",
      now: () => 42,
      load: loadBootRecord,
      save: saveBootRecord,
    });
    expect(ok).toBe(true);
    expect(lists).toEqual([["local/kstuff", BOOT_PAUSE, "local/ps5upload"]]);
    expect(loadBootRecord(H)).toMatchObject({ path: PATH, pending: true, storedAt: 42, pauseAdded: true });
  });

  const enableDeps = (con: ReturnType<typeof fakePm>, over: Partial<Parameters<typeof enableHelperAtBoot>[1]> = {}) => ({
    ...con,
    store: async () => true,
    replace: async () => "unchanged" as const,
    appVersion: async () => "11.5.0",
    now: () => 7,
    load: loadBootRecord,
    save: saveBootRecord,
    ...over,
  });

  it("asks before turning on a boot list that is off, and turns it on only on a yes", async () => {
    const yes = fakePm(["local/kstuff"], { enabled: false });
    let asked = 0;
    await enableHelperAtBoot(H, enableDeps(yes, { confirmTurnOnAutoload: async () => (++asked, true) }));
    expect(asked).toBe(1);
    expect(yes.state.writes).toEqual([{ enabled: true, list: ["local/kstuff", BOOT_PAUSE, "local/ps5upload"] }]);

    // Already listed, list off: a yes is the only write.
    const listedOff = fakePm(["local/ps5upload"], { enabled: false });
    await enableHelperAtBoot(H, enableDeps(listedOff, { confirmTurnOnAutoload: async () => true }));
    expect(listedOff.state.writes).toEqual([{ enabled: true, list: ["local/ps5upload"] }]);

    const no = fakePm(["local/kstuff"], { enabled: false });
    await enableHelperAtBoot(H, enableDeps(no, { confirmTurnOnAutoload: async () => false }));
    expect(no.state.writes).toEqual([{ enabled: false, list: ["local/kstuff", BOOT_PAUSE, "local/ps5upload"] }]);

    // A list that is on is never asked about.
    const on = fakePm([]);
    await enableHelperAtBoot(
      H,
      enableDeps(on, {
        confirmTurnOnAutoload: async () => {
          throw new Error("asked");
        },
      }),
    );
    expect(on.state.list).toEqual(["local/ps5upload"]);
  });

  it("does not add a second entry when the helper is listed by file name", async () => {
    const con = fakePm(["local/kstuff", "ps5upload.elf"]);
    await enableHelperAtBoot(H, enableDeps(con));
    expect(con.state.writes).toEqual([]);
    expect(loadBootRecord(H)).not.toBeNull();
  });

  it("throws when the list read back lacks the helper, and keeps the switch on for the next connection", async () => {
    const con = fakePm(["local/kstuff"], { keep: false });
    await expect(enableHelperAtBoot(H, enableDeps(con))).rejects.toThrow(/does not include the helper/);
    expect(loadBootRecord(H)).not.toBeNull();
    // The engine refusing the write is shown too.
    data.clear();
    await expect(
      enableHelperAtBoot(
        H,
        enableDeps(con, {
          setAutoload: async () => {
            throw new Error("Payload Manager refused the config (HTTP 400)");
          },
        }),
      ),
    ).rejects.toThrow(/HTTP 400/);
    expect(loadBootRecord(H)).not.toBeNull();
  });

  it("reads the list again when the engine does not send it back", async () => {
    const con = fakePm(["local/kstuff"]);
    await enableHelperAtBoot(
      H,
      enableDeps(con, {
        setAutoload: async (h, e, l) => {
          await con.setAutoload(h, e, l);
        },
      }),
    );
    expect(con.state.list).toEqual(["local/kstuff", BOOT_PAUSE, "local/ps5upload"]);
  });

  it("refuses without Payload Manager and stops when the user declines replacing its folder", async () => {
    const base = {
      replace: async () => "unchanged" as const,
      setAutoload: async () => {},
      appVersion: async () => "11.5.0",
      now: () => 1,
      load: loadBootRecord,
      save: saveBootRecord,
    };
    await expect(
      enableHelperAtBoot(H, { ...base, pmOverview: async () => ({ present: false, error: "x" }), store: async () => true }),
    ).rejects.toThrow(/Payload Manager/);
    expect(
      await enableHelperAtBoot(H, { ...base, pmOverview: async () => pm([], [kstuff]), store: async () => false }),
    ).toBe(false);
    expect(loadBootRecord(H)).toBeNull();
  });

  it("takes the helper and its pause out and forgets the console", async () => {
    saveBootRecord(H, rec());
    const con = fakePm(["local/kstuff", BOOT_PAUSE, "local/ps5upload"]);
    await disableHelperAtBoot(H, { ...con, load: loadBootRecord, save: saveBootRecord });
    expect(con.state.list).toEqual(["local/kstuff"]);
    expect(loadBootRecord(H)).toBeNull();

    // Off here stays off: the next connection does not put it back.
    const after = syncDeps({ ...con });
    expect(await syncHelperAtBoot(H, after.deps)).toEqual({ kind: "off" });
    expect(con.state.list).toEqual(["local/kstuff"]);
  });

  it("takes out every entry naming the helper", async () => {
    saveBootRecord(H, rec({ pauseAdded: false }));
    const con = fakePm(["ps5upload.elf", "local/kstuff", "local/ps5upload"]);
    await disableHelperAtBoot(H, { ...con, load: loadBootRecord, save: saveBootRecord });
    expect(con.state.list).toEqual(["local/kstuff"]);
  });
});
