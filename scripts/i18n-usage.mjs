/*
 * Which keys of client/src/i18n/locales/en.ts the app can still reach.
 * scripts/find-orphan-i18n.mjs and scripts/i18n-prune-unused.mjs share this, so the gate and the
 * pruner always agree on what "dead" means.
 *
 * A key counts as used when production code under client/src contains either
 *   - the key as a quoted string, anywhere. Tables like `{ labelKey: "x" }` and `tr(item.key)` end
 *     up here, so a `tr(IDENTIFIER)` call site needs no list of its own; or
 *   - a template or concatenation that can build it: `activity_kind_${kind}`, "fs_" + x,
 *     `${group}_title`. The prefixes come from the code itself, never from a list by hand.
 * Over-keeping is the safe direction: a dead key costs a translator time, a wrongly deleted one
 * shows English.
 */
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

/** Shorter static parts are noise: `a${n}` in a cache key would keep every key starting with "a". */
const MIN_PART = 3;

function sourceFiles(repoRoot) {
  const res = spawnSync("git", ["ls-files", "-c", "-o", "--exclude-standard", "client/src"], {
    cwd: repoRoot,
    encoding: "utf8",
    maxBuffer: 1 << 28,
  });
  if (res.status !== 0) throw new Error(res.stderr || "git ls-files failed");
  return res.stdout
    .split("\n")
    .filter(
      (f) =>
        /\.(tsx?|mjs|js)$/.test(f) &&
        !/\.test\./.test(f) &&
        !f.startsWith("client/src/i18n/locales/") &&
        !f.startsWith("client/src/console/locales/") &&
        fs.existsSync(path.join(repoRoot, f)),
    );
}

/** The quoted strings and the dynamic key parts in one source text. */
export function scanSource(text) {
  const literals = new Set();
  const heads = new Set();
  const tails = new Set();
  for (const m of text.matchAll(/["'`]([A-Za-z][\w.-]*)["'`]/g)) literals.add(m[1]);
  for (const m of text.matchAll(/`([A-Za-z][\w.-]*)\$\{/g)) heads.add(m[1]);
  for (const m of text.matchAll(/["']([A-Za-z][\w.-]*[._])["']\s*\+/g)) heads.add(m[1]);
  for (const m of text.matchAll(/\$\{[^}`]+\}([_.][\w.-]+)`/g)) tails.add(m[1]);
  for (const m of text.matchAll(/\+\s*["']([_.][\w.-]+)["']/g)) tails.add(m[1]);
  return { literals, heads, tails };
}

/** Keys written as a literal in a `tr("k")`, `trStatic("k")`, `t("k")` or `t(lang, "k")` call. */
export function usedKeysIn(text) {
  const used = new Set();
  for (const m of text.matchAll(/\b(?:tr|t|trStatic)\(\s*"([a-zA-Z0-9_.]+)"/g)) used.add(m[1]);
  for (const m of text.matchAll(/\bt\(\s*[A-Za-z_$][\w$.]*\s*,\s*"([a-zA-Z0-9_.]+)"/g)) used.add(m[1]);
  return used;
}

export function scanClientSource(repoRoot) {
  const literals = new Set();
  const heads = new Set();
  const tails = new Set();
  for (const f of sourceFiles(repoRoot)) {
    const found = scanSource(fs.readFileSync(path.join(repoRoot, f), "utf8"));
    for (const k of found.literals) literals.add(k);
    for (const k of found.heads) if (k.length >= MIN_PART) heads.add(k);
    for (const k of found.tails) if (k.length >= MIN_PART) tails.add(k);
  }
  return { literals, heads: [...heads], tails: [...tails] };
}

/** Sort `keys` into dead (nothing can reach them), and dynamic (only a template or concat can). */
export function classifyKeys(keys, { literals, heads, tails }) {
  const dead = [];
  const dynamic = [];
  for (const k of keys) {
    if (literals.has(k)) continue;
    if (heads.some((h) => k.startsWith(h)) || tails.some((t) => k.endsWith(t))) dynamic.push(k);
    else dead.push(k);
  }
  return { dead: dead.sort(), dynamic: dynamic.sort() };
}

/**
 * scripts/i18n-known-missing.json without `deadKeys`. Every bucket a language has survives (missing,
 * stale, untranslated, the old flat array); only the deleted keys leave them.
 */
export function withoutKeys(allowlist, deadKeys) {
  const dead = new Set(deadKeys);
  let removed = 0;
  const keep = (list) => {
    const kept = list.filter((k) => !dead.has(k));
    removed += list.length - kept.length;
    return kept;
  };
  const out = {};
  for (const [lang, entry] of Object.entries(allowlist)) {
    out[lang] = Array.isArray(entry)
      ? keep(entry)
      : Object.fromEntries(Object.entries(entry).map(([bucket, list]) => [bucket, keep(list)]));
  }
  return { allowlist: out, removed };
}
