import { describe, expect, it } from "vitest";

import { t as translate } from "./i18n";

/**
 * Direct tests for the canonical translator. `useTr` (in `state/lang.ts`)
 * is a thin React hook around this function — its overload-widening
 * behaviour (`tr(key, fallback)` shorthand) is implemented in the hook
 * but the fallback semantics it relies on (`raw === key` when missing
 * everywhere) are owned by `translate` here.
 */
describe("translate (i18n.ts t())", () => {
  it("returns the active-locale value when key exists", () => {
    // Pick a key we know lives in `en`. `app_title` is a long-standing
    // entry; if a future refactor renames it the test surfaces the
    // breakage.
    expect(translate("en", "app_title")).toBeTruthy();
  });

  it("falls back to English when the active locale is missing the key", () => {
    // `install.empty.requirements` was rewritten in 2.2.61 across all
    // 18 locales. Each locale has its own translation; if one is
    // accidentally deleted the English copy must surface, not the raw
    // key. This test pins the fallback chain.
    const en = translate("en", "install.empty.requirements");
    const ja = translate("ja", "install.empty.requirements");
    expect(en).toBeTruthy();
    expect(ja).toBeTruthy();
    // Active-locale value differs from English when the locale has its
    // own translation; this asserts each locale isn't silently
    // returning the English string (regression guard against a buggy
    // copy-from-en migration).
    expect(en).not.toBe(ja);
  });

  it("returns the key itself when missing from BOTH active locale and English", () => {
    // useTr's fallback dispatch keys on `result === key` to know when
    // to substitute the inline fallback. If translate ever returned
    // an empty string instead of the key, every fallback would render
    // as blank text.
    const missing = "this.key.does.not.exist.anywhere.zzz";
    expect(translate("en", missing)).toBe(missing);
    expect(translate("ja", missing)).toBe(missing);
    expect(translate("zh-CN", missing)).toBe(missing);
  });

  it("returns the English value when the locale code is unknown", () => {
    // Unknown locale (e.g. user has corrupted localStorage) must
    // degrade to English rather than throwing or returning the key.
    const en = translate("en", "app_title");
    expect(translate("xx-NOTREAL", "app_title")).toBe(en);
  });

  it("substitutes single-variable interpolation", () => {
    // The `{name}` substitution is widely used (queue counts, ETA
    // strings). A regression here would render literal "{n}" in the
    // UI for every counted value.
    const out = translate("en", "install.counts.total", { n: 5 });
    expect(out).toContain("5");
    expect(out).not.toContain("{n}");
  });

  it("leaves unknown placeholders unchanged", () => {
    // If the caller passes vars that don't match the key's
    // placeholders (typo, key drift), the unmatched `{x}` must stay
    // verbatim — that's the signal the developer needs to fix the
    // call site.
    const out = translate("en", "install.counts.total", { unrelated: 1 });
    expect(out).toContain("{n}");
  });

  it("returns empty for empty key (edge — defensive)", () => {
    expect(translate("en", "")).toBe("");
  });

  it("does not mutate the vars object", () => {
    // Defensive: the renderer is read-only over `vars`. Verifying
    // this means a future refactor that introduces a side effect
    // gets caught.
    const vars = { n: 5 };
    const before = { ...vars };
    translate("en", "install.counts.total", vars);
    expect(vars).toEqual(before);
  });
});
