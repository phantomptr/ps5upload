import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ProcessInfo } from "../api/ps5";

// A console in memory: its files, its process list, and what was sent to it.
const files = new Map<string, string>();
let procs: ProcessInfo[] = [];
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
  fsPathExists: async (_addr: string, dir: string) => [...files.keys()].some((p) => p.startsWith(`${dir}/`)),
  processList: async () => ({ processes: procs, truncated: false }),
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
  detectBlockers,
  pausedMarkerPath,
  pausedOnConsole,
} from "./rpBlockers";

const CONFIG = "# hush\nenabled=1\nmode=apply\nverbose=0\ninterval_s=20\nrescan_s=300\n";
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

beforeEach(() => {
  files.clear();
  sent.length = 0;
  native = true;
  procs = [hushProc];
  files.set("/data/hush/config.ini", CONFIG);
  files.set("/data/hush/pid.lock", "142\n");
  files.set("/data/hush/log.txt", LOG);
});

describe("Remote Play blockers", () => {
  it("lists Hush, by its catalogue id", () => {
    expect(RP_BLOCKERS.map((b) => b.id)).toEqual(["hush"]);
  });

  it("finds Hush only when it runs with enabled=1", async () => {
    expect((await detectBlockers(blockerCtx("192.168.1.99"))).map((b) => b.id)).toEqual(["hush"]);

    files.set("/data/hush/config.ini", CONFIG.replace("enabled=1", "enabled=0"));
    expect(await detectBlockers(blockerCtx("192.168.1.99"))).toEqual([]);

    files.set("/data/hush/config.ini", CONFIG);
    procs = [];
    expect(await detectBlockers(blockerCtx("192.168.1.99"))).toEqual([]);

    files.clear();
    expect(await detectBlockers(blockerCtx("192.168.1.99"))).toEqual([]);
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
