import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { call } from "../api";
import { t } from "../strings";
import { addToQueue, cancelQueued, itemFor, linePosition, moveToTop, parseQueue, setPaused, type Queue, type QueueItem } from "./queue";
import { itemLine, QueueView } from "./QueueList";
import { sheetActions, smbSheetActions } from "../files/Files";

const mockCall = vi.mocked(call);

const item = (over: Partial<QueueItem> = {}): QueueItem => ({
  id: 1,
  kind: "copy",
  state: "pending",
  step: 0,
  src: "smb://nas/Games/A",
  dest: "/data/games/A",
  name: "A",
  key: "k:A",
  job: "",
  error: "",
  wait: "",
  elapsed_s: 0,
  bytes_done: 0,
  bytes_total: 0,
  cancel: false,
  ...over,
});

const queue = (items: QueueItem[], over: Partial<Queue> = {}): Queue => ({ paused: false, reason: "", items, ...over });

beforeEach(() => mockCall.mockReset());

describe("the console's queue (#982)", () => {
  it("reads the helper's answer, leaving out what is not an item", () => {
    const q = parseQueue({
      paused: true,
      reason: "interrupted_install",
      items: [{ id: 3, kind: "install", state: "running", step: 1, src: "/data/a.pkg", bytes_done: 5, bytes_total: 10, cancel: false }, { id: 0, kind: "copy" }, { id: 4, kind: "x", state: "pending", src: "y" }, null],
    });
    expect(q.paused).toBe(true);
    expect(q.reason).toBe("interrupted_install");
    expect(q.items).toHaveLength(1);
    expect(q.items[0]).toMatchObject({ id: 3, kind: "install", state: "running", step: 1, bytes_done: 5, name: "", wait: "" });
    expect(parseQueue(null)).toEqual({ paused: false, reason: "", items: [] });
  });

  it("finds a game's own item, an unfinished one before an old one", () => {
    const q = queue([item({ id: 1, key: "k:A", state: "failed" }), item({ id: 2, key: "k:B", state: "running" }), item({ id: 3, key: "k:A" })]);
    expect(itemFor(q, "k:A")?.id).toBe(3);
    expect(itemFor(q, "k:B")?.id).toBe(2);
    expect(itemFor(q, "k:C")).toBeNull();
    expect(linePosition(q, 3)).toBe(1);
    expect(linePosition(q, 2)).toBe(0);
  });

  it("says what each item is doing", () => {
    const q = queue([item({ id: 1, state: "running", bytes_done: 50, bytes_total: 200 }), item({ id: 2 }), item({ id: 3, wait: "pkg_install_busy" })]);
    expect(itemLine(q, q.items[0])).toContain(t("cw_queue_copying"));
    expect(itemLine(q, q.items[0])).toContain(t("cw_home_percent", { n: 25 }));
    expect(itemLine(q, q.items[1])).toBe(t("cw_queue_position", { n: 1 }));
    expect(itemLine(q, q.items[2])).toBe(t("cw_queue_wait_install"));
    const install = item({ kind: "install", state: "running", step: 0, src: "smb://nas/a.pkg" });
    expect(itemLine(queue([install]), install)).toBe(t("cw_queue_staging"));
    expect(itemLine(queue([{ ...install, step: 1 }]), { ...install, step: 1 })).toBe(t("cw_queue_installing"));
    expect(itemLine(queue([]), item({ state: "failed", error: "interrupted" }))).toBe(t("cw_queue_failed", { why: t("cw_queue_err_interrupted") }));
    expect(itemLine(queue([]), item({ state: "cancelled" }))).toBe(t("cw_queue_cancelled"));
  });

  it("shows the running item with its speed line, Cancel; the waiting ones with Move to top and Remove; Clear finished", () => {
    const q = queue([
      item({ id: 1, state: "done", name: "Old" }),
      item({ id: 2, state: "running", job: "smb:x", elapsed_s: 90, bytes_done: 1, bytes_total: 4, name: "Now" }),
      item({ id: 3, name: "Next" }),
      item({ id: 4, name: "Later" }),
    ]);
    const h = renderToStaticMarkup(<QueueView q={q} onError={vi.fn()} />);
    expect(h).toContain(t("cw_queue_title"));
    expect(h).toContain(t("cw_rate_elapsed", { time: "1m 30s" }));
    expect(h).toContain(t("cw_home_cancel_task"));
    // only the second in line can move to the top
    expect(h.split('data-action="queue-top"').length - 1).toBe(1);
    expect(h.split('data-action="queue-cancel"').length - 1).toBe(3);
    expect(h).toContain(t("cw_queue_clear"));
    expect(h).toContain(t("cw_queue_pause"));
    expect(renderToStaticMarkup(<QueueView q={queue([])} onError={vi.fn()} />)).toBe("");
  });

  it("a queue paused by a restart during an install says so and offers Resume", () => {
    const h = renderToStaticMarkup(<QueueView q={queue([item()], { paused: true, reason: "interrupted_install" })} onError={vi.fn()} />);
    expect(h).toContain(t("cw_queue_paused_interrupted"));
    expect(h).toContain(t("cw_queue_resume"));
    expect(h).not.toContain(t("cw_queue_pause") + "<");
  });

  it("Files offers Add to queue for a package and for what is on a share", () => {
    const pkg = { name: "g.pkg", kind: "file" as const, size: 10, mtime: 0 };
    expect(sheetActions(["g.pkg"], [pkg] as never, null)).toContain("queue");
    expect(sheetActions(["a.txt"], [{ ...pkg, name: "a.txt" }] as never, null)).not.toContain("queue");
    expect(smbSheetActions()).toContain("queue");
  });

  it("calls the helper's routes", async () => {
    mockCall.mockResolvedValue({ id: 9 });
    await expect(addToQueue({ kind: "install", src: "/data/a.pkg", name: "a" })).resolves.toBe(9);
    expect(mockCall).toHaveBeenCalledWith("/api/queue/add", { kind: "install", src: "/data/a.pkg", name: "a" });
    mockCall.mockResolvedValue({});
    await cancelQueued(9);
    await moveToTop(9);
    await setPaused(true);
    expect(mockCall.mock.calls.map((c) => c[0])).toEqual(["/api/queue/add", "/api/queue/cancel", "/api/queue/top", "/api/queue/pause"]);
  });
});
