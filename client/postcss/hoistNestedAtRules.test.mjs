import postcss from "postcss";
import cascadeLayers from "@csstools/postcss-cascade-layers";
import { describe, expect, it } from "vitest";

import hoistNestedAtRules from "./hoistNestedAtRules.mjs";

const run = (css, plugins) => postcss(plugins).process(css, { from: undefined }).css;

const NESTED = `@layer utilities {
  .tint {
    background-color: var(--color-good);
    @supports (color: color-mix(in lab, red, red)) {
      background-color: color-mix(in oklab, var(--color-good) 5%, transparent);
    }
  }
}`;

describe("hoistNestedAtRules", () => {
  it("moves a nested @supports out beside its rule", () => {
    const out = run(".a { color: red; @supports (x: y) { color: blue; } }", [hoistNestedAtRules()]);
    const squash = (css) => css.replace(/[\s;]+/g, "");
    expect(squash(out)).toBe(squash(".a { color: red } @supports (x: y) { .a { color: blue } }"));
  });

  it("keeps the colour-mix through the layer flattening", () => {
    // Without the hoist the flattening drops the nested block: only the
    // full-strength fallback is left.
    expect(run(NESTED, [cascadeLayers()])).not.toContain("color-mix");
    expect(run(NESTED, [hoistNestedAtRules(), cascadeLayers()])).toContain(
      "color-mix(in oklab, var(--color-good) 5%, transparent)",
    );
  });

  it("leaves an at-rule with nested rules alone", () => {
    const css = ".a { @media (hover: hover) { &:hover { color: blue; } } }";
    expect(run(css, [hoistNestedAtRules()])).toBe(css);
  });
});
