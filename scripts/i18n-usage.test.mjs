import assert from "node:assert/strict";
import { test } from "node:test";
import { classifyKeys, scanSource, usedKeysIn, withoutKeys } from "./i18n-usage.mjs";

const classify = (keys, text) => {
  const { literals, heads, tails } = scanSource(text);
  return classifyKeys(keys, { literals, heads: [...heads], tails: [...tails] });
};

test("a key in a table counts as used, whatever passes it to tr()", () => {
  const out = classify(["nav_a", "nav_b"], 'const items = [{ key: "nav_a" }]; items.map((i) => tr(i.key));');
  assert.deepEqual(out, { dead: ["nav_b"], dynamic: [] });
});

test("a template head keeps its whole family (activity_kind_* looked dead without this)", () => {
  const src = 'export const f = (kind) => `activity_kind_${kind.replace(/-/g, "_")}`;';
  const out = classify(["activity_kind_upload_file", "activity_other"], src);
  assert.deepEqual(out, { dead: ["activity_other"], dynamic: ["activity_kind_upload_file"] });
});

test("concatenation and a template tail keep their keys too", () => {
  assert.deepEqual(classify(["fs_kind_a"], 'tr("fs_kind_" + x)').dead, []);
  assert.deepEqual(classify(["x_row_title"], "tr(`${id}_title`)").dead, []);
});

test("usedKeysIn reads every call shape the gate has to see", () => {
  const src = `tr("a.one"); tr(
    "two", undefined, "Two"); t("cw_three"); trStatic("four"); t(lang, "five"); tr(item.key, "Fallback");`;
  assert.deepEqual([...usedKeysIn(src)].sort(), ["a.one", "cw_three", "five", "four", "two"]);
});

test("pruning keeps every allowlist bucket and only removes the dead keys", () => {
  const { allowlist, removed } = withoutKeys(
    {
      de: { missing: ["dead", "a"], stale: ["b"], untranslated: ["dead", "c"] },
      ru: ["dead", "d"],
    },
    ["dead"],
  );
  assert.equal(removed, 3);
  assert.deepEqual(allowlist, {
    de: { missing: ["a"], stale: ["b"], untranslated: ["c"] },
    ru: ["d"],
  });
});
