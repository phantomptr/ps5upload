import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import type { FpkgInspection } from "../../api/fpkg";
import { mergeRole, Ps4KindNote, Ps4MergePicker } from "./Ps4Merge";

const base: FpkgInspection = {
  source: "PS4 package",
  files: 3,
  bytes: 1000,
  planned_size: 1000,
  checks: [],
  ps4_game: true,
};

describe("Ps4KindNote", () => {
  it("says what each PS4 kind becomes", () => {
    const note = (i: Partial<FpkgInspection>) => renderToStaticMarkup(<Ps4KindNote inspection={{ ...base, ...i }} />);
    expect(note({ ps4_kind: "game" })).toContain("It converts to a PS4 package");
    const patch = note({ ps4_kind: "patch", ps4_title_id: "CUSA99990", content_version: "01.05" });
    expect(patch).toContain("A PS4 update, version 01.05, for CUSA99990");
    expect(patch).toContain("choose the game it goes into");
    expect(note({ ps4_kind: "addon", ps4_addon_data: true, ps4_title_id: "CUSA99990" })).toContain(
      "Its files become a folder",
    );
    expect(note({ ps4_kind: "addon", ps4_addon_data: false, files: 0 })).toContain("nothing to convert");
    const merged = note({
      ps4_kind: "game",
      ps4_merge: { title_id: "CUSA99990", game_version: "01.00", update_version: "01.05", replaced: 3, added: 2 },
    });
    expect(merged).toContain("01.00 becomes 01.05");
    expect(renderToStaticMarkup(<Ps4KindNote inspection={{ ...base, ps4_game: false }} />)).toBe("");
  });
});

describe("mergeRole", () => {
  it("asks an update for its game, offers a game an update, and nothing else", () => {
    expect(mergeRole({ ...base, ps4_kind: "patch" })).toBe("patch");
    expect(mergeRole({ ...base, ps4_kind: "game" })).toBe("game");
    expect(mergeRole({ ...base, ps4_kind: "addon" })).toBeNull();
    expect(mergeRole({ ...base, ps4_game: false })).toBeNull();
    expect(mergeRole(null)).toBeNull();
  });
});

describe("Ps4MergePicker", () => {
  const props = {
    other: "/games/CUSA99990.pkg",
    onOther: () => {},
    onCheck: () => {},
    merged: null,
    error: null,
    checking: false,
    locked: false,
  };
  it("names the other half and says how the merge went", () => {
    const patch = renderToStaticMarkup(<Ps4MergePicker role="patch" {...props} />);
    expect(patch).toContain("Merge into the game");
    expect(patch).toContain("The game this update goes into");
    const game = renderToStaticMarkup(<Ps4MergePicker role="game" {...props} />);
    expect(game).toContain("A PS4 update for this game");
    const ok = renderToStaticMarkup(
      <Ps4MergePicker
        role="patch"
        {...props}
        merged={{
          ...base,
          ps4_merge: { title_id: "CUSA99990", title: "Test", game_version: "01.00", update_version: "01.05", replaced: 3, added: 2 },
        }}
      />,
    );
    expect(ok).toContain("Test: 01.00 becomes 01.05. The update replaces 3 files and adds 2.");
    const refused = renderToStaticMarkup(
      <Ps4MergePicker role="patch" {...props} error="the update is for CUSA11111" />,
    );
    expect(refused).toContain("the update is for CUSA11111");
  });
});
