/**
 * Names and other Latin text inside a right-to-left sentence.
 *
 * In Arabic or Persian text a symbol next to a Latin word has no direction of its own and takes
 * the sentence's: "ShadowMount+" is drawn "+ShadowMount", ".ffpkg" "ffpkg.", "/data" "data/" and
 * "7-Zip" "Zip-7". `tr()` already keeps interpolated values whole (`isolatedForRtl`); this does the
 * same for what is written in the translated sentence itself. Every right-to-left locale goes
 * through `isolateLatinAll` when it is loaded (i18n.ts), so a string added later needs nothing.
 *
 * A piece of Latin text that holds a symbol (a name ending in + or #, a path, a file extension,
 * a version like v1.15, a key combination, a link) goes between LEFT-TO-RIGHT ISOLATE and POP
 * DIRECTIONAL ISOLATE, which draw nothing. A plain word ("kstuff", "PS5") and a plain number need
 * none and are left alone, as is anything already between the two marks.
 *
 * scripts/rtl-isolate.mjs is the same function for the build scripts (the console page's
 * strings); isolateLatin.test.ts holds the two to the same result.
 */

const LRI = "⁦";
const PDI = "⁩";

// One piece: letters, digits and {placeholders}, joined by up to three of . _ - / \ : + & @ ' = ? % # ~
// (which must sit between two of them: "://" in a link, "+" in Ctrl+K), with a path's, pattern's or
// extension's leading * . / \ ~ and a name's trailing + or #. Also matched, to be skipped: a run
// already isolated. No lookbehind (old WebKit): `split` looks at the character before.
const UNIT = "(?:\\{\\w+\\}|[A-Za-z0-9]+)";
const PIECE = new RegExp(
  `${LRI}[^${PDI}]*${PDI}|[*./\\\\~]*${UNIT}(?:[._\\-/\\\\:+&@'=?%#~]{0,3}${UNIT})*[+#]*`,
  "g",
);
/** What may stand before a piece's leading symbols: nothing, a space, an opening bracket or quote.
 *  After a letter ("عربي/English") the symbol belongs to the sentence and stays outside. */
const OPENS = /[\s([{"'«“‘]/;

/** The part of a match to isolate and the leading symbols left outside it; null: nothing to do. */
function split(m: string, before: string): { lead: string; piece: string } | null {
  if (m.startsWith(LRI)) return null;
  let lead = "";
  let piece = m;
  if (before !== "" && !OPENS.test(before)) {
    lead = /^[*./\\~]*/.exec(m)![0];
    piece = m.slice(lead.length);
  }
  const own = piece.replace(/\{\w+\}/g, "");
  // No Latin letter of its own (a number, a lone placeholder), or a plain word: as it is.
  if (!/[A-Za-z]/.test(own) || /^[A-Za-z0-9]+$/.test(piece)) return null;
  return { lead, piece };
}

/** `s` with every such piece of Latin text isolated. Applying it twice changes nothing. */
export function isolateLatin(s: string): string {
  if (!/[A-Za-z]/.test(s)) return s;
  return s.replace(PIECE, (m: string, at: number) => {
    const cut = split(m, at > 0 ? s[at - 1] : "");
    return cut ? cut.lead + LRI + cut.piece + PDI : m;
  });
}

/** A whole locale. */
export function isolateLatinAll<T extends Record<string, string>>(dict: T): T {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(dict)) out[key] = isolateLatin(value);
  return out as T;
}

/** Latin text with a symbol that is not isolated in `s`: what would be drawn out of order.
 *  Empty for anything that went through `isolateLatin`. For tests. */
export function bareLatinPieces(s: string): string[] {
  const out: string[] = [];
  for (const m of s.matchAll(PIECE)) {
    const cut = split(m[0], m.index > 0 ? s[m.index - 1] : "");
    if (cut) out.push(cut.piece);
  }
  return out;
}
