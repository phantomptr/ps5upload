/**
 * The guest pages' stylesheet: only the classes those pages use.
 *
 * `src/index.css` starts with `@import "tailwindcss"`, which scans the whole app, so /join and /pad
 * each inlined all 264 KB of the desktop app's rules (a third of /join, against its size budget),
 * and a phone parsed and matched all of them. For these builds that line becomes a scan of exactly
 * the files in the page: everything reachable from its entry by relative import, the same walk the
 * bundler does. Nothing has to be listed by hand, and a new component is picked up by itself.
 */
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import type { Plugin } from "vite";

const EXTENSIONS = [".tsx", ".ts", ".jsx", ".js"];
const IMPORT = /(?:\bfrom\s+|\bimport\s+|\bimport\(\s*)["'](\.[^"']+)["']/g;

function resolve(from: string, spec: string): string | null {
  const base = path.resolve(path.dirname(from), spec);
  if (path.extname(base) && existsSync(base)) return base;
  for (const ext of EXTENSIONS) {
    if (existsSync(base + ext)) return base + ext;
    const index = path.join(base, `index${ext}`);
    if (existsSync(index)) return index;
  }
  return null;
}

/** Every source file reachable from `entry` by relative import. */
export function pageFiles(
  entry: string,
  swap: (file: string) => string = (f) => f,
): string[] {
  const seen = new Set<string>();
  const todo = [entry];
  while (todo.length) {
    const file = swap(todo.pop()!);
    if (seen.has(file) || !EXTENSIONS.includes(path.extname(file))) continue;
    seen.add(file);
    const source = readFileSync(file, "utf8");
    for (const m of source.matchAll(IMPORT)) {
      const next = resolve(file, m[1]);
      if (next) todo.push(next);
    }
  }
  return [...seen].sort();
}

/** `index.css` with its Tailwind import scanning only `files`. */
export function scopedCss(
  css: string,
  cssFile: string,
  files: string[],
): string {
  const dir = path.dirname(cssFile);
  const sources = files
    .map((f) => `@source "${path.relative(dir, f).split(path.sep).join("/")}";`)
    .join("\n");
  return css.replace(
    /@import\s+"tailwindcss"\s*;/,
    `@import "tailwindcss" source(none);\n${sources}`,
  );
}

/** The plugin: put it before `tailwindcss()`. `swap` follows the page's own module swaps. */
export function pageCss(
  entry: string,
  swap?: (file: string) => string,
): Plugin {
  return {
    name: "ps5upload-page-css",
    enforce: "pre",
    transform(code, id) {
      const file = id.split("?")[0];
      if (!file.endsWith("/src/index.css")) return null;
      return { code: scopedCss(code, file, pageFiles(entry, swap)), map: null };
    },
  };
}
