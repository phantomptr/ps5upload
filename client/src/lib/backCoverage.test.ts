// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { describe, expect, it } from "vitest";

// System Back on a phone closes what is open on top (lib/systemBack), but only what registered
// for it. The Files row menu did not, and Back left the Files tab with the menu still up (#932).
// Every menu has to take part: through useBackDismiss, or a modal surface's focus trap.

const SRC = new URL("../", import.meta.url);

/** Menus that handle Back another way. */
const OWN_BACK: Record<string, string> = {
  // Remote Play's stream menu: the stream screen takes Back itself (its own popstate handler).
  "screens/Play/StreamPanel.tsx": "popstate",
};

/** The app's .tsx files (not tests, not the console's own page), relative to src/. */
function sources(rel = ""): string[] {
  return (readdirSync(new URL(rel, SRC)) as string[]).flatMap((name) => {
    const path = rel + name;
    if (statSync(new URL(path, SRC)).isDirectory()) return name === "console" ? [] : sources(`${path}/`);
    return /\.tsx$/.test(name) && !/\.test\.tsx$/.test(name) ? [path] : [];
  });
}

describe("system Back reaches every menu", () => {
  it("each file that draws a menu registers it for Back", () => {
    const menus = sources()
      .map((rel) => ({ rel, text: readFileSync(new URL(rel, SRC), "utf8") as string }))
      .filter(({ text }) => text.includes('role="menu"'));
    // The scan sees them at all (the Files row menu among them).
    expect(menus.map((m) => m.rel)).toContain("screens/FileSystem/RowActions.tsx");
    const missing = menus
      .filter(({ rel, text }) => {
        const own = OWN_BACK[rel];
        if (own) return !text.includes(own);
        return !/useBackDismiss\(|activateFocusTrap\(|<Modal\b|<Drawer\b/.test(text);
      })
      .map(({ rel }) => rel);
    expect(missing).toEqual([]);
  });
});
