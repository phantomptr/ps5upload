#!/usr/bin/env node
// Splits the monolithic `src/i18n.ts` into one file per locale under
// `src/i18n/locales/<code>.ts`, so each locale can be lazy-loaded as
// its own Vite chunk. Run once; the main `src/i18n.ts` becomes a thin
// loader. See the comment in `src/i18n.ts` for the lazy-load design.

import fs from "node:fs";
import path from "node:path";

const SRC = path.resolve("src/i18n.ts");
const OUT_DIR = path.resolve("src/i18n/locales");

const text = fs.readFileSync(SRC, "utf8");
const lines = text.split("\n");

// Locate each locale's start line ("  en: {" or "  \"zh-CN\": {").
const headerRe = /^\s\s"?([a-zA-Z][a-zA-Z0-9_-]*)"?:\s*\{\s*$/;
const headers = [];
for (let i = 0; i < lines.length; i++) {
  const m = lines[i].match(headerRe);
  if (m) headers.push({ code: m[1], lineIdx: i });
}

// Find each block's end by tracking braces from the header's `{`.
// Returns { lineIdx, col } — col is the byte offset of the matching `}`
// within that line. Some locales have `,},` on the same line as the
// last content entry (no own line for the closing brace).
function findBlockEnd(startLine) {
  let depth = 0;
  for (let i = startLine; i < lines.length; i++) {
    const line = lines[i];
    for (let c = 0; c < line.length; c++) {
      const ch = line[c];
      if (ch === "{") depth++;
      else if (ch === "}") {
        depth--;
        if (depth === 0) return { lineIdx: i, col: c };
      }
    }
  }
  throw new Error(`unterminated block at line ${startLine}`);
}

for (const { code, lineIdx } of headers) {
  const { lineIdx: endLine, col: endCol } = findBlockEnd(lineIdx);
  // Inner body lines: everything strictly between the `{` line and the
  // closing `}` line, plus any content on the closing line up to the
  // `}` itself (17 of 18 locales pack the last entry + `},` on one line).
  const bodyLines = lines.slice(lineIdx + 1, endLine);
  const tail = lines[endLine].slice(0, endCol).replace(/,\s*$/, "");
  if (tail.trim().length > 0) bodyLines.push(tail);
  // De-indent by 4 spaces if every non-empty line starts with that;
  // monolithic file uses 4-space indent inside each locale.
  const dedented = bodyLines.map((l) =>
    l.startsWith("    ") ? l.slice(4) : l,
  );
  const safe = code.replace("-", "_");
  const out = [
    `// Auto-extracted from src/i18n.ts by scripts/split-i18n.mjs.`,
    `// Locale: ${code}. Lazy-loaded via dynamic import in src/i18n.ts.`,
    `import type { Translations } from "../types";`,
    ``,
    `const ${safe}: Translations = {`,
    ...dedented,
    `};`,
    ``,
    `export default ${safe};`,
    ``,
  ].join("\n");
  const target = path.join(OUT_DIR, `${code}.ts`);
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, out);
  console.log(`wrote ${target} (${bodyLines.length} lines)`);
}

console.log(`\nDone. ${headers.length} locales extracted.`);
console.log(`Now manually rewrite src/i18n.ts to lazy-load these chunks.`);
