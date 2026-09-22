import { beforeEach, describe, expect, it, vi } from "vitest";

// Same module-load stubs as pkgLibrary.reverify.test.ts: importing the store
// pulls in the Tauri bridge and the ps5 api.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
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
  pkgInstallPreflight: vi.fn(async () => null),
}));
vi.mock("../lib/ps5Transfers", () => ({ transferScreenBusy: () => false }));

import { invoke } from "@tauri-apps/api/core";
import { pkgLibraryStore } from "./pkgLibrary";
import { useLinkInstallPrefs } from "./linkInstallPrefs";

const HOST = "10.0.0.5:9114";
const URL_OK = "https://h.example/game.pkg";

describe("link install modes", () => {
  const mockedInvoke = vi.mocked(invoke);

  beforeEach(() => {
    vi.clearAllMocks();
    (globalThis as { window?: unknown }).window = {
      localStorage: {
        getItem: () => null,
        setItem: () => {},
        removeItem: () => {},
        clear: () => {},
      },
    };
    useLinkInstallPrefs.setState({ modes: {}, insecure: {} });
  });

  /* Direct hands the URL to the console's own installer and never opens the
   * engine's proxy — that is the entire point: the computer can then sleep. */
  it("direct mode asks the PS5 to fetch the url itself", async () => {
    mockedInvoke.mockResolvedValue({ ok: true, rc: 0 });
    const r = await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "direct" });
    const call = mockedInvoke.mock.calls.find((c) => c[0] === "pkg_dpi_install");
    expect(call, "expected a pkg_dpi_install call").toBeTruthy();
    expect((call?.[1] as { localPs5Path: string }).localPs5Path).toBe(URL_OK);
    expect(r.ok).toBe(true);
  });

  /* A refusal must not dead-end the user: the streaming path needs neither
   * the DPI daemon nor a console-reachable URL. */
  it("falls back to this computer when the PS5 cannot fetch it", async () => {
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "pkg_dpi_install") throw new Error("dpi daemon unreachable");
      throw new Error("probe not stubbed");
    });
    await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "direct" });
    // It tried direct, then moved on rather than returning the DPI error.
    expect(
      mockedInvoke.mock.calls.some((c) => c[0] === "pkg_dpi_install"),
    ).toBe(true);
    expect(
      mockedInvoke.mock.calls.some((c) => c[0] === "pkg_remote_probe"),
    ).toBe(true);
  });

  /* Accelerated must never reach for the console's installer. */
  it("stream mode does not call the DPI daemon", async () => {
    mockedInvoke.mockImplementation(async () => {
      throw new Error("probe not stubbed");
    });
    await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "stream" });
    expect(
      mockedInvoke.mock.calls.some((c) => c[0] === "pkg_dpi_install"),
    ).toBe(false);
  });

  /* With no explicit mode the stored per-host choice decides. */
  it("uses the remembered choice when no mode is passed", async () => {
    useLinkInstallPrefs.getState().setMode(HOST, "stream");
    mockedInvoke.mockImplementation(async () => {
      throw new Error("probe not stubbed");
    });
    await pkgLibraryStore(HOST).getState().installUrl(URL_OK, HOST);
    expect(
      mockedInvoke.mock.calls.some((c) => c[0] === "pkg_dpi_install"),
    ).toBe(false);
  });
});
