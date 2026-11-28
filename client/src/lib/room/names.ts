// Play Rooms: names typed by people (room names, guest names) checked before they are sent, so the
// form says what is wrong at once. The host engine and the hub check again and are the authority
// (ps5upload-hub names.rs; the engine's join_names.rs); this mirrors their rules without the word
// list, which only they carry.

/** Why a name is not taken (null: fine). */
export type NameProblem = "length" | "chars" | "link" | "code" | "reserved" | "spam";

/** Invisible format characters (zero-width, bidirectional overrides, BOM, variation selectors). */
/** Invisible format characters (zero-width, bidirectional overrides, BOM, variation selectors),
 *  by code point (a character class of them trips the linter's look-alike rules). */
const INVISIBLE_RANGES: ReadonlyArray<[number, number]> = [
  [0xad, 0xad],
  [0x34f, 0x34f],
  [0x61c, 0x61c],
  [0x115f, 0x1160],
  [0x17b4, 0x17b5],
  [0x180b, 0x180e],
  [0x200b, 0x200f],
  [0x202a, 0x202e],
  [0x2060, 0x206f],
  [0x3164, 0x3164],
  [0xfe00, 0xfe0f],
  [0xfeff, 0xfeff],
  [0xffa0, 0xffa0],
  [0xfff0, 0xfffb],
];

export function invisibleChar(c: string): boolean {
  const cp = c.codePointAt(0) ?? 0;
  return INVISIBLE_RANGES.some(([a, b]) => cp >= a && cp <= b);
}

/** `text` without invisible format characters. */
export function dropInvisible(text: string): string {
  let out = "";
  for (const c of text) if (!invisibleChar(c)) out += c;
  return out;
}
const PUNCT = "-_.,!?'’&()#+:*~";
const ALLOWED = /^[\p{L}\p{N}\p{M} \-_.,!?'’&()#+:*~]*$/u;
const RESERVED = ["ps5upload", "official", "admin", "support", "phantomptr"];
const TLDS = new Set(
  "com net org gg io me ly tv xyz co app link site online info biz ru de uk fr es it br jp cn in us eu to cc ws club live shop store top icu fun pro dev page".split(
    " ",
  ),
);
const SERVICES = ["discord", "telegram", "whatsapp", "wechat", "snapchat", "onlyfans", "t.me", "wa.me", "bit.ly"];

/** A name as it will be shown: invisible characters dropped, spaces collapsed, trimmed. */
export function cleanName(raw: string): string {
  return dropInvisible(raw).replace(/\s+/gu, " ").trim();
}

/** Anything that reads as a link ("x.com", "www", "discord"). */
export function looksLikeLink(text: string): boolean {
  const lower = text.toLowerCase();
  if (lower.includes("://") || lower.includes("www") || lower.includes("http")) return true;
  if (SERVICES.some((s) => lower.includes(s))) return true;
  const squashed = lower.replace(/\s*\.\s*/g, ".");
  for (const m of squashed.matchAll(/([\p{L}\p{N}])\.([a-z]+)/gu)) {
    if (TLDS.has(m[2])) return true;
  }
  return false;
}

/** An invite code or a number in a name ("K7P2Q9", six digits in a row). */
export function looksLikeCode(text: string): boolean {
  for (const token of text.split(/[^A-Za-z0-9]+/)) {
    if (token.length < 5) continue;
    if (/\d{6}/.test(token)) return true;
    let switches = 0;
    for (let i = 1; i < token.length; i++) {
      if (/\d/.test(token[i]) !== /\d/.test(token[i - 1])) switches++;
    }
    if (switches >= 4) return true;
  }
  return false;
}

function spam(s: string): boolean {
  if (/(.)\1{4}/u.test(s)) return true;
  const chars = [...s];
  const punct = chars.filter((c) => PUNCT.includes(c)).length;
  return punct * 2 > chars.length;
}

/** What is wrong with a name of `min..max` characters, or null. */
export function nameProblem(raw: string, min: number, max: number): NameProblem | null {
  const name = cleanName(raw);
  const n = [...name].length;
  if (n < min || n > max) return "length";
  if (!ALLOWED.test(name)) return "chars";
  if (spam(name)) return "spam";
  if (looksLikeLink(name)) return "link";
  if (looksLikeCode(name)) return "code";
  const lower = name.toLowerCase();
  if (RESERVED.some((w) => lower.includes(w))) return "reserved";
  return null;
}

/** A room name: 3–32 characters (design 3.1). */
export const roomNameProblem = (raw: string) => nameProblem(raw, 3, 32);
/** A player's name inside a room: 1–24 characters (design 3.5). */
export const playerNameProblem = (raw: string) => nameProblem(raw, 1, 24);

/** The default room name: "<host>'s room" (the words are the app's). */
export function defaultRoomName(tr: (key: string, vars: Record<string, string>, fallback: string) => string, host: string): string {
  const who = cleanName(host).slice(0, 20) || "PS5";
  return tr("room_default_name", { name: who }, `${who}'s room`);
}

/** The comparison key two names are "the same" by (design 3.5): NFKC, case folded, invisible
 *  characters dropped, spaces collapsed, and the common Cyrillic/Greek look-alikes of Latin
 *  letters mapped. Only a hint for the form; the host engine decides. */
export function nameKey(raw: string): string {
  const LOOK: Record<string, string> = {
    а: "a", е: "e", о: "o", р: "p", с: "c", у: "y", х: "x", ѕ: "s", і: "i", ј: "j", ԁ: "d", һ: "h", ӏ: "l",
    α: "a", ο: "o", ν: "v", τ: "t", ι: "i", κ: "k", ρ: "p", χ: "x", μ: "u",
  };
  return cleanName(raw.normalize("NFKC"))
    .toLowerCase()
    .replace(/./gu, (c) => LOOK[c] ?? c);
}

type Tr = (key: string, vars: undefined, fallback: string) => string;

/** What to fix in a name, in words. */
export function nameProblemText(tr: Tr, p: NameProblem, room: boolean): string {
  switch (p) {
    case "length":
      return room
        ? tr("room_name_length", undefined, "Use 3 to 32 characters.")
        : tr("room_player_name_length", undefined, "Use 1 to 24 characters.");
    case "chars":
      return tr("room_name_chars", undefined, "Use letters, numbers, spaces and simple punctuation.");
    case "link":
      return tr("room_name_link", undefined, "Names can't have links.");
    case "code":
      return tr("room_name_code", undefined, "Names can't have codes or long numbers.");
    case "reserved":
      return tr("room_name_reserved", undefined, "Pick another name.");
    case "spam":
      return tr("room_name_spam", undefined, "Pick a name with fewer repeats and symbols.");
  }
}
