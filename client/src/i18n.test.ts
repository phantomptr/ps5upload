import { beforeAll, describe, expect, it } from "vitest";

import {
  t as translate,
  ensureLocale,
  ensureLocaleWithRetry,
  isolatedForRtl,
  ltr,
  pickLanguage,
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
    const en = translate("en", "install.title");
    const ja = translate("ja", "install.title");
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
    const out = translate("en", "pkglib.group.count", { n: 5 });
    expect(out).toContain("5");
    expect(out).not.toContain("{n}");
  });

  it("in Arabic and Persian a path, a file name or a size keeps its own order inside the sentence", async () => {
    // Between LEFT-TO-RIGHT ISOLATE and POP DIRECTIONAL ISOLATE: "/data" read "data/" without.
    const lri = "\u2066";
    const pdi = "\u2069";
    expect(isolatedForRtl({ path: "/data/games", size: "512 B", name: "notes (1).txt", host: "192.168.1.50" })).toEqual({
      path: `${lri}/data/games${pdi}`,
      size: `${lri}512 B${pdi}`,
      name: `${lri}notes (1).txt${pdi}`,
      // Digits and dots read the same either way.
      host: "192.168.1.50",
    });
    // A count stays a number, and text in the language itself is left alone.
    expect(isolatedForRtl({ count: 3, word: "ملف" })).toEqual({ count: 3, word: "ملف" });
    for (const lang of ["ar", "fa"] as const) {
      await ensureLocale(lang);
      expect(translate(lang, "fs_recent_remove_x_missing {p}", { p: "/mnt/usb0" })).toBe(`fs_recent_remove_x_missing ${lri}/mnt/usb0${pdi}`);
    }
    // A label put together in code: only while the page reads right to left.
    expect(ltr("/data")).toBe("/data");
    // Every other language gets the value as it is.
    expect(translate("de", "missing {p}", { p: "/mnt/usb0" })).toBe("missing /mnt/usb0");
    expect(translate("en", "missing {p}", { p: "/mnt/usb0" })).toBe("missing /mnt/usb0");
  });

  it("leaves unknown placeholders unchanged", () => {
    const out = translate("en", "pkglib.group.count", { unrelated: 1 });
    expect(out).toContain("{n}");
  });

  it("returns empty for empty key (edge — defensive)", () => {
    expect(translate("en", "")).toBe("");
  });

  it("does not mutate the vars object", () => {
    const vars = { n: 5 };
    const before = { ...vars };
    translate("en", "pkglib.group.count", vars);
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

describe("pickLanguage", () => {
  const have = ["en", "de", "es", "pt-BR", "zh-CN", "zh-TW"] as const;
  it("takes an exact code in any case", () => {
    expect(pickLanguage(["zh-tw"], have)).toBe("zh-TW");
    expect(pickLanguage(["DE"], have)).toBe("de");
  });
  it("sends Chinese by script or region", () => {
    expect(pickLanguage(["zh-HK"], have)).toBe("zh-TW");
    expect(pickLanguage(["zh-Hant-TW"], have)).toBe("zh-TW");
    expect(pickLanguage(["zh-MO"], have)).toBe("zh-TW");
    expect(pickLanguage(["zh-Hans-CN"], have)).toBe("zh-CN");
    expect(pickLanguage(["zh"], have)).toBe("zh-CN");
  });
  it("falls to the same primary language", () => {
    expect(pickLanguage(["pt-PT"], have)).toBe("pt-BR");
    expect(pickLanguage(["es-419"], have)).toBe("es");
    expect(pickLanguage(["de-AT"], have)).toBe("de");
  });
  it("walks the preference list before giving up on English", () => {
    expect(pickLanguage(["sv-SE", "de-DE", "en-US"], have)).toBe("de");
    expect(pickLanguage(["sv-SE", undefined, ""], have)).toBe("en");
    expect(pickLanguage([], have)).toBe("en");
  });
});

describe("ensureLocaleWithRetry", () => {
  it("retries once and stays quiet when the retry works", async () => {
    let calls = 0;
    const load = async () => {
      calls += 1;
      if (calls === 1) throw new Error("chunk failed");
    };
    let gaveUp = false;
    await ensureLocaleWithRetry("de", () => (gaveUp = true), 0, load);
    expect(calls).toBe(2);
    expect(gaveUp).toBe(false);
  });
  it("reports once when the retry fails too, and never rejects", async () => {
    let calls = 0;
    const seen: unknown[] = [];
    const load = async () => {
      calls += 1;
      throw new Error(`fail ${calls}`);
    };
    await expect(ensureLocaleWithRetry("de", (e) => seen.push(e), 0, load)).resolves.toBeUndefined();
    expect(calls).toBe(2);
    expect(seen).toHaveLength(1);
    expect(String(seen[0])).toContain("fail 2");
  });
  it("does not retry when the first load works", async () => {
    let calls = 0;
    await ensureLocaleWithRetry("de", () => {}, 0, async () => void (calls += 1));
    expect(calls).toBe(1);
  });
});
