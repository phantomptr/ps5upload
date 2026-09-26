import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  cheatsList,
  cheatsGet,
  cheatsToggle,
  cheatsDelete,
  cheatsReload,
  cheatsStatus,
  cheatsEngineSet,
  cheatsReposList,
  cheatsReposSearch,
  cheatsReposDownload,
} from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

const mockedInvoke = vi.mocked(invoke);

describe("cheats API", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });

  it("cheatsList calls correct command", async () => {
    mockedInvoke.mockResolvedValueOnce({
      titles: [
        { title_id: "CUSA00001", name: "Game A", running: false },
      ],
      game_running: false,
      game_title_id: "",
    });
    const result = await cheatsList("192.168.1.50:9021");
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_list", {
      req: { addr: "192.168.1.50:9021" },
    });
    expect(result.titles).toHaveLength(1);
    expect(result.titles[0].title_id).toBe("CUSA00001");
  });

  it("cheatsGet passes title_id", async () => {
    mockedInvoke.mockResolvedValueOnce({
      mods: [
        { index: 0, name: "Inf HP", desc: "", type: "json", on: true },
      ],
    });
    const result = await cheatsGet("CUSA00001", "10.0.0.1:9021");
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_get", {
      req: { addr: "10.0.0.1:9021", title_id: "CUSA00001" },
    });
    expect(result.mods[0].name).toBe("Inf HP");
  });

  it("cheatsToggle sends correct params", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true });
    await cheatsToggle("CUSA00001", 2, false, "192.168.1.50:9021");
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_toggle", {
      req: {
        addr: "192.168.1.50:9021",
        title_id: "CUSA00001",
        index: 2,
        on: false,
      },
    });
  });

  it("cheatsDelete calls correct command", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true });
    await cheatsDelete("CUSA00001");
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_delete", {
      req: { addr: null, title_id: "CUSA00001" },
    });
  });

  it("cheatsReload calls correct command", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true });
    await cheatsReload("192.168.1.50:9021");
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_reload", {
      req: { addr: "192.168.1.50:9021" },
    });
  });

  it("cheatsStatus returns status object", async () => {
    mockedInvoke.mockResolvedValueOnce({
      enabled: true,
      patches_last: 3,
      patches_total: 12,
      game_running: true,
      game_title_id: "CUSA12345",
      game_pid: 98765,
    });
    const result = await cheatsStatus("192.168.1.50:9021");
    expect(result.enabled).toBe(true);
    expect(result.game_pid).toBe(98765);
  });

  it("cheatsEngineSet sends enabled flag", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true, enabled: false });
    const result = await cheatsEngineSet(false, "192.168.1.50:9021");
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_engine_set", {
      req: { addr: "192.168.1.50:9021", enabled: false },
    });
    expect(result.enabled).toBe(false);
  });

  it("cheatsList defaults addr to null", async () => {
    mockedInvoke.mockResolvedValueOnce({ titles: [], game_running: false, game_title_id: "" });
    await cheatsList();
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_list", {
      req: { addr: null },
    });
  });

  it("cheatsReposList calls correct command", async () => {
    mockedInvoke.mockResolvedValueOnce([
      { id: "etahen", name: "etaHEN/PS5_Cheats", raw_base: "https://raw.githubusercontent.com/...", index_files: ["json.txt"] },
    ]);
    const result = await cheatsReposList();
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_repos_list", {});
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("etahen");
  });

  it("cheatsReposSearch passes query", async () => {
    mockedInvoke.mockResolvedValueOnce({
      entries: [
        { filename: "CUSA00001.json", game_title: "Spider-Man", format: "json" },
      ],
    });
    const result = await cheatsReposSearch("spider");
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_repos_search", {
      req: { query: "spider" },
    });
    expect(result.entries).toHaveLength(1);
    expect(result.entries[0].game_title).toBe("Spider-Man");
  });

  it("cheatsReposDownload sends correct params", async () => {
    mockedInvoke.mockResolvedValueOnce({
      ok: true,
      path: "/data/cheats/CUSA00001/cheats.json",
      size: 1234,
    });
    const result = await cheatsReposDownload(
      "etahen",
      "CUSA00001.json",
      "CUSA00001",
      "192.168.1.50:9021",
    );
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_repos_download", {
      req: {
        addr: "192.168.1.50:9021",
        repo_id: "etahen",
        filename: "CUSA00001.json",
        title_id: "CUSA00001",
      },
    });
    expect(result.ok).toBe(true);
    expect(result.size).toBe(1234);
  });

  it("cheatsReposDownload defaults addr to null", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true });
    await cheatsReposDownload("etahen", "file.json", "CUSA00001");
    expect(mockedInvoke).toHaveBeenCalledWith("cheats_repos_download", {
      req: {
        addr: null,
        repo_id: "etahen",
        filename: "file.json",
        title_id: "CUSA00001",
      },
    });
  });
});
