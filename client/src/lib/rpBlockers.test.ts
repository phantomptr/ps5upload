import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ProcessInfo } from "../api/ps5";

// A console in memory: its files, its process list, and what was sent to it.
const files = new Map<string, string>();
let procs: ProcessInfo[] = [];
let truncated = false;
const sent: string[] = [];
let native = true;

const b64 = (s: string) => btoa(String.fromCharCode(...new TextEncoder().encode(s)));

vi.mock("./tauriEnv", () => ({ isTauriEnv: () => native }));
vi.mock("../api/ps5", () => ({
  fsReadPreview: async (_addr: string, path: string) => {
    const t = files.get(path);
    if (t === undefined) throw new Error("not found");
    return { size: t.length, base64: b64(t) };
  },
  fsWriteText: async (_addr: string, path: string, text: string) => {
    files.set(path, text);
    return { ok: true };
  },
  fsMkdir: async () => undefined,
  fsDelete: async (_addr: string, path: string) => {
    files.delete(path);
  },
  fsListDir: async (_addr: string, dir: string) => {
    const names = [...files.keys()].filter((p) => p.startsWith(`${dir}/`)).map((p) => p.slice(dir.length + 1));
    if (!names.length) throw new Error("not found");
    return names.map((name) => ({ name }));
  },
  fsPathExists: async (_addr: string, path: string) =>
    [...files.keys()].some((p) => p === path || p.startsWith(`${path}/`)),
  processList: async () => ({ processes: procs, truncated }),
  payloadsLocalPath: async (id: string) => `/cache/${id}/${id}.elf`,
  payloadsRelease: async () => ({ picked_asset_url: "", tag: "" }),
  payloadsDownload: async () => ({ path: "" }),
  sendPayload: async (_host: string, path: string) => {
    sent.push(path);
    return { viaPayloadManager: false };
  },
}));

import {
  HUSH_BLOCKER,
  RP_BLOCKERS,
  blockerCtx,
  checkBlockers,
  detectBlockers,
  pausedMarkerPath,
  pausedOnConsole,
} from "./rpBlockers";

const CONFIG = "# hush\nenabled=1\nmode=apply\nverbose=0\ninterval_s=20\nrescan_s=300\n";
const HOLD_CONFIG = "# hush\nenabled=1\nmode=apply\nverbose=0\ninterval_s=20\nrescan_s=60\nhold_s=45\n";
/** state.txt while the patch is in place: it names the patched process (SceAvCapture, pid 61). */
const STATE = "gadget=0x8a1234\nbase=0x8a0000\npid=61\nslot=0x8b0010 0x8a1234\n";
const STATE_PATH = "/data/hush/state.txt";
const LOG = "2026-10-31 10:00:00 hush started; firmware=13.60 (0x13600007) enabled=1 mode=apply\n";
const hushProc: ProcessInfo = {
  pid: 142,
  name: "payload.elf",
  comm: "payload.elf",
  title_id: "",
  app_id: 0,
  memory_mib: 1,
  threads: 1,
  kind: "payload",
};
const captureProc: ProcessInfo = { ...hushProc, pid: 61, name: "SceAvCapture", comm: "SceAvCapture", kind: "system" };
const statusOf = async () => (await checkBlockers(blockerCtx("192.168.1.99")))[0]?.status ?? { kind: "off" };

beforeEach(() => {
  files.clear();
  sent.length = 0;
  native = true;
  truncated = false;
  procs = [hushProc, captureProc];
  files.set("/data/hush/config.ini", CONFIG);
  files.set(STATE_PATH, STATE);
  files.set("/data/hush/pid.lock", "142\n");
  files.set("/data/hush/log.txt", LOG);
});

describe("Remote Play blockers", () => {
  it("lists Hush, by its catalogue id", () => {
    expect(RP_BLOCKERS.map((b) => b.id)).toEqual(["hush"]);
  });

  it("finds Hush in the way only while its patch is in place (state.txt)", async () => {
    expect((await detectBlockers(blockerCtx("192.168.1.99"))).map((b) => b.id)).toEqual(["hush"]);
    expect(await statusOf()).toEqual({ kind: "on" });

    // Running with nothing patched: no warning, no pause.
    files.delete(STATE_PATH);
    expect(await detectBlockers(blockerCtx("192.168.1.99"))).toEqual([]);

    files.clear();
    expect(await detectBlockers(blockerCtx("192.168.1.99"))).toEqual([]);
  });

  it("in hold mode: no warning once the hold is over, a bounded wait while it lasts", async () => {
    files.set("/data/hush/config.ini", HOLD_CONFIG);
    files.delete(STATE_PATH);
    expect(await statusOf()).toEqual({ kind: "off" });
    files.set(STATE_PATH, STATE);
    expect(await statusOf()).toEqual({ kind: "wait", maxMs: 50_000 });
  });

  it("calls a patch left by a Hush that stopped stuck, with how to clear it", async () => {
    procs = [captureProc];
    files.delete("/data/hush/pid.lock");
    expect(await statusOf()).toMatchObject({ kind: "stuck", key: "rp_blocker_hush_leftover_restart" });
    files.set("/data/hush/config.ini", HOLD_CONFIG);
    expect(await statusOf()).toMatchObject({ kind: "stuck", key: "rp_blocker_hush_leftover_reload" });
  });

  it("ignores a state.txt whose process is gone (left from before a restart)", async () => {
    procs = [{ ...captureProc, pid: 77 }];
    expect(await statusOf()).toEqual({ kind: "off" });
    // Unless the process list was cut short: then it may still be there.
    truncated = true;
    expect(await statusOf()).toMatchObject({ kind: "stuck" });
  });

  it("waits for state.txt to go, within the time it is given", async () => {
    let t = 0;
    let polls = 0;
    const clear = await HUSH_BLOCKER.waitClear!(blockerCtx("192.168.1.99"), 50_000, {
      now: () => t,
      sleep: async (ms) => {
        t += ms;
        if (++polls === 10) files.delete(STATE_PATH);
      },
    });
    expect(clear).toBe("clear");
    expect(t).toBe(10_000);
    // Hush never lets go: give up after the bound, without touching its config.
    files.set(STATE_PATH, STATE);
    t = 0;
    const out = await HUSH_BLOCKER.waitClear!(blockerCtx("192.168.1.99"), 50_000, {
      now: () => t,
      sleep: async (ms) => {
        t += ms;
      },
    });
    expect(out).toBe("timeout");
    expect(t).toBe(50_000);
    expect(files.get("/data/hush/config.ini")).toBe(CONFIG);
    // A cancelled wait stops at once.
    const ac = new AbortController();
    ac.abort();
    expect(await HUSH_BLOCKER.waitClear!(blockerCtx("192.168.1.99"), 50_000, { signal: ac.signal })).toBe("timeout");
  });

  it("pauses Hush: enabled=0, our marker, then waits for its log to say the slots are back", async () => {
    let t = 0;
    let polls = 0;
    const outcome = await HUSH_BLOCKER.pause(blockerCtx("192.168.1.99"), {
      now: () => t,
      sleep: async (ms) => {
        t += ms;
        if (++polls === 3) {
          files.set(
            "/data/hush/log.txt",
            `${LOG}2026-10-31 10:00:20 sceVencStartEncode restored in 4 slot(s)\n2026-10-31 10:00:20 disabled in config, exiting\n`,
          );
        }
      },
    });
    expect(outcome).toBe("restored");
    expect(polls).toBe(3);
    expect(files.get("/data/hush/config.ini")).toBe(CONFIG.replace("enabled=1", "enabled=0"));
    expect(files.has(pausedMarkerPath("hush"))).toBe(true);
    expect(await pausedOnConsole("192.168.1.99:9113")).toEqual(["hush"]);
  });

  it("starts anyway when Hush has not answered within about 25 s", async () => {
    let t = 0;
    const outcome = await HUSH_BLOCKER.pause(blockerCtx("192.168.1.99"), {
      now: () => t,
      sleep: async (ms) => {
        t += ms;
      },
    });
    expect(outcome).toBe("timeout");
    expect(t).toBeGreaterThanOrEqual(25_000);
    expect(t).toBeLessThan(28_000);
  });

  it("turns Hush back on and loads it again, since enabled=0 made it exit", async () => {
    files.set("/data/hush/config.ini", CONFIG.replace("enabled=1", "enabled=0"));
    files.set(pausedMarkerPath("hush"), "paused\n");
    files.delete("/data/hush/pid.lock");
    procs = [];
    expect(await HUSH_BLOCKER.resume(blockerCtx("192.168.1.99"))).toBe("running");
    expect(files.get("/data/hush/config.ini")).toBe(CONFIG);
    expect(files.has(pausedMarkerPath("hush"))).toBe(false);
    expect(sent).toEqual(["/cache/hush/hush.elf"]);
  });

  it("does not send Hush again when it still runs", async () => {
    files.set("/data/hush/config.ini", CONFIG.replace("enabled=1", "enabled=0"));
    expect(await HUSH_BLOCKER.resume(blockerCtx("192.168.1.99"))).toBe("running");
    expect(sent).toEqual([]);
  });

  it("asks for a send where payloads cannot be sent (the browser build)", async () => {
    native = false;
    procs = [];
    files.set("/data/hush/config.ini", CONFIG.replace("enabled=1", "enabled=0"));
    expect(await HUSH_BLOCKER.resume(blockerCtx("192.168.1.99"))).toBe("needs_send");
    expect(files.get("/data/hush/config.ini")).toBe(CONFIG);
  });

  it("has no paused marker on a console it never paused", async () => {
    expect(await pausedOnConsole("192.168.1.99:9113")).toEqual([]);
  });
});
