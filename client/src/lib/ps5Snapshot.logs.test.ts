import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./invokeLogged", () => ({ invoke: vi.fn() }));

import { invoke } from "./invokeLogged";
import { useConnectionStore } from "../state/connection";
import { clearBlackBoxes } from "./payloadBlackBox";
import { buildPs5Snapshot } from "./ps5Snapshot";

type Args = { path?: string; offset?: number | null; maxBytes?: number | null };

/** A console whose log directories hold only the files named here (a name, or [name, size]). */
function consoleWith(
  files: Record<string, Array<string | [string, number]>>,
  unlistable: string[] = [],
) {
  const reads: string[] = [];
  const calls: Array<{ path: string; offset: number | null; maxBytes: number | null }> = [];
  vi.mocked(invoke).mockImplementation(async (cmd: string, args?: unknown) => {
    const a = (args as Args | undefined) ?? {};
    const path = a.path ?? "";
    if (cmd === "ps5_list_dir") {
      if (unlistable.includes(path)) throw new Error("fs_list_dir failed");
      return {
        entries: (files[path] ?? []).map((f) => {
          const [name, size] = typeof f === "string" ? [f, 8] : f;
          return { name, kind: "file", size };
        }),
      };
    }
    if (cmd === "fs_read_preview") {
      reads.push(path);
      calls.push({ path, offset: a.offset ?? null, maxBytes: a.maxBytes ?? null });
      return { base64: btoa("log line") };
    }
    return {};
  });
  return Object.assign(reads, { calls });
}

describe("helper log collection", () => {
  beforeEach(() => {
    clearBlackBoxes();
    vi.mocked(invoke).mockReset();
    useConnectionStore.setState({ host: "192.168.1.50", payloadStatus: "up" });
  });

  it("reads only the log files that exist, so a fresh console logs no failed reads", async () => {
    const reads = consoleWith({ "/data/ps5upload": ["stderr.log", "startup.log"] });
    await buildPs5Snapshot({ redact: true });
    expect([...reads].sort()).toEqual(["/data/ps5upload/startup.log", "/data/ps5upload/stderr.log"]);
  });

  /* #903: the start/stop trace's earlier part comes too, so a crash report shows the steps of
   * both helpers of a takeover after the file has moved to .old. */
  it("collects the start/stop trace and its earlier part", async () => {
    const reads = consoleWith({ "/data/ps5upload": ["startup.log", "startup.log.old"] });
    const { payload_logs } = await buildPs5Snapshot({ redact: true });
    expect([...reads].sort()).toEqual(["/data/ps5upload/startup.log", "/data/ps5upload/startup.log.old"]);
    const names = payload_logs.map((f) => f.name);
    expect(names).toContain("startup.log");
    expect(names).toContain("startup_old.log");
  });

  it("still tries a file when its directory cannot be listed", async () => {
    const reads = consoleWith({}, ["/data/shadowmount"]);
    await buildPs5Snapshot({ redact: true });
    expect(reads).toContain("/data/shadowmount/debug.log");
    expect(reads).toContain("/data/shadowmount/config.ini");
    expect(reads).not.toContain("/data/ps5upload/stderr.log");
  });

  /* The FTX2 transaction logs are gone; AVA1's own event log replaces them, and nothing under
   * tx/ is listed or read (a console without the directory used to cost a failed list). */
  it("collects the AVA1 event log and reads nothing under tx/", async () => {
    const reads = consoleWith({
      "/data/ps5upload": ["stderr.log", "stderr.log.old", "crash.log"],
      "/data/ps5upload/ava": ["events.log", "events.log.old", "identity", "peers"],
    });
    const { payload_logs } = await buildPs5Snapshot({ redact: true });
    expect(reads).toContain("/data/ps5upload/ava/events.log");
    expect(reads).toContain("/data/ps5upload/ava/events.log.old");
    expect(reads.some((p) => p.includes("/tx/"))).toBe(false);
    // never the identity or the peer list: they are secrets, not logs
    expect(reads.some((p) => p.endsWith("/identity") || p.endsWith("/peers"))).toBe(false);
    const names = payload_logs.map((f) => f.name);
    expect(names).toContain("ava_events.log");
    expect(names).toContain("ava_events_old.log");
    expect(names.some((n) => n.startsWith("tx_") || n.startsWith("shards_"))).toBe(false);
  });

  /* Reads go through the engine's ranged fs.read (48 KiB pages joined behind one call), capped
   * at 256 KiB. A log longer than the cap is read from its END with an offset. */
  it("bundle_reads_logs_through_fs_read_pages: a short log from 0, a long one as its tail", async () => {
    const reads = consoleWith({
      "/data/ps5upload": [
        ["stderr.log", 100_000],
        ["stderr.log.old", 600_000],
      ],
    });
    const { payload_logs } = await buildPs5Snapshot({ redact: true });
    const short = reads.calls.find((c) => c.path === "/data/ps5upload/stderr.log")!;
    expect(short.offset).toBeNull(); // from the start, default cap
    const long = reads.calls.find((c) => c.path === "/data/ps5upload/stderr.log.old")!;
    expect(long.maxBytes).toBe(256 * 1024);
    expect(long.offset).toBe(600_000 - 256 * 1024);
    const text = payload_logs.find((f) => f.name === "stderr_old.log")!.text;
    expect(text.startsWith("[earlier 337856 bytes omitted")).toBe(true);
    expect(text.endsWith("log line")).toBe(true);
  });
});
