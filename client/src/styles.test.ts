// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

// From disk: Vitest turns a CSS import, even `?raw`, into an empty string.
const CSS: string = readFileSync(new URL("./index.css", import.meta.url), "utf8");

/** The CSS outside every @layer block: unlayered rules outrank all of Tailwind's layers. */
function unlayered(css: string): string {
  let out = "";
  let depth = 0;
  let layerDepth = -1;
  for (let i = 0; i < css.length; i++) {
    if (layerDepth < 0 && css.startsWith("@layer", i) && /^@layer[^;{]*\{/.test(css.slice(i, i + 80))) {
      layerDepth = depth;
    }
    const c = css[i];
    if (c === "{") depth++;
    if (c === "}") {
      depth--;
      if (depth === layerDepth) {
        layerDepth = -1;
        continue;
      }
    }
    if (layerDepth < 0) out += c;
  }
  return out;
}

describe("global CSS", () => {
  it("never sets a button's font outside a layer, which would beat every text-/font- class", () => {
    expect(CSS.length).toBeGreaterThan(1000);
    const top = unlayered(CSS.replace(/\/\*[\s\S]*?\*\//g, ""));
    expect(top).not.toMatch(/(^|[\s,}])button\s*\{[^}]*font/);
  });

  it("leaves a field's size to a layer, so text-xs or py-1 on an Input or Select applies", () => {
    const top = unlayered(CSS.replace(/\/\*[\s\S]*?\*\//g, ""));
    expect(top).not.toMatch(/(^|[\s,}])\.(input|select|input-sm)\s*\{[^}]*(font-size|padding)\s*:/);
    // The layered block itself is still there.
    expect(CSS).toMatch(/@layer components\s*\{[^@]*\.input,\s*\.select\s*\{[^}]*padding:/);
  });
});
