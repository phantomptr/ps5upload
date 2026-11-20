import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

import { appRegister, appUnregister } from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
let onConsole = new Set<string>();
vi.mock("./consoleFiles", () => ({
  fetchConsoleStat: vi.fn(async (_host: string, path: string) => {
    if (onConsole.has(path)) return { kind: "file", size: 1, mtime: 0, mode: 0, dev: 0 };
    throw new Error("fs_stat_errno_2");
  }),
}));

const mockedInvoke = vi.mocked(invoke);
const PARAM = btoa(JSON.stringify({ titleId: "PPSA03595", contentId: "UP0000-PPSA03595_00-X" }));

// #884: Play on a USB dump of Among Us, which the PS5 had installed on M.2, staged the dump on the
// internal drive and asked the PS5's installer to install it: the installed game's record moved
// to Console Storage and it stopped starting. An older helper does whatever it is asked, so the
// app checks too, before it asks.
describe("register and unregister leave the PS5's own install alone", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "fs_read_preview") return { size: 10, base64: PARAM };
      if (cmd === "ps5_app_register") return { title_id: "PPSA03595", title_name: "Among Us", used_nullfs: true };
      if (cmd === "ps5_app_unregister") return { ok: true, still_present: false };
      return null;
    });
    onConsole = new Set();
  });

  it("refuses to register a dump of a game installed on M.2, before asking the helper", async () => {
    onConsole = new Set(["/mnt/ext1/user/app/PPSA03595/app.pkg"]);
    await expect(appRegister("192.0.2.8", "/mnt/usb0/dump/PPSA03595")).rejects.toThrow(
      "register_title_installed:m2",
    );
    await expect(
      appRegister("192.0.2.8", "/mnt/usb0/dump/PPSA03595", { patchDrmType: true }),
    ).rejects.toThrow("register_title_installed:m2");
    expect(mockedInvoke.mock.calls.map((c) => c[0])).not.toContain("ps5_app_register");
  });

  it("registers a folder whose game the PS5 has not installed", async () => {
    await expect(appRegister("192.0.2.8", "/data/homebrew/PPSA03595")).resolves.toMatchObject({
      title_id: "PPSA03595",
    });
    expect(mockedInvoke).toHaveBeenCalledWith("ps5_app_register", expect.anything());
  });

  it("a folder row's unregister refuses the installed game and asks the helper to as well", async () => {
    onConsole = new Set(["/user/app/PPSA03595/app.pkg"]);
    await expect(
      appUnregister("192.0.2.8", "PPSA03595", { registrationOnly: true }),
    ).rejects.toThrow("unregister_title_installed:internal");
    expect(mockedInvoke.mock.calls.map((c) => c[0])).not.toContain("ps5_app_unregister");

    onConsole = new Set();
    await appUnregister("192.0.2.8", "PPSA17221", { registrationOnly: true });
    expect(mockedInvoke).toHaveBeenCalledWith("ps5_app_unregister", {
      req: { addr: "192.0.2.8", title_id: "PPSA17221", registration_only: true },
    });
  });

  it("the game page's Uninstall of an installed game is unchanged", async () => {
    onConsole = new Set(["/user/app/PPSA03595/app.pkg"]);
    await appUnregister("192.0.2.8", "PPSA03595");
    expect(mockedInvoke).toHaveBeenCalledWith("ps5_app_unregister", {
      req: { addr: "192.0.2.8", title_id: "PPSA03595" },
    });
  });
});
