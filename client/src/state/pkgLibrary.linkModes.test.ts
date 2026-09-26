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
import { useTaskStore } from "./tasks";

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
      if (cmd === "dpi_ensure") return { ok: true, listening: true, sent: false };
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

  /* The daemon that does a direct install is not always running — it was
   * down on the test console until something started it. Direct mode used to
   * skip starting it and fail whenever :9040 was closed. */
  it("starts the DPI daemon before asking the PS5 to fetch the link", async () => {
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "dpi_ensure") return { ok: true, listening: true, sent: true };
      if (cmd === "pkg_dpi_install") return { ok: true, rc: 0 };
      return {};
    });
    await pkgLibraryStore(HOST).getState().installUrl(URL_OK, HOST, { mode: "direct" });
    const order = mockedInvoke.mock.calls.map((c) => c[0]);
    expect(order.indexOf("dpi_ensure")).toBeGreaterThanOrEqual(0);
    expect(order.indexOf("dpi_ensure")).toBeLessThan(order.indexOf("pkg_dpi_install"));
  });

  /* A refusal arrives as HTTP 200 with ok:false. It used to be read as
   * success — "Sent to the PS5, you can close ps5upload" — while nothing was
   * installing. It must be treated as a failure and fall back instead. */
  it("never reports a refused link as sent", async () => {
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "dpi_ensure") return { ok: true, listening: true, sent: false };
      if (cmd === "pkg_dpi_install")
        return { ok: false, rc: 0x80a30003 | 0, err_message: "refused" };
      throw new Error("probe not stubbed");
    });
    const r = await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "direct" });
    expect(r.message ?? "").not.toMatch(/Sent to the PS5/);
    expect(mockedInvoke.mock.calls.some((c) => c[0] === "pkg_remote_probe")).toBe(true);
  });

  /* A link too long for the installer goes through a short alias on this
   * computer, which the console keeps re-resolving — so this is the one
   * direct install where closing the app would break it. Say so. */
  it("tells the user to keep the app running when the link was shortened", async () => {
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "dpi_ensure") return { ok: true, listening: true, sent: false };
      if (cmd === "pkg_dpi_install") return { ok: true, rc: 0, shortened: true };
      return {};
    });
    const r = await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "direct" });
    expect(r.ok).toBe(true);
    expect(r.message).toMatch(/keep ps5upload running/);
    expect(r.message).not.toMatch(/You can close ps5upload/);
  });

  /* A package on a saved server streams from the engine by its remote path; the
   * task shows it by the server's name and holds nothing about the connection. */
  it("streams a package from a saved server by its remote path", async () => {
    const { useConnectionsStore } = await import("./connections");
    useConnectionsStore.setState({
      connections: [
        {
          id: "nas-1",
          name: "NAS",
          protocol: "smb",
          host: "10.0.0.9",
          port: 445,
          share: "g",
          user: "me",
          start_path: "",
          host_key: null,
          has_secret: true,
        },
      ],
    });
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "pkg_install_start") return { session_id: "s1", err_code: 0 };
      throw new Error("stop here");
    });
    useTaskStore.setState({ tasks: [] });
    await pkgLibraryStore(HOST).getState().installStream("remote://nas-1/games/a.pkg", HOST);
    const call = mockedInvoke.mock.calls.find((c) => c[0] === "pkg_install_start");
    const args = call?.[1] as Record<string, unknown>;
    expect(args.path).toBe("remote://nas-1/games/a.pkg");
    expect("smb" in args).toBe(false);
    const task = useTaskStore.getState().tasks[0];
    expect(task.label).toContain("a.pkg");
    expect(JSON.stringify(task.payload)).toContain("NAS › games/a.pkg");
    expect(JSON.stringify(task.payload)).not.toContain("10.0.0.9");
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
