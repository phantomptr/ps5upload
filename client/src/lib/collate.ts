/**
 * Shared collators for sorting names. `localeCompare` with options builds its rules for each
 * comparison: measured 23-34 times slower than one reused collator, seconds of frozen page on a big
 * folder (100,000 names: 5.7 s against 0.25 s).
 */

/** Names as people read them: case and accents ignored, `file10` after `file9`. */
export const compareNames = new Intl.Collator(undefined, {
  numeric: true,
  sensitivity: "base",
}).compare;

/** Case and accents ignored, digits as written. */
export const compareText = new Intl.Collator(undefined, { sensitivity: "base" })
  .compare;
