import { describe, expect, it } from "vitest";
import type { Translator } from "../../state/lang";
import { partMissingText } from "./partMissing";

// English through the fallback, as the app shows it before a locale loads.
const tr = ((key: string, vars?: Record<string, string | number> | string, fallback?: string) => {
  const text = typeof vars === "string" ? vars : (fallback ?? key);
  const v = typeof vars === "object" ? vars : {};
  return text.replace(/\{(\w+)\}/g, (_, k: string) => String(v[k] ?? `{${k}}`));
}) as Translator;

const witcher = ["sdimg_Auto.8559a.7ea54c00.4dd04c1", "sdimg_Manual.53db9.7ea54c00.4f3261a"];

describe("a save that needs slots the other user doesn't have", () => {
  it("a copy only stops for an old helper, and says to send the new one (not to save in the same slot)", () => {
    const text = partMissingText(tr, witcher, "The Witcher 3", "Playsteed5", "copy");
    expect(text).toContain("Auto.8559a.7ea54c00.4dd04c1, Manual.53db9.7ea54c00.4f3261a");
    expect(text).toContain("too old to add save slots for Playsteed5");
    expect(text).toContain("Send the helper");
    expect(text).not.toContain("same slot");
  });

  it("a save file points a game with its own save names at a copy on the PS5 the save is on", () => {
    const text = partMissingText(tr, ["sdimg_sce_sdmemory"], "Astro Bot", "Ben", "file");
    expect(text).toContain("Ben doesn't have Astro Bot's main save yet");
    expect(text).toContain("save in the same slot");
    expect(text).toContain("use Copy to another user or Copy to another console");
  });
});
