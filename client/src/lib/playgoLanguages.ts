// The languages a package's PlayGo scenario can declare (the engine's SCENARIO_LANGUAGES, in
// Sony's system-language order), each named in itself so no translation is needed.

/** The language option's value for a package that declares every language (the engine's
 *  ALL_LANGUAGES); no value keeps the game's own. */
export const ALL_PLAYGO_LANGUAGES = "all";

export const PLAYGO_LANGUAGES: ReadonlyArray<readonly [code: string, name: string]> = [
  ["ja-JP", "日本語"],
  ["en-US", "English (US)"],
  ["fr-FR", "Français (France)"],
  ["es-ES", "Español (España)"],
  ["de-DE", "Deutsch"],
  ["it-IT", "Italiano"],
  ["nl-NL", "Nederlands"],
  ["pt-PT", "Português (Portugal)"],
  ["ru-RU", "Русский"],
  ["ko-KR", "한국어"],
  ["zh-Hant", "繁體中文"],
  ["zh-Hans", "简体中文"],
  ["fi-FI", "Suomi"],
  ["sv-SE", "Svenska"],
  ["da-DK", "Dansk"],
  ["no-NO", "Norsk"],
  ["pl-PL", "Polski"],
  ["pt-BR", "Português (Brasil)"],
  ["en-GB", "English (UK)"],
  ["tr-TR", "Türkçe"],
  ["es-419", "Español (Latinoamérica)"],
  ["ar-AE", "العربية"],
  ["fr-CA", "Français (Canada)"],
  ["cs-CZ", "Čeština"],
  ["hu-HU", "Magyar"],
  ["el-GR", "Ελληνικά"],
  ["ro-RO", "Română"],
  ["th-TH", "ไทย"],
  ["vi-VN", "Tiếng Việt"],
  ["id-ID", "Bahasa Indonesia"],
  ["uk-UA", "Українська"],
];
