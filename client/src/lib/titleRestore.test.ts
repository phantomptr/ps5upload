import { describe, expect, it } from "vitest";

import type { RestoreReply } from "../api/titleRestore";
import { humanizePs5Error } from "./humanizeError";
import { installErrorDoc, DOC_ANCHORS } from "./installErrorDoc";
import {
  restoreConfirmText,
  restoreEnded,
  restoreErrorText,
  restoreNeeded,
  restoreOutcome,
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
