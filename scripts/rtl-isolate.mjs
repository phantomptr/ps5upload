/*
 * Latin text with a symbol, kept whole inside a right-to-left sentence ("ShadowMount+" is otherwise
 * drawn "+ShadowMount"). The same function as client/src/i18n/isolateLatin.ts, which explains it,
 * for build scripts that write strings the app's own loader never sees (the console page's
 * locales, build-console-strings.mjs). client/src/i18n/isolateLatin.test.ts holds the two to the
 * same result on every right-to-left string: change both together.
 */

/** The locales written right to left (i18n.ts RTL_CODES). */
export const RTL_LOCALES = ["ar", "fa"];

const LRI = "⁦";
const PDI = "⁩";
const UNIT = "(?:\\{\\w+\\}|[A-Za-z0-9]+)";
const PIECE = new RegExp(
  `${LRI}[^${PDI}]*${PDI}|[*./\\\\~]*${UNIT}(?:[._\\-/\\\\:+&@'=?%#~]{0,3}${UNIT})*[+#]*`,
  "g",
);
const OPENS = /[\s([{"'«“‘]/;

function split(m, before) {
  if (m.startsWith(LRI)) return null;
  let lead = "";
  let piece = m;
  if (before !== "" && !OPENS.test(before)) {
    lead = /^[*./\\~]*/.exec(m)[0];
    piece = m.slice(lead.length);
  }
  const own = piece.replace(/\{\w+\}/g, "");
  if (!/[A-Za-z]/.test(own) || /^[A-Za-z0-9]+$/.test(piece)) return null;
  return { lead, piece };
}

/** `s` with every piece of Latin text that holds a symbol isolated. Idempotent. */
export function isolateLatin(s) {
  if (!/[A-Za-z]/.test(s)) return s;
  return s.replace(PIECE, (m, at) => {
    const cut = split(m, at > 0 ? s[at - 1] : "");
    return cut ? cut.lead + LRI + cut.piece + PDI : m;
  });
}
