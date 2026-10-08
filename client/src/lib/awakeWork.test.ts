import { describe, expect, it } from "vitest";

import type { Task } from "../state/tasks";
import { anyAwakeWork, awakeConsoles } from "./awakeWork";

function task(over: Partial<Task>): Task {
  return {
    id: "t",
    kind: "pkg-install",
    origin: "install",
    createdAt: "2026-10-07T00:00:00Z",
    status: "running",
    attempts: 0,
    maxAttempts: 1,
    consoleId: "192.168.86.99",
    ...over,
  } as Task;
}

describe("keeping the computer and the PS5 awake", () => {
  it("counts installs and stream installs, not only uploads", () => {
    expect(anyAwakeWork([task({ kind: "pkg-install" })])).toBe(true);
    expect(anyAwakeWork([task({ kind: "pkg-dpi-install" })])).toBe(true);
    expect(anyAwakeWork([task({ kind: "upload-archive" })])).toBe(true);
    expect(awakeConsoles([task({ kind: "pkg-install" })])).toEqual(
      new Set(["192.168.86.99"]),
    );
  });

  it("holds while queued or waiting on the console, and lets go when done", () => {
    expect(anyAwakeWork([task({ status: "queued" })])).toBe(true);
    expect(anyAwakeWork([task({ status: "awaiting" })])).toBe(true);
    for (const status of ["done", "failed", "cancelled", "paused"] as const) {
      expect(anyAwakeWork([task({ status })])).toBe(false);
    }
  });

  it("ignores quick lookups", () => {
    expect(
      anyAwakeWork([
        task({ kind: "icon-fetch" }),
        task({ kind: "library-launch" }),
      ]),
    ).toBe(false);
  });

  it("keeps every console that has work awake, and a local build only the computer", () => {
    const tasks = [
      task({ consoleId: "192.168.86.99" }),
      task({ kind: "fs-copy", consoleId: "192.168.86.100" }),
      task({ kind: "fpkg-convert", consoleId: "" }),
    ];
    expect(awakeConsoles(tasks)).toEqual(
      new Set(["192.168.86.99", "192.168.86.100"]),
    );
    expect(anyAwakeWork([task({ kind: "fpkg-convert", consoleId: "" })])).toBe(
      true,
    );
  });
});
