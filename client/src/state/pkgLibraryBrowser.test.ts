import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The install cascade in BROWSER (self-hosted engine) mode.
 *
 * `pkgLibrary.test.ts` forces `isTauriEnv() === true`, so every DPI test
 * there exercises the desktop transport. That left the browser path
 * untested — and broken: `dpi_ensure` and the payload restore were
 * desktop-only Tauri commands, so the fallback that installs a game PATCH
 * threw `BrowserUnsupportedError` before it ever reached the daemon. Base
 * games, which land on the in-process tier and never need the fallback,
 * kept working, which is why the report was "updates fail from the web UI
 * but install fine from the Windows app" (#295).
 */
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => false }));
vi.mock("../lib/browserInvoke", () => ({
  BrowserUnsupportedError: class extends Error {},
  browserInvoke: vi.fn(),
}));
vi.mock("../api/ps5", () => ({
  fsListDir: vi.fn(async () => []),
  fsDelete: vi.fn(async () => {}),
  fsMkdir: vi.fn(async () => {}),
  fsCopy: vi.fn(async () => {}),
  fsOpStatus: vi.fn(async () => ({ total_bytes: 0, bytes_copied: 0 })),
  pkgMetadataConsole: vi.fn(async () => null),
  toastPush: vi.fn(async () => ({ ok: true })),
  installFreeBytes: vi.fn(async () => 1_000_000_000_000),
  consoleReadiness: vi.fn(async () => true),
  pkgInstalledInventory: vi.fn(async () => []),
}));
vi.mock("../lib/ps5Transfers", () => ({ transferScreenBusy: () => false }));

import { browserInvoke } from "../lib/browserInvoke";
import { PKG_ACCEPTED_UNVERIFIED_HINT, runPkgInstall } from "./pkgLibrary";

const mockedInvoke = vi.mocked(browserInvoke);

/** The in-process installer rejecting a PS4 patch — the exact FW 10+/11+
 *  authid gate from #152, which is where the DPI fallback takes over. */
function rejectedPatchThenWorkingDpi() {
  mockedInvoke.mockReset();
  mockedInvoke.mockImplementation(async (cmd: string) => {
    if (cmd === "pkg_install_start") {
      return {
        err_code: 0x80b21106,
        err_message: "PS5 installer rejected the pkg",
        register_path: "none",
        package_type: "PS4DP",
      };
    }
    if (cmd === "dpi_ensure") return { ok: true, listening: true, sent: true };
    if (cmd === "pkg_dpi_install") return { ok: true, rc: 0 };
    if (cmd === "payload_restore") return { ok: true, bytes: 2230616 };
    return {};
  });
}

const calledCommands = () => mockedInvoke.mock.calls.map((c) => c[0]);

describe("runPkgInstall over the browser transport", () => {
  beforeEach(() => {
    rejectedPatchThenWorkingDpi();
  });

  it("falls back to the DPI daemon for a rejected patch", async () => {
    const r = await runPkgInstall(
      "192.168.1.50",
      "/user/data/ps5upload/pkg_library/updates/CID.pkg",
      "CID",
      "PS4DP",
      false,
    );

    // The daemon has to be brought up before it can be handed the pkg.
    expect(calledCommands()).toContain("dpi_ensure");
    expect(calledCommands()).toContain("pkg_dpi_install");
    // `ok` from the daemon is only "Sony accepted it" — with no artifact
    // on the console the cascade must report unverified, not success.
    expect(r.installed).toBe(false);
    expect(r.errMessage).toBe(PKG_ACCEPTED_UNVERIFIED_HINT);
  });

  it("restores the helper through the engine, not a desktop-only command", async () => {
    await runPkgInstall(
      "192.168.1.50",
      "/user/data/ps5upload/pkg_library/updates/CID.pkg",
      "CID",
      "PS4DP",
      false,
    );

    // Loading the DPI daemon displaces ps5upload on a single-payload
    // loader. Without a restore the console is left with no helper and
    // the web UI can never reach it again — so this is not optional.
    expect(calledCommands()).toContain("payload_restore");
    expect(mockedInvoke).toHaveBeenCalledWith(
      "payload_restore",
      expect.objectContaining({ ip: "192.168.1.50" }),
    );
    // The desktop-only pair must not be reached from a browser at all.
    expect(calledCommands()).not.toContain("payload_bundled_path");
    expect(calledCommands()).not.toContain("payload_send");
  });

  it("gives update-specific guidance when the engine has no daemon image", async () => {
    // A self-hosted engine built without the payload SDK. The cascade
    // must not surface a raw "unsupported command" string to someone
    // trying to install a game update.
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "pkg_install_start") {
        return {
          err_code: 0x80b21106,
          register_path: "none",
          package_type: "PS4DP",
        };
      }
      if (cmd === "dpi_ensure") {
        return { ok: false, listening: false, sent: false, error: "no image" };
      }
      return {};
    });

    const r = await runPkgInstall(
      "192.168.1.50",
      "/user/data/ps5upload/pkg_library/updates/CID.pkg",
      "CID",
      "PS4DP",
      false,
    );

    expect(r.installed).toBe(false);
    expect(r.errMessage).toContain("update");
    // Nothing was sent to the loader, so nothing needs putting back.
    expect(calledCommands()).not.toContain("payload_restore");
  });
});
