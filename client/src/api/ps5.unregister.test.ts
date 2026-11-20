import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { appUnregister } from "./ps5";
import { humanizePs5Error } from "../lib/humanizeError";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

const mockedInvoke = vi.mocked(invoke);

// #476: the PS5 refused (or did not finish) an uninstall, the app dropped the game from its
// list anyway, and the next refresh brought it back.
describe("appUnregister results", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });

  it("resolves for a finished uninstall and sends the named console", async () => {
    mockedInvoke.mockResolvedValue({
      ok: true,
      sony_uninstall_refused: false,
      sony_uninstall_rc: "0x00000000",
      still_present: false,
    });
    await expect(appUnregister("192.0.2.8:9113", "CUSA33878")).resolves.toMatchObject({
      stillPresent: false,
    });
    expect(mockedInvoke).toHaveBeenCalledWith("ps5_app_unregister", {
      req: { addr: "192.0.2.8", title_id: "CUSA33878" },
    });
  });

  it("rejects a refusal an older helper acknowledged", async () => {
    mockedInvoke.mockResolvedValue({
      ok: true,
      sony_uninstall_refused: true,
      sony_uninstall_rc: "0x80B21B02",
    });
    await expect(appUnregister("192.0.2.8", "CUSA33878")).rejects.toThrow(
      "uninstall_refused:0x80B21B02",
    );
  });

  it("passes a 7.1.6 helper's refusal error through", async () => {
    mockedInvoke.mockRejectedValue(
      new Error("payload rejected APP_UNREGISTER: uninstall_refused:0x80B21B02"),
    );
    await expect(appUnregister("192.0.2.8", "CUSA33878")).rejects.toThrow(
      "uninstall_refused:0x80B21B02",
    );
  });

  it("accepts our folder registration removed although Sony refused", async () => {
    mockedInvoke.mockResolvedValue({
      ok: true,
      sony_uninstall_refused: true,
      sony_uninstall_rc: "0x80B21B02",
      still_present: false,
    });
    await expect(appUnregister("192.0.2.8", "PPSA01234")).resolves.toMatchObject({
      stillPresent: false,
    });
  });

  it("reports a title still on the console instead of claiming it is gone", async () => {
    mockedInvoke.mockResolvedValue({
      ok: true,
      sony_uninstall_refused: false,
      sony_uninstall_rc: "0x00000000",
      still_present: true,
    });
    await expect(appUnregister("192.0.2.8", "CUSA33878")).resolves.toMatchObject({
      stillPresent: true,
    });
  });

  it("keeps working with an older helper that reports nothing", async () => {
    mockedInvoke.mockResolvedValue({
      ok: true,
      sony_uninstall_refused: false,
      sony_uninstall_rc: "0x00000000",
      still_present: null,
    });
    await expect(appUnregister("192.0.2.8", "CUSA33878")).resolves.toMatchObject({
      stillPresent: false,
    });
  });

  // #879: a ShadowMount+ title comes back on its next scan; an unread app.db is no "gone".
  it("says when ShadowMount+ will add the title back, and from which image", async () => {
    mockedInvoke.mockResolvedValue({
      ok: true,
      sony_uninstall_refused: false,
      sony_uninstall_rc: "0x00000000",
      still_present: false,
      unverified: false,
      managed_by: "shadowmount",
      image: "/data/homebrew/DOOM.exfat",
    });
    await expect(appUnregister("192.0.2.8", "PPSA01234")).resolves.toEqual({
      stillPresent: false,
      unverified: false,
      shadowMount: true,
      image: "/data/homebrew/DOOM.exfat",
    });
  });

  it("reports an unchecked removal as not confirmed", async () => {
    mockedInvoke.mockResolvedValue({
      ok: true,
      sony_uninstall_refused: false,
      sony_uninstall_rc: "0x00000000",
      still_present: true,
      unverified: true,
      managed_by: null,
      image: null,
    });
    await expect(appUnregister("192.0.2.8", "PPSA01234")).resolves.toEqual({
      stillPresent: true,
      unverified: true,
      shadowMount: false,
      image: null,
    });
  });

  it("rejects an ok:false reply", async () => {
    mockedInvoke.mockResolvedValue({ ok: false, error: "unregister_unmount_failed" });
    await expect(appUnregister("192.0.2.8", "CUSA33878")).rejects.toThrow(
      "unregister_unmount_failed",
    );
  });
});

describe("uninstall errors in plain words", () => {
  it("names the console's code for a refusal", () => {
    const msg = humanizePs5Error(
      "payload rejected APP_UNREGISTER: uninstall_refused:0x80B21B02",
    );
    expect(msg).toContain("0x80B21B02");
    expect(msg).toContain("still installed");
  });

  it("explains a firmware without an uninstall function", () => {
    expect(humanizePs5Error("payload rejected APP_UNREGISTER: uninstall_unavailable")).toContain(
      "still installed",
    );
  });
});
