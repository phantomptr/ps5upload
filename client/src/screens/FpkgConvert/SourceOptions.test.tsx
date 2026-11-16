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

import type { BackportInfo } from "../../api/fpkg";
import {
  DeleteSourceOption,
  RemoveBackportOption,
  backportRefusal,
  deleteSourceRefusal,
} from "./SourceOptions";

const backport: BackportInfo = {
  libraries: ["fakelib/libSceAgc.sprx", "fakelib2/libSceGnm.sprx"],
  emulators: [
    { path: "fakelib/libSceAmpr.sprx", name: "AMPR" },
    { path: "fakelib2/libSceAmpr.sprx", name: "AMPR" },
    { path: "fakelib/libSceGameUpdate.sprx", name: "DLC" },
  ],
};

describe("Remove backport libraries", () => {
  it("says how many libraries go and which emulators stay", () => {
    const html = renderToStaticMarkup(
      <RemoveBackportOption backport={backport} unpacks={false} checked onChange={() => {}} />,
    );
    expect(html).toContain("Remove backport libraries");
    expect(html).toContain("Leaves the 2 backport libraries");
    // Each kind of emulator once.
    expect(html).toContain("Emulators in fakelib are kept: AMPR, DLC.");
    expect(html).toContain('aria-checked="true"');
    expect(html).not.toContain("remove-backport-blocked");
    expect(html).not.toContain("remove-backport-packed");
  });

  it("is off and says why when the backport changed the game", () => {
    const blocked = { ...backport, blocked: "eboot.bin's SDK 9.00 is lower than param.json's sdkVersion 12.00" };
    expect(backportRefusal(blocked, true)).toBe("blocked");
    const html = renderToStaticMarkup(
      <RemoveBackportOption backport={blocked} unpacks checked onChange={() => {}} />,
    );
    expect(html).toContain("Not available for this game: eboot.bin");
    // Ticked before the reason was known, it is still not on.
    expect(html).toContain('aria-checked="false"');
    expect(html).toContain("disabled");
  });

  it("needs new zstd packs for a game that is in packs", () => {
    const packed = { ...backport, packed: "lz4" as const };
    expect(backportRefusal(packed, false)).toBe("packed");
    expect(backportRefusal(packed, true)).toBe(null);
    expect(backportRefusal(backport, false)).toBe(null);
    const off = renderToStaticMarkup(
      <RemoveBackportOption backport={packed} unpacks={false} checked={false} onChange={() => {}} />,
    );
    expect(off).toContain("Turn on zstd asset packs");
    const on = renderToStaticMarkup(
      <RemoveBackportOption backport={packed} unpacks checked onChange={() => {}} />,
    );
    expect(on).not.toContain("Turn on zstd asset packs");
    expect(on).toContain('aria-checked="true"');
  });
});

describe("Move the source to the Trash once the output is verified", () => {
  const free = { archive: false, merge: false, elsewhere: false };

  it("is refused for a source or an output that is not on this computer, an archive and a merge", () => {
    expect(deleteSourceRefusal(free)).toBe(null);
    expect(deleteSourceRefusal({ ...free, engineReason: "the source is on a PS5 or a server" })).toEqual({
      kind: "engine",
      reason: "the source is on a PS5 or a server",
    });
    expect(deleteSourceRefusal({ ...free, archive: true })).toEqual({ kind: "archive" });
    expect(deleteSourceRefusal({ ...free, merge: true })).toEqual({ kind: "merge" });
    expect(deleteSourceRefusal({ ...free, elsewhere: true })).toEqual({ kind: "elsewhere" });
    // The engine's reason comes first: it holds whatever is made.
    expect(deleteSourceRefusal({ engineReason: "no Trash", archive: true, merge: true, elsewhere: true })).toEqual({
      kind: "engine",
      reason: "no Trash",
    });
  });

  it("is off until ticked, and names the path once it is on", () => {
    const off = renderToStaticMarkup(
      <DeleteSourceOption path="/games/PPSA1-app" refusal={null} checked={false} onChange={() => {}} />,
    );
    expect(off).toContain("Move the source to the Trash once the output is verified");
    expect(off).toContain("games in the queue are never removed");
    expect(off).toContain('aria-checked="false"');
    expect(off).not.toContain("delete-source-path");
    const on = renderToStaticMarkup(
      <DeleteSourceOption path="/games/PPSA1-app" refusal={null} checked onChange={() => {}} />,
    );
    expect(on).toContain('aria-checked="true"');
    expect(on).toContain("Will be moved to the Trash: /games/PPSA1-app");
  });

  it("cannot be on while something is in the way, and says what", () => {
    const refused = (refusal: Parameters<typeof DeleteSourceOption>[0]["refusal"]) =>
      renderToStaticMarkup(<DeleteSourceOption path="/g" refusal={refusal} checked onChange={() => {}} />);
    for (const [refusal, text] of [
      [{ kind: "engine", reason: "this engine has no Trash" }, "Not available: this engine has no Trash"],
      [{ kind: "archive" }, "Not available for an archive"],
      [{ kind: "merge" }, "there are two sources"],
      [{ kind: "elsewhere" }, "saved straight to the PS5 or a server"],
    ] as const) {
      const html = refused(refusal);
      expect(html).toContain(text);
      // Ticked before, it is still not on, and no path is promised.
      expect(html).toContain('aria-checked="false"');
      expect(html).not.toContain("delete-source-path");
    }
  });
});
