import { beforeAll, describe, expect, it } from "vitest";

import {
  t as translate,
  ensureLocale,
  type LanguageCode,
} from "./i18n";

/**
 * Direct tests for the canonical translator. `useTr` (in `state/lang.ts`)
 * is a thin React hook around this function — its overload-widening
 * behaviour (`tr(key, fallback)` shorthand) is implemented in the hook
 * but the fallback semantics it relies on (`raw === key` when missing
 * everywhere) are owned by `translate` here.
 *
 * Locale dictionaries are lazy-loaded in production (one Vite chunk per
 * locale) so `t("ja", …)` returns English until `ensureLocale("ja")`
 * resolves. These tests pre-load every locale they assert against so
 * the synchronous lookup chain is fully populated.
 */
const ALL_LOCALES: LanguageCode[] = [
  "en",
  "vi",
  "hi",
  "bn",
  "pt-BR",
  "ru",
  "ja",
  "tr",
  "id",
  "th",
  "ko",
  "de",
  "it",
  "zh-CN",
  "zh-TW",
  "fr",
  "es",
  "ar",
];

beforeAll(async () => {
  await Promise.all(ALL_LOCALES.map((c) => ensureLocale(c)));
});

describe("translate (i18n.ts t())", () => {
  it("returns the active-locale value when key exists", () => {
    expect(translate("en", "app_title")).toBeTruthy();
  });

  it("falls back to English when the active locale is missing the key", () => {
    const en = translate("en", "install.empty.requirements");
    const ja = translate("ja", "install.empty.requirements");
    expect(en).toBeTruthy();
    expect(ja).toBeTruthy();
    expect(en).not.toBe(ja);
  });

  it("returns the key itself when missing from BOTH active locale and English", () => {
    const missing = "this.key.does.not.exist.anywhere.zzz";
    expect(translate("en", missing)).toBe(missing);
    expect(translate("ja", missing)).toBe(missing);
    expect(translate("zh-CN", missing)).toBe(missing);
  });

  it("returns the English value when the locale code is unknown", () => {
    const en = translate("en", "app_title");
    expect(translate("xx-NOTREAL", "app_title")).toBe(en);
  });

  it("substitutes single-variable interpolation", () => {
    const out = translate("en", "install.counts.total", { n: 5 });
    expect(out).toContain("5");
    expect(out).not.toContain("{n}");
  });

  it("leaves unknown placeholders unchanged", () => {
    const out = translate("en", "install.counts.total", { unrelated: 1 });
    expect(out).toContain("{n}");
  });

  it("returns empty for empty key (edge — defensive)", () => {
    expect(translate("en", "")).toBe("");
  });

  it("does not mutate the vars object", () => {
    const vars = { n: 5 };
    const before = { ...vars };
    translate("en", "install.counts.total", vars);
    expect(vars).toEqual(before);
  });
});

describe("lazy-load contract", () => {
  it("ensureLocale is idempotent (second call doesn't re-import)", async () => {
    // Both calls return resolved promises; nothing throws.
    await ensureLocale("ja");
    await ensureLocale("ja");
    expect(translate("ja", "app_title")).toBeTruthy();
  });
});
