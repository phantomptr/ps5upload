import { beforeEach, describe, expect, it, vi } from "vitest";

// The task store persists through `window.localStorage`; vitest's node env has no window.
vi.hoisted(() => {
  const mem = new Map<string, string>();
  (globalThis as { window?: unknown }).window = {
    localStorage: {
      getItem: (k: string) => (mem.has(k) ? (mem.get(k) as string) : null),
      setItem: (k: string, v: string) => void mem.set(k, String(v)),
      removeItem: (k: string) => void mem.delete(k),
      clear: () => mem.clear(),
    },
    location: { origin: "http://127.0.0.1:19113" },
    addEventListener: () => {},
    removeEventListener: () => {},
  };
});

const fetchJob = vi.fn();
const jobStatus = vi.fn();
vi.mock("../api/remote", () => ({ remoteApi: { fetch: (...a: unknown[]) => fetchJob(...a) } }));
vi.mock("../api/ps5", () => ({ jobStatus: (...a: unknown[]) => jobStatus(...a) }));

import { useConnectionsStore } from "../state/connections";
import { useTaskStore } from "../state/tasks";
import { materializeRemote } from "./materialize";

describe("materializeRemote", () => {
  beforeEach(() => {
    fetchJob.mockReset();
    jobStatus.mockReset();
    useTaskStore.setState({ tasks: [] });
    useConnectionsStore.setState({
      connections: [
        {
          id: "nas-1",
          name: "NAS",
          protocol: "smb",
          host: "h",
          port: 445,
          share: "g",
          user: "",
          start_path: "",
          host_key: null,
          has_secret: false,
        },
      ],
    });
  });

  it("hands a local path back untouched", async () => {
    expect(await materializeRemote("/Users/me/a.zip", { pollMs: 1 })).toBe("/Users/me/a.zip");
    expect(fetchJob).not.toHaveBeenCalled();
  });

  it("copies a server file and resolves with the local copy, as a task", async () => {
    fetchJob.mockResolvedValue({ job_id: "j1" });
    jobStatus
      .mockResolvedValueOnce({ status: "running", bytes_sent: 5, total_bytes: 10 })
      .mockResolvedValueOnce({ status: "done", dest: "/tmp/x/a.zip", bytes_sent: 10 });
    const local = await materializeRemote("remote://nas-1/games/a.zip", { pollMs: 1 });
    expect(local).toBe("/tmp/x/a.zip");
    expect(fetchJob).toHaveBeenCalledWith("remote://nas-1/games/a.zip", undefined);
    const t = useTaskStore.getState().tasks[0];
    expect(t).toMatchObject({ status: "done", label: "Copy a.zip from NAS" });
  });

  it("fails with the copy's own error, and the task says so", async () => {
    fetchJob.mockResolvedValue({ job_id: "j2" });
    jobStatus.mockResolvedValueOnce({ status: "failed", error: "Not enough space" });
    await expect(materializeRemote("remote://nas-1/a.zip", { pollMs: 1 })).rejects.toThrow(
      "Not enough space",
    );
    expect(useTaskStore.getState().tasks[0]?.status).toBe("failed");
  });
});
