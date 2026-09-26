import { describe, expect, it } from "vitest";

import en from "./locales/en";
import ar from "./locales/ar";
import bn from "./locales/bn";
import de from "./locales/de";
import es from "./locales/es";
import fr from "./locales/fr";
import hi from "./locales/hi";
import hu from "./locales/hu";
import id from "./locales/id";
import itIT from "./locales/it";
import ja from "./locales/ja";
import ko from "./locales/ko";
import pl from "./locales/pl";
import ptBR from "./locales/pt-BR";
import ru from "./locales/ru";
import th from "./locales/th";
import trTR from "./locales/tr";
import vi from "./locales/vi";
import zhCN from "./locales/zh-CN";
import zhTW from "./locales/zh-TW";
import type { Translations } from "./types";

// Every shipped locale except English (English is the source catalog and the
// universal runtime fallback — see i18n.ts).
const NON_EN: ReadonlyArray<readonly [string, Translations]> = [
  ["ar", ar],
  ["bn", bn],
  ["de", de],
  ["es", es],
  ["fr", fr],
  ["hi", hi],
  ["hu", hu],
  ["id", id],
  ["it", itIT],
  ["ja", ja],
  ["ko", ko],
  ["pl", pl],
  ["pt-BR", ptBR],
  ["ru", ru],
  ["th", th],
  ["tr", trTR],
  ["vi", vi],
  ["zh-CN", zhCN],
  ["zh-TW", zhTW],
];

/**
 * Stale keys: present in (some/all) translation files but no longer referenced
 * anywhere in the app — the call site was renamed (e.g. `…_hint` → `…_hint_v2`)
 * and the old translations were left behind. They're harmless dead weight, NOT
 * English-catalog gaps, so they're explicitly exempt from the completeness
 * invariant below. Keep this list SMALL and shrinking; if you add to it, you
 * probably meant to delete the dead key from the locale files instead.
 */
const KNOWN_STALE = new Set<string>(["upload_scanning_archive_hint"]);

describe("i18n locale integrity", () => {
  it("English source catalog is a non-empty string map", () => {
    expect(Object.keys(en).length).toBeGreaterThan(1000);
    for (const [k, v] of Object.entries(en)) {
      expect(typeof v, `en[${k}] must be a string`).toBe("string");
      expect(v.length, `en[${k}] must not be empty`).toBeGreaterThan(0);
    }
  });

  it.each(NON_EN.map(([code]) => code))(
    "locale %s exports only non-empty string values",
    (code) => {
      const dict = NON_EN.find(([c]) => c === code)![1];
      expect(Object.keys(dict).length).toBeGreaterThan(0);
      for (const [k, v] of Object.entries(dict)) {
        expect(typeof v, `${code}[${k}] must be a string`).toBe("string");
        expect((v as string).length, `${code}[${k}] empty`).toBeGreaterThan(0);
      }
    },
  );
});

describe("i18n placeholder integrity", () => {
  const placeholders = (s: string) =>
    (s.match(/\{[A-Za-z_][A-Za-z0-9_]*\}/g) ?? []).sort();

  /**
   * `t()` interpolates `{name}` by plain string replacement — it has no
   * plural support (see i18n.ts). So an en value like `"{n} rule{n}"`,
   * written by mechanically converting the call site's
   * `` `${n} rule${n === 1 ? "" : "s"}` `` fallback, renders as the
   * literal "5 rule5" for every English user.
   *
   * Prefer count-neutral phrasing ("Rules: {n}") over a `_one`/`_many`
   * key split: we ship Polish, Russian and Arabic, whose plural
   * categories a binary split cannot express correctly anyway.
   */
  const fakePlural = (v: string) => {
    // Signal is the CONJUNCTION of two things: the same placeholder used
    // more than once, and one of those uses glued to the end of a word.
    // Either alone is legitimate — `v{version}` is a version prefix,
    // `0x{hex}` a radix prefix, and a placeholder may repeat across a
    // sentence for good reason.
    for (const name of new Set(placeholders(v))) {
      const bare = name.slice(1, -1);
      const uses = v.split(name).length - 1;
      if (uses > 1 && new RegExp(`[A-Za-z]\\{${bare}\\}`).test(v)) return true;
    }
    return false;
  };

  it("en.ts has no placeholder used as a fake plural suffix", () => {
    const bad = Object.entries(en)
      .filter(([, v]) => fakePlural(v as string))
      .map(([k, v]) => `${k}: ${v as string}`);
    expect(
      bad,
      "A placeholder directly following a letter is almost always a botched " +
        "plural — it renders literally (e.g. '5 rule5'). Use count-neutral " +
        `phrasing instead:\n${bad.join("\n")}`,
    ).toEqual([]);
  });

  /**
   * A translation that drops or renames a placeholder either loses the
   * value entirely or prints the braces verbatim on screen. Neither is
   * catchable by the key-presence coverage gate.
   */
  it.each(NON_EN.map(([code]) => code))(
    "locale %s uses the same placeholders as en for every shared key",
    (code) => {
      const dict = NON_EN.find(([c]) => c === code)![1] as Record<
        string,
        string
      >;
      const drift: string[] = [];
      for (const [k, ev] of Object.entries(en)) {
        const tv = dict[k];
        if (typeof tv !== "string") continue;
        const a = placeholders(ev as string).join(",");
        const b = placeholders(tv).join(",");
        if (a !== b) drift.push(`${k}: en=[${a}] ${code}=[${b}]`);
      }
      expect(drift, `Placeholder drift in ${code}:\n${drift.join("\n")}`).toEqual(
        [],
      );
    },
  );
});

describe("i18n English-catalog completeness", () => {
  // The invariant: if EVERY translator has a key, the English source must have
  // it too — otherwise English users get the raw key / inline fallback while
  // other languages show a real translation, and the key is invisible to the
  // dev missing-key check. Keys translated in only SOME locales are not
  // required here (they're opt-in inline-fallback keys), only the universal
  // ones. This guards against re-introducing the 42 gaps reconciled in 3.3.x.
  it("every key translated in ALL locales also exists in en.ts", () => {
    const enKeys = new Set<string>(Object.keys(en));
    // Keys present in every non-English locale.
    let universal: string[] | null = null;
    for (const [, dict] of NON_EN) {
      const keys = new Set<string>(Object.keys(dict));
      universal =
        universal === null
          ? Array.from(keys)
          : universal.filter((k) => keys.has(k));
    }
    const missing = (universal ?? [])
      .filter((k) => !enKeys.has(k))
      .filter((k) => !KNOWN_STALE.has(k))
      .sort();
    expect(
      missing,
      `These keys are translated in all locales but missing from en.ts — add ` +
        `them to client/src/i18n/locales/en.ts (English source):\n${missing.join("\n")}`,
    ).toEqual([]);
  });
});
