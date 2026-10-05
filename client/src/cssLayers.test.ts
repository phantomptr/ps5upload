import postcss from "postcss";
import { describe, expect, it } from "vitest";
// @ts-expect-error -- plain .mjs config, no type declarations.
import config from "../postcss.config.mjs";

// #352: a WebView without `@layer` (Safari before 15.4) drops every Tailwind v4 rule. The build
// must hand it layer-free CSS that keeps the same cascade.
describe("postcss config flattens cascade layers (#352)", () => {
  const run = async (css: string) =>
    (await postcss(config.plugins).process(css, { from: undefined })).css;

  it("leaves no @layer in the output", async () => {
    const out = await run(
      "@layer base{a{color:red}}@layer utilities{.x{color:blue}}",
    );
    expect(out).not.toContain("@layer");
    expect(out).toContain("color:red");
    expect(out).toContain("color:blue");
  });

  it("a later layer still outranks an earlier one", async () => {
    // base `a` (1 element) vs utilities `.x`: both match <a class=x>; utilities must win, which the
    // plugin encodes as extra specificity on the utilities rule.
    const out = await run(
      "@layer base,utilities;@layer base{a.y{color:red}}@layer utilities{.x{color:blue}}",
    );
    expect(out).toMatch(/\.x:not\(#\\#\)/);
  });
});
