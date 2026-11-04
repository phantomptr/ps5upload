import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import vm from "node:vm";
import { duplicateKeys, insertEntries, localeCodes, removeKeys, scanLocale, setValues } from "./i18n-locale-file.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..");
const LOCALES = path.join(repoRoot, "client/src/i18n/locales");

const wrap = (body) => `const en: Translations = {\n${body}\n};\n\nexport default en;\n`;
const keys = (src) => scanLocale(src).entries.map((e) => e.key);

test("reads every real locale exactly as the JS engine does", () => {
  for (const code of localeCodes(LOCALES, ".ts")) {
    const src = fs.readFileSync(path.join(LOCALES, `${code}.ts`), "utf8");
    const start = src.match(/^const\s+\w+\s*:\s*Translations\s*=\s*/m);
    const from = start.index + start[0].length;
    const end = src.slice(from).match(/\n};\s*\n\s*export default/);
    const expected = vm.runInNewContext(`(${src.slice(from, from + end.index + 2)})`);
    const got = Object.fromEntries(scanLocale(src).entries.map((e) => [e.key, e.value]));
    assert.deepEqual({ ...got }, { ...expected }, code);
  }
});

test("a continuation line starting 'Word:' is not a key", () => {
  const src = wrap(`one:\n  "Heads-up: first",\ntwo:\n  "Heads-up: second",\n// note: not a key\nthree: 'single "quoted" {x}',`);
  const { entries } = scanLocale(src);
  assert.deepEqual(entries.map((e) => e.key), ["one", "two", "three"]);
  assert.deepEqual(duplicateKeys(entries), []);
  assert.equal(entries[2].value, 'single "quoted" {x}');
});

test("a real duplicate is reported", () => {
  assert.deepEqual(duplicateKeys(scanLocale(wrap('a: "1",\nb: "2",\n"a": "3",')).entries), ["a"]);
});

test("setValues changes only the value, and a value on the next line stays there", () => {
  const src = wrap('one:\n  "Old",\n"two.dotted": "Keep",');
  assert.equal(setValues(src, { one: 'New "x"\nline' }), wrap('one:\n  "New \\"x\\"\\nline",\n"two.dotted": "Keep",'));
});

test("removeKeys drops whole entries, including a value on its own line", () => {
  const src = wrap('// section\none:\n  "Old",\ntwo: "Keep", // why\nthree: "Gone",');
  assert.equal(removeKeys(src, ["one", "three"]), wrap('// section\ntwo: "Keep", // why'));
});

test("removeKeys on JSON keeps it valid when the last entry goes", () => {
  const out = removeKeys('{\n  "a": "1",\n  "b": "2"\n}\n', ["b"], { json: true });
  assert.deepEqual(JSON.parse(out), { a: "1" });
});

test("insertEntries follows the neighbour's indent and quoting, and quotes a dotted key", () => {
  const src = wrap('a: "1",\n  "b": "2",');
  const out = insertEntries(src, [
    { key: "c", value: "3", after: "b" },
    { key: "x.y", value: "4", after: "a" },
  ]);
  assert.equal(out, wrap('a: "1",\n"x.y": "4",\n  "b": "2",\n  "c": "3",'));
  assert.deepEqual(keys(out), ["a", "x.y", "b", "c"]);
});

test("insertEntries refuses a key that is already there", () => {
  assert.throws(() => insertEntries(wrap('a: "1",'), [{ key: "a", value: "2", after: null }]), /already/);
});
