/*
 * Reads and edits one locale file without reformatting it. The i18n scripts share this.
 *
 * Two shapes: client/src/i18n/locales/<code>.ts (`const de: Translations = { key: "value", … };`,
 * with comments, bare or quoted keys, and values that may start on the line after the key) and the
 * flat JSON files (client/src/console/locales, site/i18n).
 *
 * A locale file is hand-merged by many branches, so every edit here touches only the lines of the
 * entries it changes: nothing is reordered and no prettier ever runs on these files.
 */
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";

const BARE_KEY = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

function skipTrivia(src, from) {
  let i = from;
  for (;;) {
    while (i < src.length && /\s/.test(src[i])) i++;
    if (src.startsWith("//", i)) {
      const nl = src.indexOf("\n", i);
      i = nl < 0 ? src.length : nl + 1;
    } else if (src.startsWith("/*", i)) {
      const end = src.indexOf("*/", i + 2);
      if (end < 0) throw new Error("unterminated comment");
      i = end + 2;
    } else {
      return i;
    }
  }
}

/** The index just past the quoted string starting at `from`. */
function skipString(src, from) {
  const quote = src[from];
  for (let i = from + 1; i < src.length; i++) {
    if (src[i] === "\\") i++;
    else if (src[i] === quote) return i + 1;
    else if (src[i] === "\n") break;
  }
  throw new Error(`unterminated string at offset ${from}`);
}

function decodeString(raw) {
  try {
    return JSON.parse(raw);
  } catch {
    // TS allows what JSON does not: 'single quotes', \' and friends. Same sandbox the other i18n scripts use.
    return vm.runInNewContext(raw, Object.create(null), { timeout: 1000 });
  }
}

/** Where the object literal opens: after `Translations =` in a .ts file, the first `{` in JSON. */
function findOpen(src, isJson) {
  if (isJson) {
    const at = src.indexOf("{");
    if (at < 0) throw new Error("no object in the file");
    return at;
  }
  const m = src.match(/^const\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*Translations\s*=\s*\{/m);
  if (!m) throw new Error("could not find 'const <name>: Translations = {'");
  return m.index + m[0].length - 1;
}

/**
 * Every entry of the top-level object, in file order. Offsets:
 *   keyStart/keyEnd      the key token
 *   valueStart/valueEnd  the value's string token
 *   comma                offset of the comma after the value, or -1
 *   lineStart            start of the line the key is on
 *   lineEnd              just past the newline of the line the value ends on
 */
export function scanLocale(src, { json = false } = {}) {
  const open = findOpen(src, json);
  const entries = [];
  let i = open + 1;
  for (;;) {
    i = skipTrivia(src, i);
    if (src[i] === "}") break;
    if (i >= src.length) throw new Error("the object is never closed");
    const keyStart = i;
    let key;
    if (src[i] === '"' || src[i] === "'") {
      i = skipString(src, i);
      key = decodeString(src.slice(keyStart, i));
    } else {
      while (i < src.length && /[A-Za-z0-9_$]/.test(src[i])) i++;
      key = src.slice(keyStart, i);
      if (!key) throw new Error(`expected a key at offset ${keyStart}: ${src.slice(keyStart, keyStart + 40)}`);
    }
    const keyEnd = i;
    i = skipTrivia(src, i);
    if (src[i] !== ":") throw new Error(`${key}: expected ":"`);
    i = skipTrivia(src, i + 1);
    if (src[i] !== '"' && src[i] !== "'") throw new Error(`${key}: the value is not a quoted string`);
    const valueStart = i;
    i = skipString(src, i);
    const valueEnd = i;
    const value = decodeString(src.slice(valueStart, valueEnd));
    const afterValue = skipTrivia(src, i);
    const comma = src[afterValue] === "," ? afterValue : -1;
    if (src[afterValue] !== "," && src[afterValue] !== "}") {
      throw new Error(`${key}: the value is not one plain string`);
    }
    if (comma >= 0) i = comma + 1;
    const nl = src.indexOf("\n", i);
    const lineEnd = nl < 0 ? src.length : nl + 1;
    const lineStart = src.lastIndexOf("\n", keyStart - 1) + 1;
    entries.push({ key, keyStart, keyEnd, valueStart, valueEnd, value, comma, lineStart, lineEnd });
  }
  return { open, close: i, entries };
}

/** Keys that appear more than once: valid JS, but the later one silently wins. */
export function duplicateKeys(entries) {
  const seen = new Set();
  const dups = new Set();
  for (const { key } of entries) {
    if (seen.has(key)) dups.add(key);
    seen.add(key);
  }
  return [...dups].sort();
}

/** { key: value } of a locale file on disk (.ts or .json). */
export function readLocale(file) {
  const src = fs.readFileSync(file, "utf8");
  const dict = {};
  for (const e of scanLocale(src, { json: file.endsWith(".json") }).entries) dict[e.key] = e.value;
  return dict;
}

/** The language codes that have a file in `dir`, for the given extension. */
export function localeCodes(dir, ext) {
  return fs
    .readdirSync(dir)
    .filter((f) => f.endsWith(ext) && !f.endsWith(".d.ts"))
    .map((f) => path.basename(f, ext))
    .sort();
}

function applyEdits(src, edits) {
  let out = src;
  for (const e of [...edits].sort((a, b) => b.start - a.start)) {
    out = out.slice(0, e.start) + e.text + out.slice(e.end);
  }
  return out;
}

/** Replace the values of existing keys, in place. A value on the next line stays on the next line. */
export function setValues(src, values, { json = false } = {}) {
  const edits = [];
  const left = new Set(Object.keys(values));
  for (const e of scanLocale(src, { json }).entries) {
    if (!left.has(e.key)) continue;
    left.delete(e.key);
    if (e.value !== values[e.key]) edits.push({ start: e.valueStart, end: e.valueEnd, text: JSON.stringify(values[e.key]) });
  }
  if (left.size) throw new Error(`not in the file: ${[...left].join(", ")}`);
  return applyEdits(src, edits);
}

/** Delete whole entries (only their own lines). */
export function removeKeys(src, keys, { json = false } = {}) {
  const wanted = new Set(keys);
  const { entries } = scanLocale(src, { json });
  const edits = [];
  entries.forEach((e, at) => {
    if (!wanted.has(e.key)) return;
    if (src.slice(e.lineStart, e.keyStart).trim()) throw new Error(`${e.key}: shares its line with something else`);
    const rest = src.slice(e.comma >= 0 ? e.comma + 1 : e.valueEnd, e.lineEnd).trim();
    if (rest && !rest.startsWith("//")) throw new Error(`${e.key}: shares its line with something else`);
    edits.push({ start: e.lineStart, end: e.lineEnd, text: "" });
    // JSON has no trailing comma: deleting the last entry takes the comma of the one before it.
    if (json && at === entries.length - 1) {
      let before = at - 1;
      while (before >= 0 && wanted.has(entries[before].key)) before--;
      if (before >= 0 && entries[before].comma >= 0) {
        edits.push({ start: entries[before].comma, end: entries[before].comma + 1, text: "" });
      }
    }
  });
  return applyEdits(src, edits);
}

function renderEntry(key, value, indent, { json, quoted }) {
  const k = json || quoted || !BARE_KEY.test(key) ? JSON.stringify(key) : key;
  return `${indent}${k}: ${JSON.stringify(value)}`;
}

/**
 * Add new entries, each right after the entry `after` names, or before the first entry when `after`
 * is null. Items are added one at a time, so an item may name one added before it.
 * Indent and key quoting follow the neighbour, as the files mix both.
 */
export function insertEntries(src, items, { json = false } = {}) {
  let out = src;
  for (const { key, value, after } of items) {
    const scan = scanLocale(out, { json });
    const { entries } = scan;
    if (entries.some((e) => e.key === key)) throw new Error(`${key}: already in the file`);
    if (entries.length === 0) {
      const line = renderEntry(key, value, "  ", { json, quoted: false });
      out = `${out.slice(0, scan.open + 1)}\n${line}${json ? "" : ","}\n${out.slice(scan.close)}`;
      continue;
    }
    const anchorAt = after == null ? 0 : entries.findIndex((e) => e.key === after);
    if (anchorAt < 0) throw new Error(`${key}: the entry to insert after (${after}) is not in the file`);
    const anchor = entries[anchorAt];
    const indent = out.slice(anchor.lineStart, anchor.keyStart);
    const quoted = out[anchor.keyStart] === '"';
    const line = renderEntry(key, value, indent, { json, quoted });
    if (after == null) {
      out = `${out.slice(0, anchor.lineStart)}${line},\n${out.slice(anchor.lineStart)}`;
      continue;
    }
    const isLast = anchorAt === entries.length - 1;
    // Past the last JSON entry the new line takes over as the one with no comma.
    const needsComma = !json || !isLast;
    const newLine = `${line}${needsComma ? "," : ""}\n`;
    let head = out.slice(0, anchor.lineEnd);
    if (anchor.comma < 0) {
      head = `${out.slice(0, anchor.valueEnd)},${out.slice(anchor.valueEnd, anchor.lineEnd)}`;
    }
    const sep = head.endsWith("\n") ? "" : "\n";
    out = `${head}${sep}${newLine}${out.slice(anchor.lineEnd)}`;
  }
  return out;
}
