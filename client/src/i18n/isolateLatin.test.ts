import { describe, expect, it } from "vitest";

// @ts-expect-error plain .mjs script, no type declarations
import { isolateLatin as scriptIsolateUntyped, RTL_LOCALES } from "../../../scripts/rtl-isolate.mjs";
import { ensureLocale, t } from "../i18n";
import { bareLatinPieces, isolateLatin } from "./isolateLatin";
import ar from "./locales/ar";
import en from "./locales/en";
import fa from "./locales/fa";
import consoleAr from "../console/locales/ar.json";
import consoleFa from "../console/locales/fa.json";

const L = "⁦";
const P = "⁩";
const RTL = { ar, fa } as Record<string, Record<string, string>>;
const scriptIsolate = scriptIsolateUntyped as (s: string) => string;
const unmarked = (s: string) => s.split(L).join("").split(P).join("");

describe("Latin names inside a right-to-left sentence", () => {
  it("a name ending in a symbol is kept whole", () => {
    expect(isolateLatin("سُلّم إلى ShadowMount+")).toBe(`سُلّم إلى ${L}ShadowMount+${P}`);
    // In brackets, before a full stop, twice in one sentence.
    expect(isolateLatin("صورة قرص (ShadowMount+).")).toBe(`صورة قرص (${L}ShadowMount+${P}).`);
    expect(isolateLatin("ShadowMount+ ثم ShadowMount+")).toBe(`${L}ShadowMount+${P} ثم ${L}ShadowMount+${P}`);
    // Not only that name: any Latin name with a trailing + or #.
    expect(isolateLatin("مكتوب بلغة C# أو C++")).toBe(`مكتوب بلغة ${L}C#${P} أو ${L}C++${P}`);
  });

  it("letters with digits and symbols between them stay in order", () => {
    expect(isolateLatin("استخدم 7-Zip هنا")).toBe(`استخدم ${L}7-Zip${P} هنا`);
    expect(isolateLatin("الإصدار v1.15 أو kstuff-1.13-fpkg")).toBe(`الإصدار ${L}v1.15${P} أو ${L}kstuff-1.13-fpkg${P}`);
    expect(isolateLatin("اضغط Ctrl+Shift+E")).toBe(`اضغط ${L}Ctrl+Shift+E${P}`);
    expect(isolateLatin("افتح https://example.com/a?b=c الآن")).toBe(`افتح ${L}https://example.com/a?b=c${P} الآن`);
  });

  it("a path, a pattern and a file extension keep their leading symbol", () => {
    expect(isolateLatin("في /data/homebrew أو .ffpkg أو *.pkg")).toBe(
      `في ${L}/data/homebrew${P} أو ${L}.ffpkg${P} أو ${L}*.pkg${P}`,
    );
    // After a letter the symbol belongs to the sentence, not to the Latin word.
    expect(isolateLatin("الصورة/ISO")).toBe("الصورة/ISO");
    expect(isolateLatin("الصورة/game.iso")).toBe(`الصورة/${L}game.iso${P}`);
  });

  it("plain words, numbers and placeholders are left alone", () => {
    for (const s of [
      "يحتاج إلى kstuff على PS5",
      "من 1.00 إلى 13.60",
      "تم نسخ {count} من {total}",
      "النسبة 16:9 و50%",
      "انتهى.",
      "",
    ]) {
      expect(isolateLatin(s)).toBe(s);
    }
    // A sentence's own full stop, comma and colon stay outside the name.
    expect(isolateLatin("شغّل kstuff. ثم ShadowMount+، ثم elfldr: الآن")).toBe(
      `شغّل kstuff. ثم ${L}ShadowMount+${P}، ثم elfldr: الآن`,
    );
  });

  it("a placeholder inside a piece is kept as it is, so it is still filled in", () => {
    expect(isolateLatin("الإصدار v{version} في /data/{name}.pkg")).toBe(
      `الإصدار ${L}v{version}${P} في ${L}/data/{name}.pkg${P}`,
    );
    expect(isolateLatin("{count}×{size}")).toBe("{count}×{size}");
  });

  it("is safe to apply twice", () => {
    for (const s of ["سُلّم إلى ShadowMount+ في /data/x.pkg", "v{version} و C++ و Ctrl+K"]) {
      const once = isolateLatin(s);
      expect(isolateLatin(once)).toBe(once);
      expect(bareLatinPieces(once)).toEqual([]);
    }
    expect(bareLatinPieces("سُلّم إلى ShadowMount+")).toEqual(["ShadowMount+"]);
  });

  // The check that keeps this from coming back: every string of every right-to-left locale, as
  // the app shows it (through the loader, i18n.ts), holds no Latin name or path outside the marks.
  // A string added tomorrow is covered without anyone remembering to do anything.
  it("no right-to-left string the app shows has a bare Latin name with a symbol", async () => {
    expect(RTL_LOCALES).toEqual(Object.keys(RTL));
    for (const [code, dict] of Object.entries(RTL)) {
      await ensureLocale(code as "ar" | "fa");
      const bare: string[] = [];
      let names = 0;
      for (const key of Object.keys(dict)) {
        const shown = t(code, key);
        for (const piece of bareLatinPieces(shown)) bare.push(`${code} ${key}: ${piece}`);
        if (dict[key].includes("ShadowMount+")) {
          names += 1;
          // Every occurrence, not just the first.
          expect(shown.split("ShadowMount+").length, key).toBe(shown.split(`${L}ShadowMount+${P}`).length);
        }
        // Nothing but the two marks was added, and every placeholder is still there to fill.
        expect(unmarked(shown), key).toBe(dict[key]);
      }
      expect(bare).toEqual([]);
      // The strings this was found in (about 80 at the time).
      expect(names).toBeGreaterThan(50);
    }
  });

  it("placeholders in a right-to-left string are still filled, isolated once", async () => {
    await ensureLocale("ar");
    const key = Object.keys(ar).find((k) => /\{version\}/.test(ar[k as keyof typeof ar] as string))!;
    const shown = t("ar", key, { version: "12.6.32" });
    expect(shown).toContain("12.6.32");
    expect(shown).not.toContain("{version}");
  });

  it("the console page's right-to-left strings are written the same way", () => {
    for (const [code, page] of Object.entries({ ar: consoleAr, fa: consoleFa } as Record<string, Record<string, string>>)) {
      for (const [key, value] of Object.entries(page)) {
        expect(bareLatinPieces(value), `${code} ${key}`).toEqual([]);
        expect(value, `${code} ${key}`).toBe(isolateLatin(RTL[code][key]));
      }
    }
  });

  it("the build scripts' copy of the function gives the same result on every string", () => {
    for (const dict of [...Object.values(RTL), en as Record<string, string>]) {
      for (const value of Object.values(dict)) {
        expect(scriptIsolate(value)).toBe(isolateLatin(value));
      }
    }
  });

  it("left-to-right languages are not touched", async () => {
    await ensureLocale("de");
    const key = Object.keys(en).find((k) => (en as Record<string, string>)[k].includes("ShadowMount+"))!;
    expect(t("en", key)).not.toContain(L);
    expect(t("de", key)).not.toContain(L);
  });
});
