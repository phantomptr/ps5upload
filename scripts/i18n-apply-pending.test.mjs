import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, test } from "node:test";
import { applyPending } from "./i18n-apply-pending.mjs";
import { readLocale } from "./i18n-locale-file.mjs";
import { recordHashes } from "./i18n-source-hashes.mjs";

const tempDirs = [];
const tempDir = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "i18n-apply-"));
  tempDirs.push(dir);
  return dir;
};
after(() => {
  for (const dir of tempDirs) fs.rmSync(dir, { recursive: true, force: true });
});

const EN_TS = `import type { Translations } from "../types";

const en: Translations = {

// Section
alpha: "Alpha {n}",
beta:
  "Beta on its own line",
"dotted.key": "Dotted",
  cw_b: "Console B",
  cw_d: "Console D",
last: "Last",
};

export default en;
`;
// Same keys minus the ones the tests add, in another layout: no trailing comma at the end.
const DE_TS = `import type { Translations } from "../types";

const de: Translations = {

// Abschnitt
alpha: "Alfa {n}",
  "last": "Letzte"
};

export default de;
`;
const json = (o) => `${JSON.stringify(o, null, 2)}\n`;

function makeRepo() {
  const root = tempDir();
  const put = (rel, text) => {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  };
  put("client/src/i18n/locales/en.ts", EN_TS);
  put("client/src/i18n/locales/de.ts", DE_TS);
  put("client/src/console/locales/en.json", json({ cw_b: "Console B", cw_d: "Console D" }));
  put("client/src/console/locales/de.json", json({ cw_b: "Konsole B", cw_d: "Konsole D" }));
  put("site/i18n/en.json", json({ "a.one": "One", "a.two": "Two <b>x</b> {n}", "a.zed": "Zed" }));
  put("site/i18n/de.json", json({ "a.one": "Eins", "a.zed": "Zett" }));
  const pending = path.join(root, "pending");
  fs.mkdirSync(pending);
  return {
    root,
    pending,
    sidecar: (name, doc) => fs.writeFileSync(path.join(pending, name), JSON.stringify(doc)),
    read: (rel) => fs.readFileSync(path.join(root, rel), "utf8"),
    snapshot: () =>
      ["client/src/i18n/locales/de.ts", "client/src/console/locales/de.json", "client/src/console/locales/en.json", "site/i18n/de.json"].map((f) =>
        fs.readFileSync(path.join(root, f), "utf8"),
      ),
  };
}

test("puts a key beside its English neighbour, keeping every other line as it was", () => {
  const repo = makeRepo();
  repo.sidecar("a.json", { app: { beta: { de: "Beta" }, "dotted.key": { de: "Gepunktet" } } });
  const r = applyPending({ root: repo.root, dir: repo.pending });
  assert.deepEqual(r.refused, []);
  assert.equal(r.inserted, 2);
  const out = repo.read("client/src/i18n/locales/de.ts");
  assert.equal(
    out,
    DE_TS.replace('alpha: "Alfa {n}",\n', 'alpha: "Alfa {n}",\nbeta: "Beta",\n"dotted.key": "Gepunktet",\n'),
  );
});

test("a key after the last entry adds the missing comma and stays valid", () => {
  const repo = makeRepo();
  fs.writeFileSync(path.join(repo.root, "client/src/i18n/locales/en.ts"), EN_TS.replace('last: "Last",\n', 'last: "Last",\nomega: "Omega",\n'));
  repo.sidecar("a.json", { app: { omega: { de: "Omega!" } } });
  assert.deepEqual(applyPending({ root: repo.root, dir: repo.pending }).refused, []);
  const dict = readLocale(path.join(repo.root, "client/src/i18n/locales/de.ts"));
  assert.equal(dict.omega, "Omega!");
  assert.equal(dict.last, "Letzte");
  assert.match(repo.read("client/src/i18n/locales/de.ts"), /"last": "Letzte",\n {2}"omega": "Omega!",\n};/);
});

test("a key with no earlier neighbour goes first", () => {
  const repo = makeRepo();
  const en = EN_TS.replace('alpha: "Alpha {n}",\n', 'first: "First",\nalpha: "Alpha {n}",\n');
  fs.writeFileSync(path.join(repo.root, "client/src/i18n/locales/en.ts"), en);
  repo.sidecar("a.json", { app: { first: { de: "Erste" } } });
  assert.deepEqual(applyPending({ root: repo.root, dir: repo.pending }).refused, []);
  assert.match(repo.read("client/src/i18n/locales/de.ts"), /first: "Erste",\nalpha:/);
});

test("console keys reach both the app locale and the generated JSON, in sorted place", () => {
  const repo = makeRepo();
  fs.writeFileSync(path.join(repo.root, "client/src/i18n/locales/en.ts"), EN_TS.replace('  cw_d: "Console D",\n', '  cw_c: "Console C",\n  cw_d: "Console D",\n'));
  repo.sidecar("a.json", { console: { cw_c: { de: "Konsole C" } } });
  assert.deepEqual(applyPending({ root: repo.root, dir: repo.pending }).refused, []);
  assert.equal(readLocale(path.join(repo.root, "client/src/i18n/locales/de.ts")).cw_c, "Konsole C");
  assert.equal(repo.read("client/src/console/locales/de.json"), json({ cw_b: "Konsole B", cw_c: "Konsole C", cw_d: "Konsole D" }));
  assert.equal(repo.read("client/src/console/locales/en.json"), json({ cw_b: "Console B", cw_c: "Console C", cw_d: "Console D" }));
});

test("site keys land in the site file, with tags and placeholders checked", () => {
  const repo = makeRepo();
  repo.sidecar("a.json", { site: { "a.two": { de: "Zwei <b>x</b> {n}" } } });
  assert.deepEqual(applyPending({ root: repo.root, dir: repo.pending }).refused, []);
  assert.equal(repo.read("site/i18n/de.json"), json({ "a.one": "Eins", "a.two": "Zwei <b>x</b> {n}", "a.zed": "Zett" }));

  const again = makeRepo();
  again.sidecar("a.json", { site: { "a.two": { de: "Zwei x {n}" } } });
  const before = again.snapshot();
  assert.equal(applyPending({ root: again.root, dir: again.pending }).refused.length, 1);
  assert.deepEqual(again.snapshot(), before);
});

test("applying twice changes nothing the second time", () => {
  const repo = makeRepo();
  repo.sidecar("a.json", { app: { beta: { de: "Beta" } }, site: { "a.two": { de: "Zwei <b>x</b> {n}" } } });
  assert.equal(applyPending({ root: repo.root, dir: repo.pending }).inserted, 2);
  const once = repo.snapshot();
  const second = applyPending({ root: repo.root, dir: repo.pending });
  assert.deepEqual(second.refused, []);
  assert.equal(second.inserted, 0);
  assert.deepEqual(second.changedFiles, []);
  assert.deepEqual(repo.snapshot(), once);
});

test("--check writes nothing", () => {
  const repo = makeRepo();
  repo.sidecar("a.json", { app: { beta: { de: "Beta" } } });
  const before = repo.snapshot();
  const r = applyPending({ root: repo.root, dir: repo.pending, check: true });
  assert.equal(r.inserted, 1);
  assert.deepEqual(repo.snapshot(), before);
});

for (const [name, doc, why] of [
  ["a dropped placeholder", { app: { alpha: { de: "Alfa" } } }, /placeholders differ/],
  ["a renamed placeholder", { app: { alpha: { de: "Alfa {m}" } } }, /placeholders differ/],
  ["a key already present with a different value", { app: { last: { de: "Andere" } } }, /already there with a different value/],
  ["a key English does not have", { app: { nope: { de: "Nein" } } }, /not in the English catalogue/],
  ["a language with no file", { app: { beta: { xx: "?" } } }, /no client\/src\/i18n\/locales\/xx\.ts/],
  ["an empty text", { app: { beta: { de: " " } } }, /not a text/],
  ["a console key without the prefix", { console: { beta: { de: "Beta" } } }, /cw_/],
]) {
  test(`refuses ${name}, and one bad entry stops the whole run`, () => {
    const repo = makeRepo();
    repo.sidecar("good.json", { app: { "dotted.key": { de: "Gepunktet" } } });
    repo.sidecar("bad.json", doc);
    const before = repo.snapshot();
    const r = applyPending({ root: repo.root, dir: repo.pending });
    assert.equal(r.refused.length, 1, r.refused.join("\n"));
    assert.match(r.refused[0], why);
    assert.deepEqual(repo.snapshot(), before);
  });
}

test("English changed only in the console's en.json moves to en.ts, with its translations", () => {
  const repo = makeRepo();
  recordHashes(repo.root);
  fs.writeFileSync(path.join(repo.root, "client/src/console/locales/en.json"), json({ cw_b: "Console B", cw_d: "Console D, now longer" }));
  repo.sidecar("a.json", { console: { cw_d: { de: "Konsole D, jetzt länger" } } });

  // The plain rule still holds without the flag, and nothing is written.
  const before = repo.snapshot();
  assert.equal(applyPending({ root: repo.root, dir: repo.pending }).refused.length, 1);
  assert.deepEqual(repo.snapshot(), before);

  assert.deepEqual(applyPending({ root: repo.root, dir: repo.pending, replaceDrifted: true }).refused, []);
  assert.equal(readLocale(path.join(repo.root, "client/src/i18n/locales/en.ts")).cw_d, "Console D, now longer");
  assert.equal(readLocale(path.join(repo.root, "client/src/i18n/locales/de.ts")).cw_d, "Konsole D, jetzt länger");
  assert.equal(readLocale(path.join(repo.root, "client/src/console/locales/de.json")).cw_d, "Konsole D, jetzt länger");

  const again = applyPending({ root: repo.root, dir: repo.pending, replaceDrifted: true });
  assert.deepEqual(again.refused, []);
  assert.deepEqual(again.changedFiles, []);
});

test("a new key only the console's en.json has goes to en.ts beside its neighbours", () => {
  const repo = makeRepo();
  recordHashes(repo.root);
  fs.writeFileSync(path.join(repo.root, "client/src/console/locales/en.json"), json({ cw_b: "Console B", cw_c: "Console C", cw_d: "Console D" }));
  repo.sidecar("a.json", { console: { cw_c: { de: "Konsole C" } } });
  assert.deepEqual(applyPending({ root: repo.root, dir: repo.pending }).refused, []);
  assert.match(repo.read("client/src/i18n/locales/en.ts"), /cw_b: "Console B",\n {2}cw_c: "Console C",\n {2}cw_d:/);
  assert.equal(readLocale(path.join(repo.root, "client/src/i18n/locales/de.ts")).cw_c, "Konsole C");
  assert.equal(readLocale(path.join(repo.root, "client/src/console/locales/de.json")).cw_c, "Konsole C");
});

test("when en.ts moved, en.ts wins over a console en.json that is behind", () => {
  const repo = makeRepo();
  fs.writeFileSync(path.join(repo.root, "client/src/i18n/locales/de.ts"), DE_TS.replace('alpha: "Alfa {n}",\n', 'alpha: "Alfa {n}",\ncw_d: "Konsole D",\n'));
  recordHashes(repo.root);
  const enFile = path.join(repo.root, "client/src/i18n/locales/en.ts");
  fs.writeFileSync(enFile, EN_TS.replace('cw_d: "Console D"', 'cw_d: "Console D2"'));
  repo.sidecar("a.json", { console: { cw_d: { de: "Konsole D2" } } });
  assert.deepEqual(applyPending({ root: repo.root, dir: repo.pending, replaceDrifted: true }).refused, []);
  assert.equal(readLocale(enFile).cw_d, "Console D2");
  assert.equal(readLocale(path.join(repo.root, "client/src/console/locales/en.json")).cw_d, "Console D2");
  assert.equal(readLocale(path.join(repo.root, "client/src/console/locales/de.json")).cw_d, "Konsole D2");
});

test("two sidecars giving different texts for one key are refused; the same text is fine", () => {
  const repo = makeRepo();
  repo.sidecar("a.json", { app: { beta: { de: "Beta" } } });
  repo.sidecar("b.json", { app: { beta: { de: "Béta" } } });
  assert.match(applyPending({ root: repo.root, dir: repo.pending }).refused[0], /a\.json and b\.json/);

  const same = makeRepo();
  same.sidecar("a.json", { app: { beta: { de: "Beta" } } });
  same.sidecar("b.json", { app: { beta: { de: "Beta" } } });
  const r = applyPending({ root: same.root, dir: same.pending });
  assert.deepEqual(r.refused, []);
  assert.equal(r.inserted, 1);
});

test("--replace-drifted replaces only a translation whose English moved", () => {
  const repo = makeRepo();
  recordHashes(repo.root);
  const enFile = path.join(repo.root, "client/src/i18n/locales/en.ts");
  fs.writeFileSync(enFile, EN_TS.replace('last: "Last",', 'last: "Final",'));
  repo.sidecar("a.json", { app: { last: { de: "Endgültig" } } });

  // Without the flag the plain rule holds.
  assert.equal(applyPending({ root: repo.root, dir: repo.pending }).refused.length, 1);

  const r = applyPending({ root: repo.root, dir: repo.pending, replaceDrifted: true });
  assert.deepEqual(r.refused, []);
  assert.equal(r.replaced, 1);
  assert.equal(readLocale(path.join(repo.root, "client/src/i18n/locales/de.ts")).last, "Endgültig");

  // alpha's English did not move, so its translation stays protected.
  const other = path.join(repo.pending, "b.json");
  fs.writeFileSync(other, JSON.stringify({ app: { alpha: { de: "Anders {n}" } } }));
  assert.match(applyPending({ root: repo.root, dir: repo.pending, replaceDrifted: true }).refused[0], /already there/);
});
