import { describe, it, expect, vi } from "vitest";

// Same helper, but running the way the self-hosted web UI does: outside
// Tauri. The native sleep inhibitor has no browser equivalent, so calling it
// there fails — harmlessly, but loudly enough to seed a warning into every
// bug report captured during an upload. It should not be called at all.
const calls: Array<{ cmd: string; args: unknown }> = [];
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => {
    calls.push({ cmd, args });
    return Promise.resolve();
  },
}));
vi.mock("./tauriEnv", () => ({ isTauriEnv: () => false }));

import { setTransferKeepAwake } from "./keepAwakeHold";

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("setTransferKeepAwake in a browser", () => {
  it("makes no native call", async () => {
    setTransferKeepAwake(true);
    await flush();
    setTransferKeepAwake(false);
    await flush();
    expect(calls).toEqual([]);
  });
});
