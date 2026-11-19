import { beforeEach, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/invokeLogged", () => ({ invoke }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => false }));

import { sendPayloadChecked } from "./ps5";

beforeEach(() => invoke.mockReset());

// `payload_send_check` is a desktop command. The web UI has no such command, and its send is
// checked by the engine that makes it.
it("the web UI sends a payload without the desktop-only question first", async () => {
  invoke.mockResolvedValue({ ok: true, status: "sent" });
  await expect(sendPayloadChecked("10.0.0.2", "/cache/etaHEN.elf")).resolves.toEqual({
    viaPayloadManager: false,
  });
  expect(invoke.mock.calls.map(([cmd]) => cmd)).toEqual(["payload_send"]);
});
