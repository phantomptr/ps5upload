import { beforeEach, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/invokeLogged", () => ({ invoke }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => false }));
const store = vi.hoisted(() => new Map<string, string>());
vi.mock("../lib/safeStorage", () => ({
  safeGetItem: (k: string) => store.get(k) ?? null,
  safeSetItem: (k: string, v: string) => void store.set(k, v),
  safeRemoveItem: (k: string) => void store.delete(k),
}));

import { loadConsolePayload, sendPayloadChecked } from "./ps5";
import { loadKernelSends } from "../lib/autoLoaderGuard";

beforeEach(() => invoke.mockReset());

// `payload_send_check` is a desktop command. The web UI has no such command, and its send is
// checked by the engine that makes it.
it("the web UI sends a payload without the desktop-only question first", async () => {
  invoke.mockResolvedValue({ ok: true, status: "sent" });
  await expect(sendPayloadChecked("10.0.0.2", "/cache/etaHEN.elf")).resolves.toEqual({
    viaPayloadManager: false,
    unstamped: false,
  });
  expect(invoke.mock.calls.map(([cmd]) => cmd)).toEqual(["payload_send"]);
});

// A kernel patcher sent by hand, from this computer or from Files on the PS5, is remembered so
// the auto-run playlist never sends it a second time in that console boot.
it("a kernel patcher sent by hand is remembered for the auto-run guard", async () => {
  invoke.mockResolvedValue({ ok: true, status: "sent", bytes: 10 });
  await sendPayloadChecked("10.0.0.3", "/cache/kstuff.elf");
  expect(loadKernelSends("10.0.0.3").kstuff).toBeTypeOf("number");
  await loadConsolePayload("10.0.0.4", "/data/payloads/etaHEN.bin");
  expect(loadKernelSends("10.0.0.4").etahen).toBeTypeOf("number");
  await loadConsolePayload("10.0.0.5", "/data/payloads/ftpsrv.elf");
  expect(loadKernelSends("10.0.0.5")).toEqual({});
});
