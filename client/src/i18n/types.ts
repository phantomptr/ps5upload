/** Shape of a single locale's translation dictionary.
 *
 *  Every translation file under `./locales/<code>.ts` exports a
 *  `Translations`-typed object. The structure is intentionally loose
 *  (string-keyed) because the keyset is huge (1k+ entries) and locale
 *  files often omit entries that fall back to English. Strict-typing
 *  the keyset would force every locale to copy every key — defeating
 *  the value of "fall back to en for missing keys" that the runtime
 *  already implements. */
export type Translations = Record<string, string>;
