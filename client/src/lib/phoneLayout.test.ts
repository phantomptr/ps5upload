// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { PHONE_LAYOUT_QUERY } from "./phoneLayout";

const css: string = readFileSync(new URL("../index.css", import.meta.url), "utf8");

describe("the phone layout's query", () => {
  it("is the `phone:` variant's, so the script and the styles agree on what a phone is", () => {
    const variant = css.match(/@custom-variant phone \{\s*@media ([^{]+)\{/);
    expect(variant?.[1].trim()).toBe(PHONE_LAYOUT_QUERY);
  });

  it("is where the selection bar hides the bottom navigation (#936)", () => {
    expect(css).toContain(`@media ${PHONE_LAYOUT_QUERY} {\n  html[data-selection-bar] [data-bottom-nav]`);
  });
});
