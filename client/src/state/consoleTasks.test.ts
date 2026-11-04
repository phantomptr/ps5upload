import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { parseConsoleTaskList, ConsoleTaskError, type ConsoleTaskInfo } from "../api/consoleTasks";
import * as api from "../api/consoleTasks";
import {
  applyConsoleTasks,
  cancelConsoleTaskRow,
  consoleTaskLabel,
  consoleTaskProgress,
  FRESH_END_S,
  readConsoleTasks,
  resetConsoleTasksState,
  UNSUPPORTED_RETRY_MS,
} from "./consoleTasks";
import { commandTask, taskCapabilities } from "./taskControls";
import { useTaskStore } from "./tasks";
import { anyAwakeWork } from "../lib/awakeWork";
import { presenceItems } from "./mesh";
import * as transfers from "../lib/ps5Transfers";

const HOST = "192.0.2.100";

const info = (over: Partial<ConsoleTaskInfo> = {}): ConsoleTaskInfo => ({
  id: "aa",
  kind: "copy",
  state: "running",
  name: "game.iso",
  files_done: 1,
  files_total: 4,
  bytes_done: 100,
  bytes_total: 400,
  age_s: 0,
  cancellable: true,
  ...over,
});

const rows = () => useTaskStore.getState().tasks.filter((t) => t.kind === "console-task");

beforeEach(() => {
  useTaskStore.setState({ tasks: [] });
  resetConsoleTasksState();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("mapping", () => {
  it("names a task by what it does and what it works on", () => {
    expect(consoleTaskLabel({ kind: "delete", name: "old" })).toBe("Deleting · old");
    expect(consoleTaskLabel({ kind: "move", name: "" })).toBe("Moving");
    expect(consoleTaskLabel({ kind: "something-new", name: "x" })).toBe("Working · x");
  });

  it("measures progress in bytes when it can, else in files, else not at all", () => {
    expect(consoleTaskProgress(info())).toEqual({ current: 100, total: 400, unit: "bytes" });
    expect(consoleTaskProgress(info({ bytes_total: 0 }))).toEqual({ current: 1, total: 4, unit: "files" });
    expect(consoleTaskProgress(info({ bytes_total: 0, files_total: 0 }))).toBeUndefined();
  });

  it("reads an odd reply as an empty list, and a reply without the flag as supported", () => {
    expect(parseConsoleTaskList(null)).toEqual({ supported: true, tasks: [] });
    expect(parseConsoleTaskList({ supported: false, tasks: [] }).supported).toBe(false);
    expect(parseConsoleTaskList({ tasks: [{ nope: 1 }, info()] }).tasks).toHaveLength(1);
  });
});

describe("applyConsoleTasks", () => {
  it("adds a running task, marked as started on the PS5, once", () => {
    applyConsoleTasks(HOST, [info()]);
    applyConsoleTasks(HOST, [info({ bytes_done: 200 })]);
    expect(rows()).toHaveLength(1);
    const [t] = rows();
    expect(t.status).toBe("running");
    expect(t.label).toBe("Copying · game.iso");
    expect(t.detail).toBe("Started on the PS5");
    expect(t.consoleId).toBe(HOST);
    expect(t.progress).toEqual({ current: 200, total: 400, unit: "bytes" });
  });

  it("keeps each console's tasks to itself", () => {
    applyConsoleTasks(HOST, [info()]);
    applyConsoleTasks("192.0.2.99", []);
    expect(rows()[0].status).toBe("running");
    applyConsoleTasks("192.0.2.99", [info()]);
    expect(rows()).toHaveLength(2);
  });

  it.each([
    ["done", "done"],
    ["failed", "failed"],
    ["cancelled", "cancelled"],
    ["gone", "unverified"],
  ] as const)("a task that ended %s ends its row %s", (state, status) => {
    applyConsoleTasks(HOST, [info()]);
    applyConsoleTasks(HOST, [info({ state, age_s: 1 })]);
    expect(rows()[0].status).toBe(status);
    expect(rows()[0].endedAtMs).toBeTruthy();
  });

  it("shows a task that ended between two reads", () => {
    applyConsoleTasks(HOST, [info({ state: "done", age_s: 3 })]);
    expect(rows()[0].status).toBe("done");
  });

  it("does not report what ended long before this app looked", () => {
    applyConsoleTasks(HOST, [info({ state: "done", age_s: FRESH_END_S + 1 })]);
    expect(rows()).toHaveLength(0);
  });

  it("has nothing to say about a task nobody saw run or end", () => {
    applyConsoleTasks(HOST, [info({ state: "gone", age_s: 0 })]);
    expect(rows()).toHaveLength(0);
  });

  it("does not bring back a finished row the user dismissed", () => {
    applyConsoleTasks(HOST, [info({ state: "done", age_s: 3 })]);
    useTaskStore.getState().removeTask(rows()[0].id);
    applyConsoleTasks(HOST, [info({ state: "done", age_s: 13 })]);
    expect(rows()).toHaveLength(0);
  });

  it("ends a row whose task is no longer listed, without claiming how", () => {
    applyConsoleTasks(HOST, [info()]);
    applyConsoleTasks(HOST, []);
    expect(rows()[0].status).toBe("unverified");
  });

  it("is not the app's own work: closing the app or sleeping does not concern it", () => {
    applyConsoleTasks(HOST, [info()]);
    expect(anyAwakeWork(useTaskStore.getState().tasks)).toBe(false);
    expect(presenceItems(useTaskStore.getState().tasks, HOST)).toEqual([]);
  });
});

describe("readConsoleTasks", () => {
  it("backs off from a helper without the list, with no error, then asks again", async () => {
    const f = vi.spyOn(api, "fetchConsoleTasks").mockResolvedValue({ supported: false, tasks: [] });
    let now = 1000;
    await readConsoleTasks(HOST, () => now);
    await readConsoleTasks(HOST, () => now);
    expect(f).toHaveBeenCalledTimes(1);
    expect(rows()).toHaveLength(0);
    now += UNSUPPORTED_RETRY_MS + 1;
    await readConsoleTasks(HOST, () => now);
    expect(f).toHaveBeenCalledTimes(2);
  });

  it("leaves the rows alone when a read fails", async () => {
    applyConsoleTasks(HOST, [info()]);
    vi.spyOn(api, "fetchConsoleTasks").mockRejectedValue(new Error("no answer"));
    await readConsoleTasks(HOST);
    expect(rows()[0].status).toBe("running");
  });

  it("adds no traffic during a transfer to that console", async () => {
    vi.spyOn(transfers, "transferScreenBusy").mockReturnValue(true);
    const f = vi.spyOn(api, "fetchConsoleTasks");
    await readConsoleTasks(HOST);
    expect(f).not.toHaveBeenCalled();
  });
});

describe("cancel", () => {
  it("is offered only for a running task the console can stop", () => {
    applyConsoleTasks(HOST, [info(), info({ id: "bb", kind: "pkg_install", cancellable: false })]);
    const [install, copy] = rows();
    expect(taskCapabilities(copy).canCancel).toBe(true);
    expect(taskCapabilities(install).canCancel).toBe(false);
    applyConsoleTasks(HOST, [info({ state: "done" }), info({ id: "bb", kind: "pkg_install", cancellable: false })]);
    expect(taskCapabilities(rows()[1]).canCancel).toBe(false);
  });

  it("asks the console, then reads how it ended", async () => {
    applyConsoleTasks(HOST, [info()]);
    const c = vi.spyOn(api, "cancelConsoleTask").mockResolvedValue();
    vi.spyOn(api, "fetchConsoleTasks").mockResolvedValue({
      supported: true,
      tasks: [info({ state: "cancelled", age_s: 0 })],
    });
    expect(await commandTask(rows()[0], "cancel")).toBe(true);
    expect(c).toHaveBeenCalledWith(HOST, "aa");
    expect(rows()[0].status).toBe("cancelled");
  });

  it.each(["unknown_job", "ended"])("treats a task that is already over (%s) as no failure", async (reason) => {
    vi.spyOn(api, "cancelConsoleTask").mockRejectedValue(new ConsoleTaskError("gone", reason));
    vi.spyOn(api, "fetchConsoleTasks").mockResolvedValue({ supported: true, tasks: [] });
    await expect(cancelConsoleTaskRow(HOST, "aa")).resolves.toBeUndefined();
  });

  it("says so when the console refuses, and keeps the row running", async () => {
    applyConsoleTasks(HOST, [info()]);
    vi.spyOn(api, "cancelConsoleTask").mockRejectedValue(new ConsoleTaskError("nope", "failed"));
    vi.spyOn(api, "fetchConsoleTasks").mockResolvedValue({ supported: true, tasks: [info()] });
    expect(await commandTask(rows()[0], "cancel")).toBe(false);
    expect(rows()[0].status).toBe("running");
  });
});
