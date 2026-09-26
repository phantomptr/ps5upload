import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { tmdbFetch } from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

const mockedInvoke = vi.mocked(invoke);

describe("tmdb API", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });

  it("tmdbFetch sends basic request", async () => {
    mockedInvoke.mockResolvedValueOnce({
      ok: true,
      title_id: "CUSA00001_00",
      name: "Test Game",
    });
    const result = await tmdbFetch("CUSA00001_00", false, "192.168.1.50:9021");
    expect(mockedInvoke).toHaveBeenCalledWith("tmdb_fetch", {
      req: {
        addr: "192.168.1.50:9021",
        title_id: "CUSA00001_00",
        refresh: false,
        region: null,
      },
    });
    expect(result.ok).toBe(true);
    expect(result.name).toBe("Test Game");
  });

  it("tmdbFetch sends region when provided", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true });
    await tmdbFetch("CUSA00001_00", true, undefined, "UP9000");
    expect(mockedInvoke).toHaveBeenCalledWith("tmdb_fetch", {
      req: {
        addr: null,
        title_id: "CUSA00001_00",
        refresh: true,
        region: "UP9000",
      },
    });
  });

  it("tmdbFetch defaults addr and region to null", async () => {
    mockedInvoke.mockResolvedValueOnce({ ok: true });
    await tmdbFetch("CUSA00001_00");
    expect(mockedInvoke).toHaveBeenCalledWith("tmdb_fetch", {
      req: {
        addr: null,
        title_id: "CUSA00001_00",
        refresh: false,
        region: null,
      },
    });
  });

  it("tmdbFetch handles error response", async () => {
    mockedInvoke.mockResolvedValueOnce({
      ok: false,
      error: "not_found",
      title_id: "CUSA99999_00",
    });
    const result = await tmdbFetch("CUSA99999_00");
    expect(result.ok).toBe(false);
    expect(result.error).toBe("not_found");
  });

  it("tmdbFetch handles richer response fields", async () => {
    mockedInvoke.mockResolvedValueOnce({
      ok: true,
      title_id: "CUSA00001_00",
      name: "Elden Ring",
      publisher: "Bandai Namco",
      release_date: "2022-02-25",
      genre: "Action, RPG",
      sku: "ELDENRING001",
    });
    const result = await tmdbFetch("CUSA00001_00");
    expect(result.publisher).toBe("Bandai Namco");
    expect(result.release_date).toBe("2022-02-25");
    expect(result.genre).toBe("Action, RPG");
    expect(result.sku).toBe("ELDENRING001");
  });
});
