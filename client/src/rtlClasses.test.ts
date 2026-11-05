// @vitest-environment node
import { describe, expect, it } from "vitest";
// @ts-expect-error plain .mjs script, no type declarations
import { physicalLines, toLogical } from "../../scripts/rtl-logical-classes.mjs";

describe("the app's classes follow the writing direction", () => {
  it("no screen uses a left or right margin, padding, border or alignment (node scripts/rtl-logical-classes.mjs fixes it)", () => {
    expect((physicalLines() as { file: string; line: number; text: string }[]).map((f) => `${f.file}:${f.line}`)).toEqual([]);
  });

  it("rewrites what it is for", () => {
    expect(toLogical(`<p className="ml-2 mr-auto pl-[3px] pr-1.5 -ml-1 !mr-2 md:pl-4 [&>svg]:mr-1.5">`)).toBe(
      `<p className="ms-2 me-auto ps-[3px] pe-1.5 -ms-1 !me-2 md:ps-4 [&>svg]:me-1.5">`,
    );
    expect(toLogical(`"text-left text-right border-l border-r-2 border-l-[red] rounded-l-md rounded-tr-lg"`)).toBe(
      `"text-start text-end border-s border-e-2 border-s-[red] rounded-s-md rounded-se-lg"`,
    );
  });

  it("leaves alone what only looks like it", () => {
    for (const same of [
      `"rounded-lg rounded-full"`,
      `"left-1/2 -translate-x-1/2 right-0"`,
      `"pl-PL pr-review ml"`,
      `"rp-pl-1 text-left-ish"`,
      `"pl-[env(safe-area-inset-left)] pr-[max(0.5rem,env(safe-area-inset-right))]"`,
      `// a physical ml-2 named in a comment`,
    ]) {
      expect(toLogical(same)).toBe(same);
    }
  });
});
