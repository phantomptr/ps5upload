import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import type { InstallStatus } from "../api/ps5";
import { recoveryResendsHelper, isAutoRecoverable } from "../lib/uploadRecovery";
import { isTransientRefusal, pkgRowInstalled, sampleFromStatus, statusToOutcome } from "./pkgLibrary";
import { unpackFolderName, archiveSpaceCheck, isArchiveFolderName } from "./rarPackages";

function status(p: Partial<InstallStatus>): InstallStatus {
  return {
    job: "j",
    ps5_addr: "1.2.3.4",
    content_id: "",
    title_id: null,
    phase: "failed",
    route: "stream",
    verdict: "failed",
    code: 0,
    hint: null,
    reason: null,
    metrics: { total_bytes: 10, served_bytes: 0, throughput_mbps: 0, phase_ms: {}, retries: 0, sony_rc: 0 },
    app_ver_before: null,
    app_ver_after: null,
    patch_verdict: null,
    shortened: false,
    started_at: 0,
    updated_at: 0,
    ...p,
  };
}

describe("what a finished install says", () => {
  it("a Stop is not a failure", () => {
    const o = statusToOutcome(status({ reason: "cancelled" }));
    expect(o.stopped).toBe(true);
    expect(o.installed).toBe(false);
  });

  it("an engine that forgot the job says the PS5 may still be installing", () => {
    const o = statusToOutcome(status({ phase: "done", verdict: "unverified", reason: "job_lost" }));
    expect(o.unverified).toBe(true);
    expect(o.errMessage).toMatch(/restarted/);
  });

  it("a package with all its bytes delivered is not talked about as a link", () => {
    const full = status({
      phase: "done",
      verdict: "unverified",
      metrics: { total_bytes: 10, served_bytes: 10, throughput_mbps: 0, phase_ms: { verify: 120000 }, retries: 0, sony_rc: 0 },
    });
    expect(statusToOutcome(full).errMessage).toMatch(/whole package/);
    expect(statusToOutcome(status({ phase: "done", verdict: "unverified" })).errMessage).toMatch(/took the link/);
  });

  it("says which thing a reinstall would replace", () => {
    expect(statusToOutcome(status({ reason: "destructive_guard", replaces: "update" })).errMessage).toMatch(/undo/);
    expect(statusToOutcome(status({ reason: "destructive_guard", replaces: "game" })).errMessage).toMatch(/removes the installed copy/);
  });

  it("engine failures name themselves, in the reason's own words", () => {
    for (const [reason, text] of [
      ["busy", /Another install/],
      ["base_missing", /base game/],
      ["image_mounted", /image/],
      ["reply_lost", /never came back/],
      ["loader_unreachable", /rest mode/],
    ] as const) {
      expect(statusToOutcome(status({ reason, hint: "raw engine text" })).errMessage).toMatch(text);
    }
  });

  it("only the busy code with nothing fetched is retried", () => {
    expect(isTransientRefusal(status({ code: 0x80020002 }))).toBe(true);
    expect(
      isTransientRefusal(
        status({
          code: 0x80020002,
          metrics: { total_bytes: 10, served_bytes: 1, throughput_mbps: 0, phase_ms: {}, retries: 0, sony_rc: 0 },
        }),
      ),
    ).toBe(false);
    expect(isTransientRefusal(status({ code: 0x80b2116f }))).toBe(false);
  });

  it("a console reading its own copy is not shown as a stream, nor is the engine's own check", () => {
    expect(sampleFromStatus(status({ phase: "resolve" })).phase).toBe("queued");
    expect(sampleFromStatus(status({ phase: "deliver", route: "loopback" })).phase).toBe("install");
    expect(sampleFromStatus(status({ phase: "deliver", route: "stream" })).phase).toBe("download");
  });
});

describe("which row reads as installed", () => {
  const art = (kind: "base" | "patch" | "dlc", p: Partial<{ size: number; fingerprint: string; contentId: string }> = {}) => ({
    kind,
    size: 100,
    fingerprint: "a".repeat(64),
    contentId: "UP0000-PPSA00001_00-X",
    ...p,
  });

  it("a PS5 update that carries the whole app counts by its exact bytes", () => {
    expect(
      pkgRowInstalled(
        { titleId: "PPSA00001", category: "gp", fingerprint: "a".repeat(64), size: 100 },
        new Set(["PPSA00001"]),
        [art("base")],
      ),
    ).toBe(true);
  });

  it("a PS5 debug package counts by its content id on the same kind of file", () => {
    expect(
      pkgRowInstalled(
        { titleId: "PPSA00001", category: "ac", contentId: "UP0000-PPSA00001_00-X", fingerprint: "b".repeat(64), size: 999 },
        new Set(["PPSA00001"]),
        [art("dlc", { fingerprint: "c".repeat(64) })],
      ),
    ).toBe(true);
  });

  it("a PS5 add-on this app installed stays installed when the inventory has none to compare", () => {
    const row = { titleId: "PPSA00001", category: "gp", installedHere: true } as const;
    expect(pkgRowInstalled(row, new Set(["PPSA00001"]), [art("base", { fingerprint: "d".repeat(64) })])).toBe(true);
    // The game itself is gone: nothing can be applied to it.
    expect(pkgRowInstalled(row, new Set(), [])).toBe(false);
  });

  it("a PS4 update is still decided by the inventory", () => {
    expect(
      pkgRowInstalled(
        { titleId: "CUSA00001", category: "gp", installedHere: true, fingerprint: "b".repeat(64) },
        new Set(["CUSA00001"]),
        [art("base", { contentId: "UP0000-CUSA00001_00-X", fingerprint: "d".repeat(64) })],
      ),
    ).toBe(false);
  });
});

describe("recovering a failed upload", () => {
  it("what is not about the transport is not retried", () => {
    for (const reason of ["source_missing", "source_names_made_up", "ava1_file_too_large"]) {
      expect(isAutoRecoverable(reason, "x", { canSendHelper: true })).toBe(false);
    }
    expect(isAutoRecoverable(null, "cancelled", { canSendHelper: true })).toBe(false);
    expect(isAutoRecoverable(null, "rar_password_required", { canSendHelper: true })).toBe(false);
  });

  it("only a failure that can be the helper's sends it again", () => {
    expect(recoveryResendsHelper(null)).toBe(true);
    expect(recoveryResendsHelper("ava1_unreachable")).toBe(true);
    expect(recoveryResendsHelper("source_read_failed")).toBe(false);
    expect(recoveryResendsHelper("ava1_refused_12")).toBe(false);
    expect(recoveryResendsHelper("ava1_stalled")).toBe(false);
  });
});

describe("archive folders and room", () => {
  it("keeps letters of every script, so two archives do not share a folder", () => {
    const a = unpackFolderName("/x/游戏一.zip");
    const b = unpackFolderName("/x/游戏二.zip");
    expect(a).not.toBe(b);
    expect(isArchiveFolderName(a)).toBe(true);
    expect(unpackFolderName("/x/a b.part1.rar")).toBe("rar_a_b");
  });

  it("warns, but does not refuse, when the install also needs its room on internal storage", () => {
    const vol = { path: "/user", free_bytes: 150, total_bytes: 1000, writable: true } as never;
    const space = archiveSpaceCheck([vol], "/user/data/pkg", 100);
    expect(space.kind === "no_room").toBe(false);
    expect(space.need).toBe(100);
  });
});
