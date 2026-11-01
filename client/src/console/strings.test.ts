import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  _setStringsLoaderForTests,
  chooseLanguage,
  cleanStrings,
  getLanguage,
  LANGUAGES,
  makeT,
  onLanguageChange,
  pickLanguage,
  setLanguage,
  stringsReady,
  stringsUrl,
  STRINGS_TIMEOUT_MS,
  stringsVersion,
  t,
} from "./strings";
import en from "./locales/en.json";
import de from "./locales/de.json";
// @ts-expect-error plain .mjs build script shared with this test, no type declarations
import { consoleStringFiles, extractConsoleStrings } from "../../../scripts/build-console-strings.mjs";

/** The committed locales/<code>.json files, as text. */
const committed = import.meta.glob("./locales/*.json", { query: "?raw", import: "default", eager: true }) as Record<
  string,
  string
>;

// Vitest runs in node here: <html> is a plain object with the two attributes strings.ts sets.
let html: { lang: string; dir: string };
beforeEach(() => {
  html = { lang: "en", dir: "ltr" };
  vi.stubGlobal("document", { documentElement: html });
});
afterEach(() => {
  vi.restoreAllMocks();
  _setStringsLoaderForTests(null);
  chooseLanguage("");
  vi.unstubAllGlobals();
});

/** A loader that answers from the committed files and records what was asked. */
function fileLoader(asked: string[], fail: string[] = []) {
  return (code: string) => {
    asked.push(code);
    if (fail.includes(code)) return Promise.reject(new Error("HTTP 404"));
    const text = committed[`./locales/${code}.json`];
    return text ? Promise.resolve(JSON.parse(text) as Record<string, string>) : Promise.reject(new Error("none"));
  };
}

describe("console strings", () => {
  it("t_falls_back_to_english", () => {
    const say = makeT({ en: { cw_x: "Hello {name}", cw_y: "Both" }, "zh-CN": { cw_y: "两者" } }, "zh-CN");
    expect(say("cw_x", { name: "Ana" })).toBe("Hello Ana");
    expect(say("cw_y")).toBe("两者");
    expect(say("cw_missing_everywhere")).toBe("cw_missing_everywhere");
  });

  it("picks the closest shipped language for the browser's", () => {
    const have = ["en", "zh-CN", "zh-TW", "pt-BR", "de"];
    expect(pickLanguage("de-DE", have)).toBe("de");
    expect(pickLanguage("zh-cn", have)).toBe("zh-CN");
    expect(pickLanguage("zh-HK", have)).toBe("zh-TW");
    expect(pickLanguage("zh-Hant-TW", have)).toBe("zh-TW");
    expect(pickLanguage("zh", have)).toBe("zh-CN");
    expect(pickLanguage("pt-PT", have)).toBe("pt-BR");
    expect(pickLanguage("xx", have)).toBe("en");
    expect(pickLanguage(undefined, have)).toBe("en");
  });

  it("the committed locale files match the locale sources (run npm run build:console)", () => {
    const want = consoleStringFiles() as Record<string, string>;
    // Git for Windows checks them out with CRLF (core.autocrlf); the script writes LF.
    const have = Object.fromEntries(
      Object.entries(committed).map(([p, text]) => [p.replace("./locales/", ""), text.replace(/\r\n/g, "\n")]),
    );
    expect(Object.keys(have).sort()).toEqual(Object.keys(want).sort());
    for (const [f, text] of Object.entries(want)) expect(have[f], f).toBe(text);
  });

  it("the build script emits one file per locale, and LANGUAGES lists exactly those", () => {
    const all = extractConsoleStrings() as Record<string, Record<string, string>>;
    expect(Object.keys(consoleStringFiles() as object).sort()).toEqual(Object.keys(all).map((c) => `${c}.json`).sort());
    expect(LANGUAGES).toEqual(Object.keys(all).sort());
    expect(LANGUAGES).toHaveLength(21);
  });

  it("every shipped locale translates every console key", () => {
    const keys = Object.keys(en).sort();
    expect(keys.length).toBeGreaterThan(0);
    for (const [p, text] of Object.entries(committed)) {
      expect(Object.keys(JSON.parse(text) as object).sort(), p).toEqual(keys);
    }
  });

  it("asks the helper for /strings/<code>.json with the build hash", () => {
    expect(stringsUrl("zh-CN")).toMatch(/^\/strings\/zh-CN\.json\?v=[\w-]+$/);
  });

  it("keeps only string cw_ values from a strings file", () => {
    expect(cleanStrings({ cw_a: "A", cw_b: 3, other: "x", cw_c: null })).toEqual({ cw_a: "A" });
    expect(cleanStrings(["cw_a"])).toEqual({});
    expect(cleanStrings(null)).toEqual({});
  });

  it("shows English until the chosen language loads, then switches and re-renders", async () => {
    const asked: string[] = [];
    let release: (v: Record<string, string>) => void = () => {};
    _setStringsLoaderForTests((code) => {
      asked.push(code);
      return new Promise((resolve) => {
        release = resolve;
      });
    });
    const seen = vi.fn();
    const off = onLanguageChange(seen);
    const before = stringsVersion();
    chooseLanguage("de");
    expect(getLanguage()).toBe("de");
    expect(asked).toEqual(["de"]);
    // Loading: English, never raw keys.
    expect(t("cw_nav_settings")).toBe(en.cw_nav_settings);
    expect(html.lang).toBe("en");
    seen.mockClear();
    release(de);
    await stringsReady();
    expect(t("cw_nav_settings")).toBe(de.cw_nav_settings);
    expect(html.lang).toBe("de");
    expect(seen).toHaveBeenCalled();
    expect(stringsVersion()).toBeGreaterThan(before);
    off();
  });

  it("a key the language lacks falls back to English", async () => {
    _setStringsLoaderForTests(() => Promise.resolve({ cw_nav_settings: "Einstellungen!" }));
    setLanguage("de");
    await stringsReady();
    expect(t("cw_nav_settings")).toBe("Einstellungen!");
    expect(t("cw_settings_language_system")).toBe(en.cw_settings_language_system);
  });

  it("loads each language once, applies right-to-left, and switches back at once", async () => {
    const asked: string[] = [];
    _setStringsLoaderForTests(fileLoader(asked));
    setLanguage("ar");
    await stringsReady();
    expect(html.dir).toBe("rtl");
    setLanguage("en");
    expect(asked).toEqual(["ar"]);
    expect(html.dir).toBe("ltr");
    expect(t("cw_nav_settings")).toBe(en.cw_nav_settings);
    setLanguage("ar");
    expect(html.lang).toBe("ar"); // cached: no wait
    expect(asked).toEqual(["ar"]);
  });

  it("a load that lands after another choice does not take over", async () => {
    let releaseDe: (v: Record<string, string>) => void = () => {};
    _setStringsLoaderForTests((code) =>
      code === "de"
        ? new Promise((resolve) => {
            releaseDe = resolve;
          })
        : Promise.resolve(JSON.parse(committed[`./locales/${code}.json`]) as Record<string, string>),
    );
    setLanguage("de");
    setLanguage("fr");
    await stringsReady();
    expect(html.lang).toBe("fr");
    releaseDe(de);
    await Promise.resolve();
    await Promise.resolve();
    expect(html.lang).toBe("fr");
    expect(getLanguage()).toBe("fr");
  });

  it("a failed load stays in English and logs once", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const asked: string[] = [];
    _setStringsLoaderForTests(fileLoader(asked, ["de", "fr"]));
    setLanguage("de");
    await stringsReady();
    setLanguage("fr");
    await stringsReady();
    expect(t("cw_nav_settings")).toBe(en.cw_nav_settings);
    expect(html.lang).toBe("en");
    expect(warn).toHaveBeenCalledTimes(1);
    // A later choice retries (the failure is not remembered).
    setLanguage("de");
    expect(asked).toEqual(["de", "fr", "de"]);
  });

  it("switching from a shown language to one still loading keeps the shown one (no English flash)", async () => {
    let releaseFr: (v: Record<string, string>) => void = () => {};
    _setStringsLoaderForTests((code) =>
      code === "fr"
        ? new Promise((resolve) => {
            releaseFr = resolve;
          })
        : Promise.resolve(de),
    );
    setLanguage("de");
    await stringsReady();
    expect(html.lang).toBe("de");
    setLanguage("fr");
    expect(getLanguage()).toBe("fr");
    expect(html.lang).toBe("de"); // still German while French loads
    expect(t("cw_nav_settings")).toBe(de.cw_nav_settings);
    releaseFr(JSON.parse(committed["./locales/fr.json"]) as Record<string, string>);
    await stringsReady();
    expect(html.lang).toBe("fr");
  });

  it("the real loader gives up on a strings file that never arrives", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const fetchMock = vi.fn(
      (_url: string, init?: RequestInit) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => reject(new Error("aborted")));
        }),
    );
    vi.stubGlobal("fetch", fetchMock);
    try {
      setLanguage("ko");
      const done = stringsReady();
      await vi.advanceTimersByTimeAsync(STRINGS_TIMEOUT_MS);
      await done;
      expect(warn).toHaveBeenCalledTimes(1);
      expect(html.lang).toBe("en");
    } finally {
      vi.useRealTimers();
    }
  });

  it("the real loader fetches the helper's URL and keeps English when it fails", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const fetchMock = vi.fn(() => Promise.resolve(new Response("nope", { status: 404 })));
    vi.stubGlobal("fetch", fetchMock);
    setLanguage("ja");
    await stringsReady();
    expect(fetchMock).toHaveBeenCalledWith(stringsUrl("ja"), expect.objectContaining({ credentials: "same-origin" }));
    expect(t("cw_nav_settings")).toBe(en.cw_nav_settings);
    expect(warn).toHaveBeenCalledTimes(1);
    fetchMock.mockImplementation(() =>
      Promise.resolve(new Response(committed["./locales/ja.json"], { status: 200, headers: { "Content-Type": "application/json" } })),
    );
    setLanguage("ja");
    await stringsReady();
    expect(html.lang).toBe("ja");
  });
});
