import { beforeEach, describe, expect, it } from "vitest";

import {
  SPEED_TEST_REMOTE,
  runSpeedTest,
  speedTestFor,
  stopSpeedTest,
  useSpeedTestStore,
  type SpeedTestDeps,
} from "./speedTest";

const A = "10.0.0.5";
const MIB = 1024 * 1024;
const at = () => speedTestFor(useSpeedTestStore.getState(), A);

function world(over: Partial<SpeedTestDeps> = {}) {
  const log: string[] = [];
  // Each job: two polls running, then done with the engine's own timing.
  const polls: Record<string, number> = {};
  const deps: SpeedTestDeps = {
    prepare: async (mib) => {
      log.push(`prepare ${mib}`);
      return {
        path: "/pc/speed.bin",
        download_dir: "/pc/back",
        bytes: mib * MIB,
      };
    },
    mkdir: async (_h, p) => void log.push(`mkdir ${p}`),
    startUpload: async (src, dest) => (
      log.push(`upload ${src} -> ${dest}`),
      "up"
    ),
    startDownload: async (remote, dir) => (
      log.push(`download ${remote} -> ${dir}`),
      "down"
    ),
    jobStatus: async (id) => {
      polls[id] = (polls[id] ?? 0) + 1;
      if (polls[id] < 3)
        return {
          status: "running",
          bytes_sent: 10 * MIB,
          total_bytes: 100 * MIB,
        };
      // upload: 100 MiB in 1 s; download: 100 MiB in 2 s
      return {
        status: "done",
        bytes_sent: 100 * MIB,
        total_bytes: 100 * MIB,
        elapsed_ms: id === "up" ? 1000 : 2000,
      };
    },
    jobCancel: async (id) => void log.push(`cancel ${id}`),
    deleteRemote: async (_h, p) => void log.push(`delete ${p}`),
    cleanup: async () => void log.push("cleanup"),
    sleep: async () => {},
    ...over,
  };
  return { deps, log };
}

beforeEach(() => useSpeedTestStore.setState({ byHost: {} }));

describe("runSpeedTest", () => {
  it("sends a file to the PS5 and reads it back, and reports both speeds", async () => {
    const w = world();
    await runSpeedTest(A, 100, w.deps);
    expect(at()).toMatchObject({
      phase: "done",
      sizeBytes: 100 * MIB,
      uploadBps: 100 * MIB,
      downloadBps: 50 * MIB,
      error: null,
    });
    expect(w.log).toEqual([
      "prepare 100",
      "mkdir /data/ps5upload/tests",
      `upload /pc/speed.bin -> ${SPEED_TEST_REMOTE}`,
      `download ${SPEED_TEST_REMOTE} -> /pc/back`,
      `delete ${SPEED_TEST_REMOTE}`,
      "cleanup",
    ]);
  });

  it("shows which leg is running and how far it is", async () => {
    const seen: string[] = [];
    const w = world({
      sleep: async () => {
        const s = at();
        seen.push(`${s?.phase} ${s?.sent}/${s?.total}`);
      },
    });
    await runSpeedTest(A, 100, w.deps);
    expect(seen).toContain(`uploading ${10 * MIB}/${100 * MIB}`);
    expect(seen).toContain(`downloading ${10 * MIB}/${100 * MIB}`);
  });

  it("removes its files from both machines when a leg fails, and says which", async () => {
    const w = world({
      jobStatus: async (id) =>
        id === "up"
          ? {
              status: "done",
              bytes_sent: MIB,
              total_bytes: MIB,
              elapsed_ms: 100,
            }
          : { status: "failed", error: "connection lost" },
    });
    await runSpeedTest(A, 16, w.deps);
    expect(at()).toMatchObject({
      phase: "failed",
      error: "connection lost",
      failedLeg: "download",
    });
    // The upload's figure is kept: it was measured.
    expect(at()?.uploadBps).toBeGreaterThan(0);
    expect(w.log.slice(-2)).toEqual([`delete ${SPEED_TEST_REMOTE}`, "cleanup"]);
  });

  it("stops when asked, cancels the running leg and still cleans up", async () => {
    let w: ReturnType<typeof world>;
    // eslint-disable-next-line prefer-const
    w = world({
      jobStatus: async () => ({
        status: "running",
        bytes_sent: 1,
        total_bytes: 100,
      }),
      sleep: async () => stopSpeedTest(A),
    });
    await runSpeedTest(A, 16, w.deps);
    expect(at()?.phase).toBe("stopped");
    expect(w.log).toContain("cancel up");
    expect(w.log.slice(-2)).toEqual([`delete ${SPEED_TEST_REMOTE}`, "cleanup"]);
  });

  it("a second start while one runs does nothing", async () => {
    let release: () => void = () => {};
    const w = world({
      prepare: async (mib) => {
        await new Promise<void>((r) => (release = r));
        return { path: "/p", download_dir: "/d", bytes: mib * MIB };
      },
    });
    const first = runSpeedTest(A, 16, w.deps);
    await Promise.resolve();
    const w2 = world();
    await runSpeedTest(A, 16, w2.deps);
    expect(w2.log).toEqual([]);
    release();
    await first;
  });
});
