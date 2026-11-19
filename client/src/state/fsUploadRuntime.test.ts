import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
import { installLocalStorageStub } from "../test-utils/storage";
installLocalStorageStub();
const resume = vi.hoisted(() => vi.fn());
vi.mock("./fsUpload", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./fsUpload")>();
  return { ...actual, resumeFsUpload: resume };
});

import { useFsUploadStore } from "./fsUpload";
import { fsUploadQueueFor, pauseFsUploadQueue, useFsUploadQueue } from "./fsUploadQueue";
import { resumeStoppedFsUploadOnWake } from "./fsUploadRuntime";
import { useUploadSettingsStore } from "./uploadSettings";

const HOST = "10.0.0.7";

describe("a Files upload stopped by an outage, when the console answers again", () => {
  beforeEach(() => {
    resume.mockReset();
    useUploadSettingsStore.setState({ autoResume: true });
    useFsUploadStore.setState({
      byHost: {
        [HOST]: { active: null, stopped: { why: "failed", reason: null, error: "connection reset" } },
      },
    } as never);
    // What the failed pick left behind: the queue paused, with a pick still waiting. (The pick is
    // itself paused here so that the test's queue has nothing to send.)
    useFsUploadQueue.setState({
      byHost: {
        [HOST]: {
          items: [{ id: "p2", request: { host: HOST }, allowedExisting: [], paused: "user" }],
          paused: false,
          running: false,
          error: null,
          currentId: null,
        },
      },
    } as never);
    pauseFsUploadQueue(HOST);
    expect(fsUploadQueueFor(HOST).paused).toBe(true);
  });

  it("lets the picks queued behind it go on once it has resumed", async () => {
    resume.mockResolvedValue({ ok: true });
    expect(resumeStoppedFsUploadOnWake(HOST)).toBe(true);
    await Promise.resolve();
    await Promise.resolve();
    expect(fsUploadQueueFor(HOST).paused).toBe(false);
  });

  it("leaves the queue paused when the resume did not go through", async () => {
    resume.mockResolvedValue({ ok: false, error: "still down" });
    expect(resumeStoppedFsUploadOnWake(HOST)).toBe(true);
    await Promise.resolve();
    await Promise.resolve();
    expect(fsUploadQueueFor(HOST).paused).toBe(true);
  });
});
