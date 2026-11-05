import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

import { onNotPaired } from "../lib/consoleSession";
import { jobStatus, pkgInstall } from "./ps5";

const mockedInvoke = vi.mocked(invoke);

beforeEach(() => mockedInvoke.mockReset());

describe("a failed job opens the pairing dialog for the console that failed", () => {
  it("uses the console the engine names, not the one the job is polled against", async () => {
    // A PS5 to PS5 copy is polled with the destination, but the source is the unpaired one.
    mockedInvoke.mockResolvedValue({
      status: "failed",
      error: "not paired",
      error_reason: "ava1_not_paired",
      error_console: "10.0.0.2",
    });
    const heard: (string | undefined)[] = [];
    const off = onNotPaired((h) => heard.push(h));
    await jobStatus("j1", "10.0.0.3");
    off();
    expect(heard).toEqual(["10.0.0.2"]);
  });

  it("falls back to the polled console when the failure names none", async () => {
    mockedInvoke.mockResolvedValue({
      status: "failed",
      error: "not paired",
      error_reason: "ava1_not_paired",
    });
    const heard: (string | undefined)[] = [];
    const off = onNotPaired((h) => heard.push(h));
    await jobStatus("j1", "10.0.0.3");
    off();
    expect(heard).toEqual(["10.0.0.3"]);
  });
});

describe("a job refused for lack of room", () => {
  it("is worded here, in the user's language, not in the engine's English", async () => {
    mockedInvoke.mockResolvedValue({
      status: "failed",
      error: "/data/x needs 2048 more bytes (0.0 MiB) for this upload but only 1024 bytes",
      error_detail: "/data/x needs 2048 more bytes (0.0 MiB) for this upload but only 1024 bytes",
      error_reason: "preflight_insufficient_space",
      error_space: {
        volume: "/data/x",
        need_bytes: 2048,
        avail_bytes: 1024,
        free_bytes: 3072,
        reserve_bytes: 2048,
        promised_bytes: 0,
        reused_bytes: 0,
        image: false,
        late: false,
      },
    });
    const snap = await jobStatus("j1", "10.0.0.3");
    expect(snap.error).toContain("1.00 KiB can be used");
    expect(snap.error_detail).toBe(snap.error);
    expect(snap.error_reason).toBe("preflight_insufficient_space");
  });

  it("keeps the engine's text for a failure with no figures", async () => {
    mockedInvoke.mockResolvedValue({ status: "failed", error: "disk full", error_detail: "x" });
    const snap = await jobStatus("j1", "10.0.0.3");
    expect(snap.error).toBe("disk full");
    expect(snap.error_detail).toBe("x");
  });
});

describe("starting an install on a console that already has one", () => {
  const req = { ps5_addr: "10.0.0.3", source: { url: "http://h/x.pkg" }, content_id: "" };

  it("reads the engine's 409 as busy, from either transport", async () => {
    for (const message of ["engine HTTP 409 Conflict: busy", "busy"]) {
      mockedInvoke.mockRejectedValueOnce(new Error(message));
      await expect(pkgInstall(req)).resolves.toEqual({ ok: false, error: "busy" });
    }
    // Tauri rejects with a plain string.
    mockedInvoke.mockRejectedValueOnce("engine HTTP 409 Conflict: busy");
    await expect(pkgInstall(req)).resolves.toEqual({ ok: false, error: "busy" });
  });

  it("still reads an older engine's 200", async () => {
    mockedInvoke.mockResolvedValue({ ok: false, error: "busy", job: "1-1" });
    await expect(pkgInstall(req)).resolves.toMatchObject({ ok: false, error: "busy" });
  });

  it("lets any other failure through", async () => {
    mockedInvoke.mockRejectedValueOnce(new Error("engine HTTP 400: ps5_addr is required"));
    await expect(pkgInstall(req)).rejects.toThrow("ps5_addr is required");
    mockedInvoke.mockRejectedValueOnce(new Error("the console is busy right now"));
    await expect(pkgInstall(req)).rejects.toThrow("busy right now");
  });
});
