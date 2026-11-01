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
    // A user without a save of the game can't be picked.
    expect(copyStep(s, { type: "pick", target: cy })).toBe(s);
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

  it("shows who has a save, and what the others must do", () => {
    const html = view({ step: "pick" });
    expect(html).toContain("Copy Ann&#x27;s save of Astro Bot to:");
    expect(html).toContain("Has a save of this game");
    expect(html).toContain("Start Astro Bot once as Cy and save, then come back.");
    expect(html.match(/disabled=""/g)?.length).toBe(1);
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
