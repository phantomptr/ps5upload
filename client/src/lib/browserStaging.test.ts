import { beforeEach, describe, expect, it, vi } from "vitest";

import { useTaskStore } from "../state/tasks";
import {
  cancelStaging,
  isStagingCancelled,
  planRelease,
  stageWithTask,
} from "./browserStaging";

const NOW = 1_000_000_000_000;
const entry = (uploadId: string, held = false, at = NOW) => ({
  uploadId,
  at,
  held,
});

describe("planRelease", () => {
  it("keeps a copy that is on its way to a queue", () => {
    const all = { "/s/a.pkg": entry("a") };
    const { keep, release } = planRelease(all, new Set(), NOW);
    expect(release).toEqual([]);
    expect(keep["/s/a.pkg"]).toEqual(all["/s/a.pkg"]);
  });

  it("marks a copy in use, and deletes it when its work has gone", () => {
    const all = { "/s/a.pkg": entry("a") };
    const first = planRelease(all, new Set(["/s/a.pkg"]), NOW);
    expect(first.release).toEqual([]);
    expect(first.keep["/s/a.pkg"].held).toBe(true);
    const second = planRelease(first.keep, new Set(), NOW);
    expect(second.release).toEqual(["a"]);
    expect(second.keep).toEqual({});
  });

  it("forgets one nothing ever used, after the engine's own sweep has had it", () => {
    const old = {
      "/s/a.pkg": entry("a", false, NOW - 9 * 24 * 60 * 60 * 1000),
    };
    const { keep, release } = planRelease(old, new Set(), NOW);
    expect(keep).toEqual({});
    expect(release).toEqual([]);
  });
});

describe("stageWithTask", () => {
  const file = { name: "game.pkg", size: 100 } as File;
  beforeEach(() => useTaskStore.setState({ tasks: [] }));

  it("shows progress in a task and finishes it", async () => {
    const seen: number[] = [];
    const staged = await stageWithTask(
      file,
      async (_f, opts) => {
        opts?.onProgress?.(40, 100);
        expect(useTaskStore.getState().tasks[0].progress?.current).toBe(40);
        return {
          uploadId: "u",
          path: "/s/game.pkg",
          filename: "game.pkg",
          size: 100,
        };
      },
      (sent) => seen.push(sent),
    );
    expect(staged.uploadId).toBe("u");
    expect(seen).toEqual([40]);
    expect(useTaskStore.getState().tasks[0].status).toBe("done");
  });

  it("Cancel on the task aborts the upload", async () => {
    const started = vi.fn();
    const run = stageWithTask(file, (_f, opts) => {
      started();
      return new Promise((_resolve, reject) => {
        opts?.signal?.addEventListener("abort", () =>
          reject(new DOMException("stopped", "AbortError")),
        );
      });
    });
    const control = useTaskStore.getState().tasks[0].control as {
      stagingId: string;
    };
    expect(cancelStaging(control.stagingId)).toBe(true);
    const err = await run.catch((e) => e);
    expect(isStagingCancelled(err)).toBe(true);
    expect(useTaskStore.getState().tasks[0].status).toBe("cancelled");
    expect(started).toHaveBeenCalledOnce();
  });

  it("a failure is the task's error too", async () => {
    await expect(
      stageWithTask(file, async () => {
        throw new Error("disk full");
      }),
    ).rejects.toThrow("disk full");
    const t = useTaskStore.getState().tasks[0];
    expect(t.status).toBe("failed");
    expect(t.lastError?.message).toBe("disk full");
  });
});
