import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const pushNotification = vi.fn();
vi.mock("./notifications", () => ({
  pushNotification: (...a: unknown[]) => pushNotification(...a),
}));

import { clearNotice, deleteSourceAfterUpload } from "./uploadQueue";
import type { QueueItem } from "./uploadQueue";

const item = (over: Partial<QueueItem>): QueueItem =>
  ({ id: "x", addr: "10.0.0.2", displayName: "Game", status: "pending", sourceKind: "file", ...over }) as QueueItem;

describe("queue Clear notice", () => {
  it("says a running upload was stopped (Clear cancels it)", () => {
    const up = item({ id: "u", status: "running", sourceKind: "folder" });
    const n = clearNotice([up], []);
    expect(n?.title).toMatch(/stopped/);
    expect(n?.body).not.toMatch(/run to completion/);
  });

  it("says a running install keeps going (Clear keeps it)", () => {
    const inst = item({ id: "i", status: "running", sourceKind: "install" });
    const n = clearNotice([inst], [inst]);
    expect(n?.title).toMatch(/install is still running/);
  });

  it("says nothing when nothing was running", () => {
    expect(clearNotice([item({ status: "done" })], [])).toBeNull();
  });
});

describe("delete source after upload", () => {
  beforeEach(() => pushNotification.mockClear());

  it("warns when the delete failed instead of dropping it", async () => {
    await deleteSourceAfterUpload("/games/a.pkg", "10.0.0.2", async () => {
      throw new Error("permission denied");
    });
    expect(pushNotification).toHaveBeenCalledWith(
      "warning",
      expect.stringContaining("not deleted"),
      { body: expect.stringContaining("permission denied") },
    );
  });

  it("stays quiet when the delete worked", async () => {
    await deleteSourceAfterUpload("/games/a.pkg", "10.0.0.2", async () => ({ ok: true }));
    expect(pushNotification).not.toHaveBeenCalled();
  });
});
