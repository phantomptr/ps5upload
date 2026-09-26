import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  backupSnapshot,
  backupRestore,
  userCreate,
  cheatsReload,
  remoteplayRequest,
  remoteplayCancel,
  fwSpoofStatus,
  profileInfo,
} from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

const mockedInvoke = vi.mocked(invoke);

/**
 * The payload reports a refused action as `ok:false` inside an otherwise
 * successful HTTP 200, so `await` alone never surfaces it. That is how
 * "Start FTP Server" (a screen since retired) could fail with `bind_failed` and leave the screen
 * looking like nothing had happened.
 *
 * Action wrappers must therefore reject, the way `sendPayload` already
 * does. Status wrappers must NOT — for those `ok:false` means "this
 * console doesn't expose the feature", which the screens render as an
 * empty state rather than an error.
 */
describe("ok:false guards on action endpoints", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });

  it("rejects a refused backup snapshot instead of reporting success", async () => {
    mockedInvoke.mockResolvedValue({ ok: false, error: "store_failed" });
    await expect(backupSnapshot("tag", "/data/x")).rejects.toThrow(/store_failed/);
  });

  it("rejects a refused backup restore", async () => {
    mockedInvoke.mockResolvedValue({ ok: false, error: "not_cached" });
    await expect(backupRestore("tag", 1786320000)).rejects.toThrow(/not_cached/);
  });

  it("rejects a refused user creation", async () => {
    mockedInvoke.mockResolvedValue({ ok: false, error: "invalid_name" });
    await expect(userCreate("player")).rejects.toThrow(/invalid_name/);
  });

  it("rejects a refused cheats reload", async () => {
    mockedInvoke.mockResolvedValue({ ok: false, error: "oom" });
    await expect(cheatsReload()).rejects.toThrow(/oom/);
  });

  it("rejects a refused remote-play request", async () => {
    mockedInvoke.mockResolvedValue({ ok: false, error: "busy" });
    await expect(remoteplayRequest()).rejects.toThrow(/busy/);
  });

  it("rejects a refused remote-play cancel", async () => {
    mockedInvoke.mockResolvedValue({ ok: false, error: "not_running" });
    await expect(remoteplayCancel()).rejects.toThrow(/not_running/);
  });

  it("falls back to a readable message when no error string is given", async () => {
    mockedInvoke.mockResolvedValue({ ok: false });
    await expect(cheatsReload()).rejects.toThrow(/failed/i);
  });
});

describe("status endpoints keep reporting ok:false as data", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });

  it("does not throw when firmware spoofing is unsupported", async () => {
    mockedInvoke.mockResolvedValue({ ok: false, spoofed: false });
    await expect(fwSpoofStatus()).resolves.toMatchObject({ ok: false });
  });

  it("does not throw when no profile is available", async () => {
    mockedInvoke.mockResolvedValue({ ok: false });
    await expect(profileInfo()).resolves.toMatchObject({ ok: false });
  });
});
