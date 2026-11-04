// @ts-expect-error -- plain .mjs rule, no type declarations.
import rules from "./no-untranslated-jsx.mjs";
import { Linter } from "eslint";
import tseslint from "typescript-eslint";
import { describe, expect, it } from "vitest";

const linter = new Linter();
const messages = (code: string) =>
  linter
    .verify(
      code,
      [
        {
          files: ["**/*.tsx"],
          languageOptions: {
            parser: tseslint.parser,
            parserOptions: { ecmaFeatures: { jsx: true } },
          },
          plugins: { local: rules },
          rules: { "local/no-untranslated-jsx": "error" },
        },
      ],
      { filename: "x.tsx" },
    )
    .map((m) => m.message);

describe("no-untranslated-jsx", () => {
  it("catches text and literal attributes, as before", () => {
    expect(messages(`<a>Cancel</a>`)).toHaveLength(1);
    expect(messages(`<a title="Cancel" />`)).toHaveLength(1);
  });
  it("catches a template literal in a UI attribute", () => {
    expect(messages("<a title={`Unmount ${p}`} />")).toHaveLength(1);
    expect(messages("<a aria-label={`${n} logged errors`} />")).toHaveLength(1);
  });
  it("catches literals in a branch of a condition", () => {
    expect(messages(`<a placeholder={x ? "Drop it" : "Pick one"} />`)).toHaveLength(2);
    expect(messages(`<a title={x ?? "Nothing here"} />`)).toHaveLength(1);
    expect(messages(`<a title={x && "Nothing here"} />`)).toHaveLength(1);
  });
  it("leaves tr(), and templates with no text of their own, alone", () => {
    expect(messages(`<a title={tr("k", "Unmount")} />`)).toHaveLength(0);
    expect(messages("<a title={`${tr('k')}: ${name}`} />")).toHaveLength(0);
    expect(messages("<a title={`${a} · ${b}`} />")).toHaveLength(0);
    expect(messages("<a title={x ? tr('k') : undefined} />")).toHaveLength(0);
  });
  it("leaves attributes a user doesn't read alone", () => {
    expect(messages("<a href={`/files/${p}`} className={`a ${b}`} />")).toHaveLength(0);
  });
});
