import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";

const { build, status, notify } = vi.hoisted(() => ({
  build: vi.fn(),
  status: vi.fn(),
  notify: vi.fn(),
}));

vi.mock("../api/fpkg", () => ({ fpkg: { build } }));
vi.mock("../api/ps5", () => ({ jobStatus: status, jobCancel: vi.fn() }));
vi.mock("./notifications", () => ({ pushNotification: notify }));

import { useFpkgConversion } from "./fpkgConversion";

describe("FPKG conversion tracking", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    build.mockReset();
    status.mockReset();
    notify.mockReset();
    useFpkgConversion.setState({ jobId: null, job: null, error: null, starting: false });
  });

  afterEach(() => vi.useRealTimers());

  it("keeps polling and reports success without a mounted screen", async () => {
    build.mockResolvedValue({ job_id: "conversion-1" });
    status.mockResolvedValueOnce({ status: "running", bytes_sent: 5, total_bytes: 10 });
    status.mockResolvedValueOnce({ status: "done", dest: "/out/game.pkg" });

    await useFpkgConversion.getState().start({ source: "/game" });
    await vi.advanceTimersByTimeAsync(1100);

    expect(useFpkgConversion.getState().job?.status).toBe("done");
    expect(useFpkgConversion.getState().jobId).toBeNull();
    expect(notify).toHaveBeenCalledWith(
      "success",
      "FPKG conversion complete",
      expect.objectContaining({ body: "/out/game.pkg", link: "/convert" }),
    );
  });

  it("surfaces a failed job", async () => {
    build.mockResolvedValue({ job_id: "conversion-2" });
    status.mockResolvedValue({ status: "failed", error: "disk full" });

    await useFpkgConversion.getState().start({ source: "/game" });
    await vi.advanceTimersByTimeAsync(600);

    expect(useFpkgConversion.getState().job?.status).toBe("failed");
    expect(notify).toHaveBeenCalledWith(
      "error",
      "FPKG conversion failed",
      expect.objectContaining({ body: "disk full" }),
    );
  });
});
