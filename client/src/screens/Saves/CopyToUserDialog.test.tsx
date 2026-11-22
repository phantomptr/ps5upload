import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { SaveEntry } from "../../api/ps5";
import { copyStep, CopyToUserView, type CopySource, type CopyState } from "./CopyToUserDialog";
import type { CopyTarget } from "./copyTargets";

const save = { title_id: "PPSA01650", user_id: 2, path: "/p", size: 1, mtime: 1, kind: "ps5" } as SaveEntry;
const ben: CopyTarget = { uid: 2, name: "Ben", save };
const cy: CopyTarget = { uid: 3, name: "Cy", save: null };
const fromAnn: CopySource = { kind: "save", owner: "Ann" };
const noop = () => {};
const view = (state: CopyState, source: CopySource = fromAnn) =>
  renderToStaticMarkup(
    <CopyToUserView
      game="Astro Bot"
      source={source}
      targets={[ben, cy]}
      state={state}
      onPick={noop}
      onConfirm={noop}
      onBack={noop}
      onClose={noop}
    />,
  );

describe("Copy to another user dialog", () => {
  it("walks pick → confirm → busy → done, and back from a failure", () => {
    let s: CopyState = { step: "pick" };
    s = copyStep(s, { type: "pick", target: ben });
    expect(s).toEqual({ step: "confirm", target: ben });
    expect(copyStep(s, { type: "back" })).toEqual({ step: "pick" });
    s = copyStep(s, { type: "start" });
    expect(s.step).toBe("busy");
    // A second start or a back while it runs changes nothing.
    expect(copyStep(s, { type: "start" })).toBe(s);
    expect(copyStep(s, { type: "back" })).toBe(s);
    expect(copyStep(s, { type: "done" })).toEqual({ step: "done", target: ben });
    const failed = copyStep(s, { type: "failed", error: "is running" });
    expect(failed).toEqual({ step: "failed", target: ben, error: "is running" });
    expect(copyStep(failed, { type: "back" })).toEqual({ step: "pick" });
  });

  it("lets a copy go to a user with no save of the game, as theirs", () => {
    const pick: CopyState = { step: "pick" };
    expect(copyStep(pick, { type: "pick", target: cy })).toEqual({ step: "confirm", target: cy });
    const html = view(pick);
    expect(html).toContain("Copy Ann&#x27;s save of Astro Bot to:");
    expect(html).toContain("Has a save of this game");
    expect(html).toContain("No save of this game yet. The copy becomes this user&#x27;s save.");
    expect(html).not.toContain('disabled=""');
    const confirm = view({ step: "confirm", target: cy });
    expect(confirm).toContain("Give Cy Ann&#x27;s save of Astro Bot?");
    expect(confirm).toContain("nothing is replaced");
    expect(confirm).not.toContain("Replace save");
    expect(confirm).toContain(">Copy<");
  });

  it("lets a backup go to a user with no save of the game, as theirs (#809)", () => {
    const backup: CopySource = { kind: "backup", when: "1 Nov" };
    const pick: CopyState = { step: "pick" };
    expect(copyStep(pick, { type: "pick", target: cy })).toEqual({ step: "confirm", target: cy });
    const html = view(pick, backup);
    expect(html).toContain("No save of this game yet. The backup becomes this user&#x27;s save.");
    expect(html).not.toContain('disabled=""');
    const confirm = view({ step: "confirm", target: cy }, backup);
    expect(confirm).toContain("Give Cy the backup of Astro Bot from 1 Nov?");
    expect(confirm).toContain("nothing is replaced");
    expect(confirm).not.toContain("Replace save");
  });

  it("asks before replacing and says what to do when done", () => {
    expect(view({ step: "confirm", target: ben })).toContain(
      "Replace Ben&#x27;s save of Astro Bot with Ann&#x27;s? Ben&#x27;s current save is backed up first.",
    );
    expect(view({ step: "busy", target: ben })).toContain("Copying the save. Keep the app open.");
    expect(view({ step: "done", target: ben })).toContain(
      "Astro Bot for Ben now has Ann&#x27;s save. Start the game as Ben to load it.",
    );
    expect(view({ step: "failed", target: ben, error: "Close the game" })).toContain("Couldn&#x27;t copy the save: Close the game");
  });

  it("says the same for a backup restored to another user", () => {
    const b: CopySource = { kind: "backup", when: "2026-11-01 10:15 UTC" };
    expect(view({ step: "pick" }, b)).toContain("Put the backup from 2026-11-01 10:15 UTC into whose save of Astro Bot?");
    expect(view({ step: "done", target: ben }, b)).toContain(
      "Astro Bot for Ben now has the backup from 2026-11-01 10:15 UTC. Start the game as Ben to load it.",
    );
  });
});
