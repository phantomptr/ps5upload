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
import { RemoveBackportOption, backportRefusal } from "./SourceOptions";

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
