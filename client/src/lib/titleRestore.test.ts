import { describe, expect, it } from "vitest";

import type { RestoreReply } from "../api/titleRestore";
import { humanizePs5Error } from "./humanizeError";
import { installErrorDoc, DOC_ANCHORS } from "./installErrorDoc";
import {
  restoreConfirmText,
  restoreEnded,
  restoreErrorText,
  normalizeRestoreReply,
  normalizeRestoreRun,
  restoreNeeded,
  restoreOutcome,
  restoreRollbackNeedsClear,
  restoreStateText,
} from "./titleRestore";

const reply = (state: string, extra: Partial<RestoreReply> = {}): RestoreReply => ({
  ok: true,
  title_id: "PPSA01234",
  state,
  ...extra,
});

describe("Restore to home screen (#961)", () => {
  it("replaces Play only for a game on a drive that the home screen does not list", () => {
    for (const s of ["restorable", "pending", "no_room", "retail", "aside_cross_device", "ambiguous"]) {
      expect(restoreNeeded(reply(s)), s).toBe(true);
    }
    for (const s of ["registered", "not_installed", "record_elsewhere", "folder_registration", "db_unreadable", "system_title"]) {
      expect(restoreNeeded(reply(s)), s).toBe(false);
    }
    expect(restoreNeeded(null)).toBe(false);
    // A refusal from prepare is not a check answer.
    expect(restoreNeeded({ ...reply("restorable"), ok: false })).toBe(false);
  });

  it("names where the game is and why it can or can't come back", () => {
    expect(restoreStateText(reply("restorable", { where: "m2" }), "Astro")).toContain("the M.2 drive");
    expect(restoreStateText(reply("restorable", { where: "m2" }), "Astro")).toContain("nothing is downloaded");
    expect(restoreStateText(reply("pending", { aside: "/mnt/ext1/ps5upload/restore/PPSA01234" }), "Astro")).toContain(
      "/mnt/ext1/ps5upload/restore/PPSA01234",
    );
    expect(restoreStateText(reply("no_room", { size: 2 * 1024 ** 3 }), "Astro")).toContain("2");
    expect(restoreStateText(reply("retail"), "Astro")).toContain("download it again");
    expect(restoreStateText(reply("registered"), "Astro")).toBeNull();
    expect(restoreStateText(null, "Astro")).toBeNull();
  });

  it("says what a restore does before it starts, the update included", () => {
    const text = restoreConfirmText(reply("restorable", { where: "usb", size: 1024 ** 3 }), "Astro");
    expect(text).toContain("USB extended storage");
    expect(text).toContain("Nothing is downloaded");
    expect(text).toContain("put back where it was");
    expect(text).not.toContain("update (");
    const withPatch = restoreConfirmText(
      reply("restorable", { where: "m2", size: 10, patch: "/mnt/ext1/user/patch/PPSA01234/patch.pkg", patch_size: 5 }),
      "Astro",
    );
    expect(withPatch).toContain("Its update");
  });

  it("tells each ending apart", () => {
    expect(restoreEnded("installing")).toBe(false);
    expect(restoreEnded("queued")).toBe(false);
    expect(restoreEnded("finishing")).toBe(false);
    for (const p of ["restored", "rolled_back", "pending", "failed"]) expect(restoreEnded(p)).toBe(true);

    expect(restoreOutcome({ phase: "restored" }, "Astro")).toEqual({
      level: "success",
      body: "Astro is back on the home screen.",
    });
    const kept = restoreOutcome(
      { phase: "restored", result: reply("restored", { patch_kept: "/x/patch.pkg" }) },
      "Astro",
    );
    expect(kept.body).toContain("/x/patch.pkg");
    const back = restoreOutcome({ phase: "rolled_back", install_hint: "the PS5 refused it (0x80B2116F)." }, "Astro");
    expect(back.level).toBe("error");
    expect(back.body).toContain("0x80B2116F");
    expect(back.body).toContain("back where it was");
    expect(restoreOutcome({ phase: "pending" }, "Astro").level).toBe("info");
    expect(restoreOutcome({ phase: "failed", error: "restore_unsupported: old" }, "Astro").body).toContain(
      "older version",
    );
  });

  it("says a failed install was removed from the PS5, and why a restore is still pending", () => {
    const cleared = restoreOutcome(
      { phase: "rolled_back", cleared: true, install_hint: "the console stopped asking." },
      "Astro",
    );
    expect(cleared.body).toContain("unfinished copy was removed");
    expect(cleared.body).toContain("back where it was");
    const fromResult = restoreOutcome({ phase: "rolled_back", result: reply("rolled_back", { cleared: true }) }, "Astro");
    expect(fromResult.body).toContain("unfinished copy was removed");
    const why = (reason: string) =>
      restoreOutcome({ phase: "pending", result: reply("unsettled", { reason }) }, "Astro").body;
    expect(why("clearing")).toContain("still removing");
    expect(why("update_in_place")).toContain("update");
    expect(why("addons_present")).toContain("add-ons");
    expect(why("uninstall_refused")).toContain("Delete the failed download");
    expect(why("not_on_home_screen")).toContain("has not finished");
    const stuck = restoreOutcome(
      { phase: "pending", install_hint: "the PS5's installer stopped answering, so nothing more was tried" },
      "Astro",
    );
    expect(stuck.level).toBe("error");
    expect(stuck.body).toContain("Restart the PS5");
    expect(
      restoreOutcome({ phase: "failed", error: "the PS5's installer stopped answering, so nothing more was tried" }, "Astro")
        .body,
    ).toContain("Restart the PS5");
  });

  it("asks before removing what a failed restore left only when that is what stands in the way", () => {
    for (const reason of ["recorded_without_package", "on_home_screen", "place_taken"]) {
      expect(restoreRollbackNeedsClear(reply("unsettled", { reason })), reason).toBe(true);
    }
    for (const r of [
      reply("rolled_back"),
      reply("unsettled", { reason: "db_unreadable" }),
      reply("unsettled", { reason: "clearing" }),
      reply("nothing_pending"),
    ]) {
      expect(restoreRollbackNeedsClear(r), `${r.state} ${r.reason}`).toBe(false);
    }
    expect(restoreRollbackNeedsClear(null)).toBe(false);
  });

  it("asks the internal drive for what it really needs", () => {
    expect(restoreStateText(reply("no_room", { size: 1024 ** 3, need_volume: 3 * 1024 ** 3 }), "Astro")).toContain("3");
  });

  it("reads any status the engine sends without throwing, and never leaves it running forever", () => {
    // An unknown phase ends (failed, with the word), so the page and the Games banner stop waiting.
    const odd = normalizeRestoreRun({ phase: "clearing", title_id: "PPSA01234" }, "PPSA01234");
    expect(odd.phase).toBe("failed");
    expect(odd.error).toContain("clearing");
    expect(restoreEnded("clearing")).toBe(true);
    expect(restoreEnded(undefined)).toBe(true);
    for (const raw of [null, undefined, 42, "text", [], { phase: 7 }]) {
      const run = normalizeRestoreRun(raw, "PPSA01234");
      expect(run.title_id).toBe("PPSA01234");
      expect(restoreEnded(run.phase)).toBe(true);
      expect(() => restoreOutcome(run, "Astro")).not.toThrow();
    }
    const messy = normalizeRestoreRun(
      {
        phase: "installing",
        job: 5,
        install_hint: { no: "object" },
        error: null,
        result: "not an object",
        prepared: { ok: true, state: "prepared", size: "big", where: 3, cleared: "yes" },
        install: { phase: "deliver", metrics: { total_bytes: Number.NaN, served_bytes: -1 } },
      },
      "PPSA01234",
    );
    expect(messy.phase).toBe("installing");
    expect(messy.job).toBeUndefined();
    expect(messy.install_hint).toBeUndefined();
    expect(messy.result).toBeUndefined();
    expect(messy.prepared).toEqual({ ok: true, title_id: "", state: "prepared" });
    expect(messy.install).toEqual({ phase: "deliver", metrics: { total_bytes: undefined, served_bytes: undefined } });
    expect(normalizeRestoreReply("x")).toBeNull();
    expect(restoreErrorText(undefined as unknown as string, "Astro")).toBe("");
  });

  it("words the engine's refusals", () => {
    expect(restoreErrorText("busy", "Astro")).toContain("installing something else");
    expect(restoreErrorText("restore_registered", "Astro")).toContain("on the home screen already");
    expect(restoreErrorText("restore_backup_failed", "Astro")).toContain("nothing was changed");
    expect(restoreErrorText("restore_move_failed", "Astro")).toContain("nothing was changed");
    expect(restoreErrorText("something else", "Astro")).toBe("something else");
  });

  it("turns the helper's refused launch into the way out, with its FAQ entry", () => {
    const raw = "payload rejected APP_LAUNCH: launch_not_in_database: rc=0x80940005";
    expect(humanizePs5Error(raw)).toContain("Restore to home screen");
    expect(installErrorDoc(raw)).toEqual({ query: DOC_ANCHORS.restoreHomeScreen });
  });
});
