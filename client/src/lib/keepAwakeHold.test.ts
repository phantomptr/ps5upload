import { describe, it, expect, vi } from "vitest";

// Record every IPC call the helper makes. The helper is best-effort and
// fire-and-forget, so the test drives it and then flushes the microtask
// queue (a setTimeout(0) macrotask runs after all pending .then chains).
const calls: Array<{ cmd: string; args: unknown }> = [];
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => {
    calls.push({ cmd, args });
    return Promise.resolve();
  },
}));

import { setTransferKeepAwake } from "./keepAwakeHold";

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("setTransferKeepAwake", () => {
  it("only hits the backend on 0<->1 edges", async () => {
    setTransferKeepAwake(true);
    await flush();
    expect(calls).toEqual([
      { cmd: "keep_awake_acquire", args: { reason: "transfer" } },
    ]);

    // Re-asserting the same state is a no-op — no second acquire.
    setTransferKeepAwake(true);
    await flush();
    expect(calls).toHaveLength(1);

    setTransferKeepAwake(false);
    await flush();
    expect(calls).toEqual([
      { cmd: "keep_awake_acquire", args: { reason: "transfer" } },
      { cmd: "keep_awake_release", args: { reason: "transfer" } },
    ]);
  });

  it("collapses rapid flips to the latest desired state", async () => {
    calls.length = 0; // current state is released (false) after the prior test
    setTransferKeepAwake(true);
    setTransferKeepAwake(false);
    setTransferKeepAwake(true);
    await flush();
    // Net transition false -> true: a single acquire, no release churn,
    // because each queued step reads the shared latest `desired` at run
    // time (true) and the first step already moved current to true.
    expect(calls).toEqual([
      { cmd: "keep_awake_acquire", args: { reason: "transfer" } },
    ]);
  });
});
