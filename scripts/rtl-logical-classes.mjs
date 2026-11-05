#!/usr/bin/env node
/*
 * The app runs right to left in Arabic and Persian, so its margins, paddings, borders and text
 * alignment are written by start and end (`ms-2`), not by left and right (`ml-2`): a physical class
 * keeps its side when the layout mirrors. This rewrites them.
 *
 * Only the classes that always follow the writing direction are touched. `left-*` and `right-*` are
 * left to the author: `left-1/2 -translate-x-1/2` means the same in both directions, and an overlay on
 * a video has no direction at all.
 *
 *   node scripts/rtl-logical-classes.mjs           rewrite
 *   node scripts/rtl-logical-classes.mjs --check   list what would change, exit 1 when anything would
 *
 * client/src/rtlClasses.test.ts fails on a physical class; running this fixes it.
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const ROOTS = ["client/src/screens", "client/src/components", "client/src/layout"];

// A class starts the string, or follows a space, a quote, a variant's colon or an important's "!".
const START = "(?<=^|[\\s\"'`:!])(-?)";
// A safe-area inset is a side of the screen, not of the text: `pl-[env(safe-area-inset-left)]` stays.
const SIZED = "(?=auto|px|\\d|\\[(?![^\\]]*safe-area)|\\()";
const END = "(?=$|[\\s\"'`\\]}-])";
const WORD_END = "(?=$|[\\s\"'`\\]}])";

const RULES = [
  [new RegExp(`${START}ml-${SIZED}`, "gm"), "$1ms-"],
  [new RegExp(`${START}mr-${SIZED}`, "gm"), "$1me-"],
  [new RegExp(`${START}pl-${SIZED}`, "gm"), "$1ps-"],
  [new RegExp(`${START}pr-${SIZED}`, "gm"), "$1pe-"],
  [new RegExp(`${START}text-left${WORD_END}`, "gm"), "$1text-start"],
  [new RegExp(`${START}text-right${WORD_END}`, "gm"), "$1text-end"],
  [new RegExp(`${START}border-l${END}`, "gm"), "$1border-s"],
  [new RegExp(`${START}border-r${END}`, "gm"), "$1border-e"],
  [new RegExp(`${START}rounded-l${END}`, "gm"), "$1rounded-s"],
  [new RegExp(`${START}rounded-r${END}`, "gm"), "$1rounded-e"],
  [new RegExp(`${START}rounded-tl${END}`, "gm"), "$1rounded-ss"],
  [new RegExp(`${START}rounded-tr${END}`, "gm"), "$1rounded-se"],
  [new RegExp(`${START}rounded-bl${END}`, "gm"), "$1rounded-es"],
  [new RegExp(`${START}rounded-br${END}`, "gm"), "$1rounded-ee"],
];

/** `line` with its physical classes written by start and end. Comments keep their words. */
export function toLogical(line) {
  if (/^\s*(\/\/|\/?\*)/.test(line)) return line;
  return RULES.reduce((text, [re, to]) => text.replace(re, to), line);
}

/** The source files in scope, as paths relative to the repository. */
export function sourceFiles(root = repoRoot) {
  const out = [];
  const walk = (dir) => {
    for (const e of fs.readdirSync(path.join(root, dir), { withFileTypes: true })) {
      const rel = `${dir}/${e.name}`;
      if (e.isDirectory()) walk(rel);
      else if (e.name.endsWith(".tsx") && !e.name.endsWith(".test.tsx")) out.push(rel);
    }
  };
  ROOTS.forEach(walk);
  return out.sort();
}

/** Every line that is still physical: { file, line, text }. */
export function physicalLines(root = repoRoot) {
  const found = [];
  for (const file of sourceFiles(root)) {
    fs.readFileSync(path.join(root, file), "utf8")
      .split("\n")
      .forEach((text, i) => {
        if (toLogical(text) !== text) found.push({ file, line: i + 1, text: text.trim() });
      });
  }
  return found;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.includes("--check")) {
    const found = physicalLines();
    for (const f of found) console.log(`${f.file}:${f.line}: ${f.text}`);
    console.log(`${found.length} line(s) use a physical class`);
    process.exit(found.length ? 1 : 0);
  }
  let changed = 0;
  for (const file of sourceFiles()) {
    const full = path.join(repoRoot, file);
    const before = fs.readFileSync(full, "utf8");
    const after = before.split("\n").map(toLogical).join("\n");
    if (after !== before) {
      fs.writeFileSync(full, after);
      changed++;
    }
  }
  console.log(`${changed} file(s) rewritten`);
}
