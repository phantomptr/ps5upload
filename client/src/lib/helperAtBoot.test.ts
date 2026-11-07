import { beforeEach, describe, expect, it, vi } from "vitest";

const data = new Map<string, string>();
vi.stubGlobal("window", {
  localStorage: {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
    removeItem: (k: string) => void data.delete(k),
  },
});

import type { PmOverview, PmStoredPayload } from "../api/payloadManager";
import { LOADER_SETTLE_MS } from "./autoRedeploy";
import {
  BOOT_PAUSE,
  bootVerdict,
  disableHelperAtBoot,
  enableHelperAtBoot,
  helperAtBootSettleMs,
  isBootCopy,
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

function syncDeps(over: Partial<SyncDeps> = {}) {
  const calls = { replace: 0, restore: 0 };
  const deps: SyncDeps = {
    pmOverview: async () => pm(["local/kstuff", BOOT_PAUSE, "local/ps5upload"]),
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

  it("writes nothing when Payload Manager is gone or no longer lists the helper", async () => {
    saveBootRecord(H, rec());
    const gone = syncDeps({ pmOverview: async () => ({ present: false, error: "refused" }) });
    expect(await syncHelperAtBoot(H, gone.deps)).toEqual({ kind: "no-pm" });
    const unlisted = syncDeps({ pmOverview: async () => pm(["local/kstuff"]) });
    expect(await syncHelperAtBoot(H, unlisted.deps)).toEqual({ kind: "unlisted" });
    expect(gone.calls.replace + unlisted.calls.replace).toBe(0);
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
      setAutoload: async (_h, _e, list) => void lists.push(list),
      appVersion: async () => "11.5.0",
      now: () => 42,
      load: loadBootRecord,
      save: saveBootRecord,
    });
    expect(ok).toBe(true);
    expect(lists).toEqual([["local/kstuff", BOOT_PAUSE, "local/ps5upload"]]);
    expect(loadBootRecord(H)).toMatchObject({ path: PATH, pending: true, storedAt: 42, pauseAdded: true });
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
    const lists: string[][] = [];
    await disableHelperAtBoot(H, {
      pmOverview: async () => pm(["local/kstuff", BOOT_PAUSE, "local/ps5upload"]),
      setAutoload: async (_h, _e, list) => void lists.push(list),
      load: loadBootRecord,
      save: saveBootRecord,
    });
    expect(lists).toEqual([["local/kstuff"]]);
    expect(loadBootRecord(H)).toBeNull();
  });
});
