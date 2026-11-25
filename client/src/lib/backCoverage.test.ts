import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// System Back on a phone closes what is open on top (lib/systemBack), but only what registered
// for it. The Files row menu did not, and Back left the Files tab with the menu still up (#932).
// Every menu has to take part: through useBackDismiss, or a modal surface's focus trap.

const SRC = fileURLToPath(new URL("..", import.meta.url));

/** Menus that handle Back another way. */
const OWN_BACK: Record<string, string> = {
  // Remote Play's stream menu: the stream screen takes Back itself (its own popstate handler).
  "screens/Play/StreamPanel.tsx": "popstate",
};

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) return name === "console" ? [] : sources(p);
    return /\.tsx$/.test(name) && !/\.test\.tsx$/.test(name) ? [p] : [];
  });
}

describe("system Back reaches every menu", () => {
  it("each file that draws a menu registers it for Back", () => {
    const missing = sources(SRC)
      .filter((p) => readFileSync(p, "utf8").includes('role="menu"'))
      .map((p) => ({ rel: relative(SRC, p).split("\\").join("/"), text: readFileSync(p, "utf8") }))
      .filter(({ rel, text }) => {
        const own = OWN_BACK[rel];
        if (own) return !text.includes(own);
        return !/useBackDismiss\(|activateFocusTrap\(|<Modal\b|<Drawer\b/.test(text);
      })
      .map(({ rel }) => rel);
    expect(missing).toEqual([]);
  });
});
