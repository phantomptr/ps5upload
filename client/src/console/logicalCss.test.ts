// @vitest-environment node
// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { execFileSync } from "node:child_process";
// @ts-expect-error -- as above.
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
// @ts-expect-error -- as above.
import { tmpdir } from "node:os";
// @ts-expect-error -- as above.
import { join } from "node:path";
// @ts-expect-error -- as above.
import { fileURLToPath } from "node:url";
import { transform } from "lightningcss";
import postcss from "postcss";
import { describe, expect, it } from "vitest";
// @ts-expect-error plain .mjs build helper, no type declarations
import { splitLangLists } from "../../postcss/langLists.mjs";

/**
 * The page sets dir="rtl" for Arabic and Persian (strings.ts), so its styles are written by start and
 * end, not by left and right. A left or right that means the same in both directions (a box centred
 * with a translate, one stretched to both sides) says so in a comment right after it.
 */

declare const process: { execPath: string; env: Record<string, string | undefined> };

const HERE: string = fileURLToPath(new URL(".", import.meta.url));
const CLIENT: string = fileURLToPath(new URL("../../", import.meta.url));

/** From disk: Vitest turns a CSS import, even `?raw`, into an empty string. */
function readSheets(dir = "."): Record<string, string> {
  const out: Record<string, string> = {};
  for (const e of readdirSync(join(HERE, dir), { withFileTypes: true }) as { name: string; isDirectory(): boolean }[]) {
    const rel = `${dir}/${e.name}`;
    if (e.isDirectory()) Object.assign(out, readSheets(rel));
    else if (e.name.endsWith(".css")) out[rel] = readFileSync(join(HERE, rel), "utf8");
  }
  return out;
}

const SHEETS = readSheets();
/** `.cw-x margin-left: 1rem` for each declaration in `css` that is physical without saying why. */
function physicalDeclarations(css: string): string[] {
  const out: string[] = [];
  postcss.parse(css).walkDecls((d) => {
    const prop = d.prop.toLowerCase();
    const value = d.value.toLowerCase();
    const parts = postcss.list.space(value);
    const physical =
      /^(margin|padding)-(left|right)$/.test(prop) ||
      /^(left|right)$/.test(prop) ||
      /^border-(left|right)(-|$)/.test(prop) ||
      /^border-(top|bottom)-(left|right)-radius$/.test(prop) ||
      (/^(text-align|float|clear)$/.test(prop) && /^(left|right)$/.test(value)) ||
      // Four values with different sides are a left and a right too.
      (/^(margin|padding)$/.test(prop) && parts.length === 4 && parts[1] !== parts[3]);
    if (!physical) return;
    const next = d.next();
    if (next?.type === "comment" && next.text.startsWith("physical:")) return;
    const rule = d.parent as { selector?: string };
    out.push(`${rule.selector ?? ""} ${d.prop}: ${d.value}`);
  });
  return out;
}

describe("the console page's stylesheets", () => {
  it("are found", () => {
    expect(Object.keys(SHEETS).sort()).toEqual(
      expect.arrayContaining(["./console.css", "./files/viewer/viewer.css", "./files/search/search.css"]),
    );
  });

  for (const [file, css] of Object.entries(SHEETS)) {
    it(`${file} has no left or right that follows the writing direction`, () => {
      expect(css.length).toBeGreaterThan(0);
      expect(physicalDeclarations(css)).toEqual([]);
    });
  }

  it("the check itself catches what it is for", () => {
    const bad = `.a { margin-left: 1rem; }
      .b { padding-right: 1rem; }
      .c { left: 0; }
      .d { border-right: 1px solid red; }
      .e { text-align: left; }
      .f { margin: 0 1rem 0 0; }
      .g { float: right; }
      .h { border-top-left-radius: 1px; }`;
    expect(physicalDeclarations(bad)).toHaveLength(8);
    const fine = `.a { margin-inline-start: 1rem; text-align: start; margin: 0 1rem; padding: 1px 2px 3px 2px; }
      .b { left: 50%; /* physical: centred */ transform: translateX(-50%); }`;
    expect(physicalDeclarations(fine)).toEqual([]);
  });

  /** The stylesheets as the build leaves them: the minifier at the page's target, then one language per :lang(). */
  function built(): string {
    const css = Object.values(SHEETS).join("\n");
    const { code } = transform({
      filename: "console.css",
      code: new TextEncoder().encode(css),
      minify: true,
      targets: { safari: 13 << 16 },
    });
    return splitLangLists(new TextDecoder().decode(code)) as string;
  }

  it("what the safari13 floor gets: only logical properties it has, the rest as left/right by language", () => {
    const css = built();
    // Safari 12.1 has these. Everything else logical (inset-inline, the two-value shorthands) must be gone.
    const logical = new Set(css.match(/[a-z-]*-(inline|block)[a-z-]*(?=:)/g) ?? []);
    const has = new Set([
      "margin-inline-start",
      "margin-inline-end",
      "padding-inline-start",
      "padding-inline-end",
      "border-inline-start",
      "border-inline-end",
    ]);
    expect([...logical].filter((p) => !has.has(p))).toEqual([]);
    expect(logical.size).toBeGreaterThan(0);
    // `inset-inline-*` came out as physical pairs chosen by language.
    expect(css).toMatch(/:lang\(ar\)[^{]*\{(left|right):/);
    expect(css).toMatch(/:not\(:lang\(ar\)\):not\(:lang\(fa\)\)[^{]*\{(left|right):/);
  });

  it("no language list is left: Chromium before 141 would drop both rules on a phone", () => {
    expect(built()).not.toMatch(/:lang\([a-z-]+,/);
  });

  it("the page as really built has no language list either, and has the left/right pairs", () => {
    // In a process of its own: under Vitest NODE_ENV is "test", React comes out in its dev build and
    // the page is over its budget.
    const outDir: string = mkdtempSync(join(tmpdir(), "ps5upload-console-page-"));
    try {
      execFileSync(
        process.execPath,
        [join(CLIENT, "node_modules/vite/bin/vite.js"), "build", "-c", "vite.console.config.ts", "--outDir", outDir, "--emptyOutDir", "--logLevel", "error"],
        { cwd: CLIENT, env: { ...process.env, NODE_ENV: "production" }, stdio: "pipe" },
      );
      const page: string = readFileSync(join(outDir, "index.html"), "utf8");
      const css = (/<style>([\s\S]*)<\/style>/.exec(page) ?? [])[1] ?? "";
      expect(css).toMatch(/:lang\(ar\)[^{]*\{(left|right):/);
      expect(css).toMatch(/:not\(:lang\(ar\)\):not\(:lang\(fa\)\)[^{]*\{(left|right):/);
      expect(css).not.toMatch(/:lang\([a-z-]+,/);
    } finally {
      rmSync(outDir, { recursive: true, force: true });
    }
  }, 120_000);
});
