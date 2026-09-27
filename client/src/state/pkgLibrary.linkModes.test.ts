import { beforeEach, describe, expect, it, vi } from "vitest";

// Same module-load stubs as pkgLibrary.reverify.test.ts: importing the store
// pulls in the Tauri bridge and the ps5 api. The install itself now goes
// through the unified `pkgInstall`/`pkgInstallStatus` api (mocked here); only
// the metadata probe still rides the raw `invoke` bridge.
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
  pkgInstall: vi.fn(async () => ({ ok: true, job: "job1" })),
  pkgInstallStatus: vi.fn(async () => ({ phase: "done", verdict: "installed" })),
}));
vi.mock("../lib/ps5Transfers", () => ({ transferScreenBusy: () => false }));

import { invoke } from "@tauri-apps/api/core";
import { pkgLibraryStore } from "./pkgLibrary";
import {
  pkgInstall,
  pkgInstallStatus,
  type InstallSource,
  type InstallStatus,
  type InstallRequestBody,
} from "../api/ps5";
import { useLinkInstallPrefs } from "./linkInstallPrefs";
import { useTaskStore } from "./tasks";

const HOST = "10.0.0.5:9114";
const URL_OK = "https://h.example/game.pkg";

/** A terminal InstallStatus with defaults; override per case. */
function installStatus(over: Partial<InstallStatus>): InstallStatus {
  return {
    job: "job1",
    ps5_addr: HOST,
    content_id: "",
    title_id: null,
    phase: "done",
    route: "stream",
    verdict: "installed",
    code: 0,
    hint: null,
    reason: null,
    metrics: {
      total_bytes: 0,
      served_bytes: 0,
      throughput_mbps: 0,
      phase_ms: {},
      retries: 0,
      sony_rc: 0,
    },
    app_ver_before: null,
    app_ver_after: null,
    patch_verdict: null,
    shortened: false,
    started_at: 0,
    updated_at: 0,
    ...over,
  };
}

const sourcesPosted = (): InstallSource[] =>
  vi
    .mocked(pkgInstall)
    .mock.calls.map((c) => (c[0] as InstallRequestBody).source);

describe("link install modes", () => {
  const mockedInvoke = vi.mocked(invoke);
  const mockedInstall = vi.mocked(pkgInstall);
  const mockedStatus = vi.mocked(pkgInstallStatus);

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
    mockedInstall.mockResolvedValue({ ok: true, job: "job1" });
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "done", verdict: "installed" }),
    );
  });

  /* Direct hands the URL to the console's own installer via the unified
   * endpoint (source: {url}). The engine brings the daemon up internally. */
  it("direct mode asks the PS5 to fetch the url itself", async () => {
    const r = await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "direct" });
    expect(sourcesPosted()).toContainEqual({ url: URL_OK });
    expect(r.ok).toBe(true);
  });

  /* A refusal must not dead-end the user: after the direct attempt fails, the
   * streaming path takes over — which first probes the link. */
  it("falls back to this computer when the PS5 cannot fetch it", async () => {
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "failed", verdict: "failed", hint: "refused" }),
    );
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_remote_probe")
        return { total_size: 10, content_id: "CID", title: "Game" };
      return {};
    });
    await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "direct" });
    // It tried direct (a {url} install), then moved on to the streaming path,
    // which probes the link rather than returning the direct failure.
    expect(sourcesPosted()).toContainEqual({ url: URL_OK });
    expect(
      mockedInvoke.mock.calls.some((c) => c[0] === "pkg_remote_probe"),
    ).toBe(true);
  });

  /* A refusal must be treated as a failure and fall back, never reported as a
   * successful "Sent to the PS5". */
  it("never reports a refused link as sent", async () => {
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "failed", verdict: "failed", hint: "refused" }),
    );
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_remote_probe")
        return { total_size: 10, content_id: "CID", title: "Game" };
      return {};
    });
    const r = await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "direct" });
    expect(r.message ?? "").not.toMatch(/Sent to the PS5/);
    expect(
      mockedInvoke.mock.calls.some((c) => c[0] === "pkg_remote_probe"),
    ).toBe(true);
  });

  /* A link too long for the installer goes through a short alias on this
   * computer, which the console keeps re-resolving — so this is the one direct
   * install where closing the app would break it. Say so. */
  it("tells the user to keep the app running when the link was shortened", async () => {
    mockedStatus.mockResolvedValue(
      installStatus({ phase: "done", verdict: "installed", shortened: true }),
    );
    const r = await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "direct" });
    expect(r.ok).toBe(true);
    expect(r.message).toMatch(/keep ps5upload running/);
    expect(r.message).not.toMatch(/You can close ps5upload/);
  });

  /* A package on a saved server streams from the engine by its remote path; the
   * request carries the connection + path, and the task shows it by the
   * server's name and holds nothing about the connection. */
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
    useTaskStore.setState({ tasks: [] });
    await pkgLibraryStore(HOST)
      .getState()
      .installStream("remote://nas-1/games/a.pkg", HOST);
    expect(sourcesPosted()).toContainEqual({
      remote: { connection: "nas-1", path: "games/a.pkg" },
    });
    const task = useTaskStore.getState().tasks[0];
    expect(task.label).toContain("a.pkg");
    expect(JSON.stringify(task.payload)).toContain("NAS › games/a.pkg");
    expect(JSON.stringify(task.payload)).not.toContain("10.0.0.9");
  });

  /* Stream mode posts a {url} source and lets the engine choose stream
   * delivery — it never hands a console_path or reaches a client daemon. */
  it("stream mode posts a url source", async () => {
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_remote_probe")
        return { total_size: 10, content_id: "CID", title: "Game" };
      return {};
    });
    await pkgLibraryStore(HOST)
      .getState()
      .installUrl(URL_OK, HOST, { mode: "stream" });
    expect(sourcesPosted()).toContainEqual({ url: URL_OK });
  });

  /* With no explicit mode the stored per-host choice decides. Stream probes the
   * link first; direct does not — so the probe proves the remembered choice. */
  it("uses the remembered choice when no mode is passed", async () => {
    useLinkInstallPrefs.getState().setMode(HOST, "stream");
    mockedInvoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "pkg_remote_probe")
        return { total_size: 10, content_id: "CID", title: "Game" };
      return {};
    });
    await pkgLibraryStore(HOST).getState().installUrl(URL_OK, HOST);
    expect(
      mockedInvoke.mock.calls.some((c) => c[0] === "pkg_remote_probe"),
    ).toBe(true);
  });
});
