import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, test } from "node:test";
import { checkSourceHashes, describeProblems, dropKeys, recordHashes } from "./i18n-source-hashes.mjs";

const tempDirs = [];
const tempDir = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "i18n-hash-"));
  tempDirs.push(dir);
  return dir;
};
after(() => {
  for (const dir of tempDirs) fs.rmSync(dir, { recursive: true, force: true });
});

const locale = (id, entries) =>
  `const ${id}: Translations = {\n${Object.entries(entries).map(([k, v]) => `${k}: ${JSON.stringify(v)},`).join("\n")}\n};\n\nexport default ${id};\n`;

function makeRepo() {
  const root = tempDir();
  const put = (rel, text) => {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), text);
  };
  const app = (code, entries) => put(`client/src/i18n/locales/${code}.ts`, locale(code, entries));
  app("en", { save: "Save", conn_password: "Password", stop: "Stop" });
  app("de", { save: "Speichern", conn_password: "Passwort", stop: "Stopp" });
  app("fr", { save: "Enregistrer", conn_password: "Mot de passe", stop: "Arrêter" });
  put("site/i18n/en.json", '{\n  "a": "Site text"\n}\n');
  put("site/i18n/de.json", '{\n  "a": "Seitentext"\n}\n');
  recordHashes(root);
  return { root, app, put };
}

const clean = (root) => {
  const r = checkSourceHashes(root);
  assert.deepEqual([r.missingLedger, r.drift, r.unrecorded], [false, [], []], describeProblems(r));
};

test("a fresh record is clean", () => {
  clean(makeRepo().root);
});

test("no record at all fails with the command that makes one", () => {
  const root = tempDir();
  fs.mkdirSync(path.join(root, "client/src/i18n/locales"), { recursive: true });
  fs.writeFileSync(path.join(root, "client/src/i18n/locales/en.ts"), locale("en", { a: "A" }));
  const r = checkSourceHashes(root);
  assert.equal(r.missingLedger, true);
  assert.match(describeProblems(r), /--update/);
});

test("English changing while translations don't is drift, naming the languages", () => {
  const { root, app } = makeRepo();
  app("en", { save: "Save it now", conn_password: "Password", stop: "Stop" });
  const r = checkSourceHashes(root);
  assert.deepEqual(r.drift.map((d) => [d.set, d.key, d.langs]), [["app", "save", ["de", "fr"]]]);
  assert.match(describeProblems(r), /save \(de, fr\)/);
});

test("drift in the site strings is caught too", () => {
  const { root, put } = makeRepo();
  put("site/i18n/en.json", '{\n  "a": "Other site text"\n}\n');
  assert.deepEqual(checkSourceHashes(root).drift.map((d) => [d.set, d.key]), [["site", "a"]]);
});

test("one language left behind is drift for that language only", () => {
  const { root, app } = makeRepo();
  app("en", { save: "Save it now", conn_password: "Password", stop: "Stop" });
  app("de", { save: "Jetzt speichern", conn_password: "Passwort", stop: "Stopp" });
  assert.deepEqual(checkSourceHashes(root).drift.map((d) => d.langs), [["fr"]]);
});

test("--update does not bless drift; the other keys get recorded", () => {
  const { root, app } = makeRepo();
  app("en", { save: "Save it now", conn_password: "Password", stop: "Stop", added: "New" });
  app("de", { save: "Speichern", conn_password: "Passwort", stop: "Stopp", added: "Neu" });
  app("fr", { save: "Enregistrer", conn_password: "Mot de passe", stop: "Arrêter", added: "Nouveau" });
  const { drift } = recordHashes(root);
  assert.deepEqual(drift.map((d) => d.key), ["save"]);
  const after = checkSourceHashes(root);
  assert.deepEqual(after.drift.map((d) => d.key), ["save"]);
  assert.deepEqual(after.unrecorded, []);
});

test("retranslating every language turns drift into a plain record", () => {
  const { root, app } = makeRepo();
  app("en", { save: "Save it now", conn_password: "Password", stop: "Stop" });
  app("de", { save: "Jetzt speichern", conn_password: "Passwort", stop: "Stopp" });
  app("fr", { save: "Enregistrer maintenant", conn_password: "Mot de passe", stop: "Arrêter" });
  const r = checkSourceHashes(root);
  assert.deepEqual(r.drift, []);
  assert.deepEqual(r.unrecorded.map((u) => u.key), ["save"]);
  assert.deepEqual(recordHashes(root).drift, []);
  clean(root);
});

test("a later English change is drift again after an earlier one was retranslated and recorded", () => {
  const { root, app } = makeRepo();
  app("en", { save: "Save it now", conn_password: "Password", stop: "Stop" });
  app("de", { save: "Jetzt speichern", conn_password: "Passwort", stop: "Stopp" });
  app("fr", { save: "Enregistrer maintenant", conn_password: "Mot de passe", stop: "Arrêter" });
  recordHashes(root);
  app("en", { save: "Save it later", conn_password: "Password", stop: "Stop" });
  assert.deepEqual(checkSourceHashes(root).drift.map((d) => d.key), ["save"]);
});

test("--ack accepts a key as it is, and only that key", () => {
  const { root, app } = makeRepo();
  app("en", { save: "SAVE", conn_password: "Password", stop: "Halt" });
  assert.equal(checkSourceHashes(root).drift.length, 2);
  assert.deepEqual(recordHashes(root, { acknowledge: ["save"] }).drift.map((d) => d.key), ["stop"]);
  assert.deepEqual(checkSourceHashes(root).drift.map((d) => d.key), ["stop"]);
});

test("--ack of a key English doesn't have is an error", () => {
  const { root } = makeRepo();
  assert.throws(() => recordHashes(root, { acknowledge: ["nope"] }), /not in English/);
});

test("a translation already equal to the new English is not drift", () => {
  const { root, app } = makeRepo();
  app("de", { save: "Speichern", conn_password: "Passwort", stop: "Stop" });
  recordHashes(root);
  app("en", { save: "Save", conn_password: "Password", stop: "Stopp" });
  app("fr", { save: "Enregistrer", conn_password: "Mot de passe", stop: "Stopp" });
  app("de", { save: "Speichern", conn_password: "Passwort", stop: "Stopp" });
  assert.deepEqual(checkSourceHashes(root).drift, []);
});

test("new, removed and retranslated keys all ask for a record, without being drift", () => {
  const { root, app } = makeRepo();
  app("en", { save: "Save", conn_password: "Password", added: "New" });
  app("de", { save: "Sichern", conn_password: "Passwort", added: "Neu" });
  const r = checkSourceHashes(root);
  assert.deepEqual(r.drift, []);
  assert.deepEqual(r.unrecorded.map((u) => u.key).sort(), ["added", "save", "stop"]);
  assert.match(describeProblems(r), /--update/);
});

test("a new language asks for a record, then is tracked", () => {
  const { root, app } = makeRepo();
  app("it", { save: "Salva", conn_password: "Parola d'ordine", stop: "Ferma" });
  assert.equal(checkSourceHashes(root).drift.length, 0);
  assert.ok(checkSourceHashes(root).unrecorded.length > 0);
  recordHashes(root);
  clean(root);
  app("en", { save: "Save now", conn_password: "Password", stop: "Stop" });
  assert.deepEqual(checkSourceHashes(root).drift[0].langs, ["de", "fr", "it"]);
});

test("dropKeys forgets a deleted key and touches nothing else", () => {
  const { root, app } = makeRepo();
  app("en", { save: "Save", conn_password: "Password" });
  app("de", { save: "Speichern", conn_password: "Passwort" });
  app("fr", { save: "Enregistrer", conn_password: "Mot de passe" });
  dropKeys(root, "app", ["stop"]);
  clean(root);
});

test("a ledger line never reads as a credential: 8 characters, then a space", () => {
  const { root } = makeRepo();
  const ledger = JSON.parse(fs.readFileSync(path.join(root, "scripts/i18n-source-hashes.json"), "utf8"));
  assert.match(ledger.app.conn_password, /^[0-9a-f]{8} .{6}$/);
});
