#!/usr/bin/env node
// Fails on flex rows a phone squeezes to a letter a line (#794). The app sets `min-width: 0` on
// everything (client/src/index.css), so a text block that is `flex-1` with no floor shrinks to
// nothing beside buttons that keep their width. A row is reported when:
//   - it is a flex row that does not wrap (`flex`, no `flex-col`, no `flex-wrap`),
//   - a child is `flex-1` with no floor (`basis-[…]`, `min-w-[…]`), is text and not a field, and
//     is not cut to one line (`truncate`, also on what it holds: a cut name stays readable),
//   - its siblings hold more buttons than a phone's row has room for beside readable text (their
//     English labels are measured roughly; one icon button or a short "Copy" is fine).
// The fix: `flex-wrap` on the row and `basis-[12rem]` on the text, so the buttons go below it.
// It reads class names in the source, so a row whose classes are built elsewhere is not seen;
// a row that is fine as it is goes in ALLOW with the reason.
//
//   node scripts/check-phone-squeeze.mjs          # list and fail
//   node scripts/check-phone-squeeze.mjs --list   # list only
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const ROOT = path.join(REPO, "client/src");
let ts;
try {
  ts = createRequire(path.join(REPO, "client/package.json"))("typescript");
} catch {
  console.error("phone squeeze check: needs the client's packages (cd client && npm ci)");
  process.exit(2);
}

/** `file:data-testid or first words of the text block`: rows that are fine, and why. */
const ALLOW = new Map([]);

/** Not the app's screens: the console's own page has its own styles. */
const SKIP_DIRS = new Set(["console", "node_modules", "i18n"]);

const CONTROLS = /^(button|a|Link|select|Select|Switch|SegmentedControl|Segmented|.*Button|.*Toggle|.*Menu)$/;
/** The text block is itself a field or a control: it has its own least width, and no words to stack. */
const FIELDS = /^(input|Input|textarea|select|button|a|Link|NavLink|pre|.*Slider|.*Toggle|.*Button)$/;

// A label's letter and a button's padding at `text-sm`, and an icon button: rough, and English (the
// other languages run longer).
const CHAR_PX = 7;
const PAD_PX = 28;
const ICON_PX = 32;
// A phone's card is about 330 px inside and text needs 12rem (192 px) to read; the other
// languages' labels run up to half as long again. More than this beside the text squeezes it
// (ROOM_PX=60 in the environment lists the near misses too).
const ROOM_PX = Number(process.env.ROOM_PX || 100);

function walk(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (SKIP_DIRS.has(e.name)) continue;
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (e.name.endsWith(".tsx") && !/\.test\.tsx$/.test(e.name)) out.push(p);
  }
  return out;
}

/** Every string literal in a className attribute (cn(…), a ternary, a template's fixed parts). */
function classText(el) {
  const opening = el.openingElement ?? el;
  const attr = opening.attributes?.properties?.find((a) => ts.isJsxAttribute(a) && a.name.getText() === "className");
  if (!attr?.initializer) return "";
  const parts = [];
  const visit = (n) => {
    if (ts.isStringLiteralLike(n) || ts.isTemplateHead(n) || ts.isTemplateMiddle(n) || ts.isTemplateTail(n)) parts.push(n.text);
    ts.forEachChild(n, visit);
  };
  visit(attr.initializer);
  return parts.join(" ");
}

const has = (cls, token) => cls.split(/\s+/).includes(token);
const tagOf = (el) => (el.openingElement ?? el).tagName.getText();
const isJsx = (n) => ts.isJsxElement(n) || ts.isJsxSelfClosingElement(n);

/** The elements a row lays out: its JSX children, through `{cond && <X/>}`, ternaries, maps and fragments. */
function layoutChildren(el) {
  const out = [];
  const visit = (n) => {
    if (isJsx(n)) return void out.push(n);
    ts.forEachChild(n, visit);
  };
  for (const c of el.children ?? []) {
    if (ts.isJsxFragment(c)) out.push(...layoutChildren(c));
    else visit(c);
  }
  return out;
}

/** The label a control shows: its `tr(…, "Default")` text, else its own words; "" for an icon alone. */
function labelText(n) {
  let best = "";
  const visit = (c) => {
    if (ts.isJsxAttribute(c)) return; // title, aria-label: not on show
    if (ts.isCallExpression(c) && /^(tr|trStatic|t)$/.test(c.expression.getText())) {
      const last = c.arguments[c.arguments.length - 1];
      if (last && ts.isStringLiteralLike(last) && last.text.length > best.length) best = last.text;
      return;
    }
    if (ts.isJsxText(c) && c.text.trim().length > best.length) best = c.text.trim();
    ts.forEachChild(c, visit);
  };
  for (const c of n.children ?? []) visit(c);
  return best;
}

/** About how wide (px) the controls in `n` are at the app's small text: what the row can't shrink. */
function controlsWidth(n) {
  let px = 0;
  const visit = (c) => {
    if (isJsx(c) && CONTROLS.test(tagOf(c))) {
      const label = ts.isJsxElement(c) ? labelText(c) : "";
      px += label ? label.length * CHAR_PX + PAD_PX : ICON_PX;
      return;
    }
    ts.forEachChild(c, visit);
  };
  visit(n);
  return px;
}

/** One line, cut with "…": the block itself, or the first thing it holds. */
function cutToOneLine(el) {
  if (/\b(truncate|line-clamp-1|whitespace-nowrap)\b/.test(classText(el))) return true;
  const kids = layoutChildren(el);
  return kids.length > 0 && kids.every((k) => /\b(truncate|line-clamp-1)\b/.test(classText(k)));
}

// With `min-w-0` or without: index.css gives everything `min-width: 0`.
const floorless = (cls) => has(cls, "flex-1") && !/(^|\s)(basis-\[|min-w-\[|basis-\d|sm:flex-1|w-full)/.test(cls);
const nonWrappingRow = (cls) => (has(cls, "flex") || has(cls, "inline-flex")) && !/(^|\s)(flex-col|flex-wrap|flex-wrap-reverse)(\s|$)/.test(cls);

const labelOf = (el, sf) => {
  const opening = el.openingElement ?? el;
  const id = opening.attributes.properties.find((a) => ts.isJsxAttribute(a) && a.name.getText() === "data-testid");
  if (id?.initializer && ts.isStringLiteral(id.initializer)) return id.initializer.text;
  return el.getText(sf).replace(/\s+/g, " ").slice(0, 60);
};

const findings = [];
for (const file of walk(ROOT)) {
  const rel = path.relative(REPO, file);
  const sf = ts.createSourceFile(file, fs.readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const visit = (n) => {
    if (ts.isJsxElement(n) && nonWrappingRow(classText(n))) {
      const kids = layoutChildren(n);
      for (const text of kids) {
        if (!floorless(classText(text)) || cutToOneLine(text) || FIELDS.test(tagOf(text))) continue;
        if (ts.isJsxSelfClosingElement(text) || !text.getText(sf).replace(/<[^>]*>/g, "").trim()) continue; // a spacer
        const px = kids.filter((k) => k !== text).reduce((sum, k) => sum + controlsWidth(k), 0);
        if (px <= ROOM_PX) continue;
        const row = classText(n);
        // The row sits at the wide breakpoint only (`hidden md:flex`), or stacks on a phone.
        if (/(^|\s)(hidden)(\s|$)/.test(row) && /(md|lg|xl):flex/.test(row)) continue;
        const line = sf.getLineAndCharacterOfPosition(text.getStart(sf)).line + 1;
        const key = `${rel}:${labelOf(text, sf)}`;
        if ([...ALLOW.keys()].some((k) => key.startsWith(k))) continue;
        const rowLine = sf.getLineAndCharacterOfPosition(n.getStart(sf)).line + 1;
        findings.push(`${rel}:${line} (row at ${rowLine})  ~${px}px of buttons beside  ${labelOf(text, sf)}`);
      }
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
}

if (findings.length) {
  console.log(findings.join("\n"));
  console.log(
    `\n${findings.length} flex row(s) whose text a phone squeezes beside its buttons (#794).\n` +
      "Fix: `flex-wrap` on the row and `basis-[12rem]` on the `min-w-0 flex-1` text, or add the row to ALLOW in this script with the reason.",
  );
  if (!process.argv.includes("--list")) process.exit(1);
} else {
  console.log("phone squeeze check: no flex row squeezes its text beside buttons");
}
