// Names Rename suggests for a game, from formats: `{title} [{titleid}] [v{version:short}]`.
//
// A {field} or [field] is replaced by a value: title, titleid (id), contentid, version (ver),
// format (kind), platform or name (the current name without its extension). [word] stays as it is
// when word is no field, so [USA] is text; {word} must be a field. {{ and }} are a brace. Modifiers
// after a field: upper, lower, nospace, under, dash, dot, short (01.004.000 to 1.4), and a number
// (at most that many characters). <...> is left out when a field in it has no value; a field outside
// <...> with no value means the format doesn't apply.
//
// The grammar follows fthdgn-gh/Alembic's name_formats (GPL-3.0-or-later).

export interface NameValues {
  title?: string | null;
  titleid?: string | null;
  contentid?: string | null;
  version?: string | null;
  /** PKG, EXFAT, FFPFSC, FFPKG, FFPFS; nothing for a folder. */
  format?: string | null;
  /** PS5 or PS4. */
  platform?: string | null;
  /** The current name, without its leading "." or extension. */
  name?: string | null;
}

type Field = keyof NameValues;

type Node =
  | { type: "text"; value: string }
  | { type: "field"; field: Field; mods: string[] }
  | { type: "group"; nodes: Node[] };

const ALIASES: Record<string, Field> = {
  title: "title",
  titleid: "titleid",
  id: "titleid",
  contentid: "contentid",
  version: "version",
  ver: "version",
  format: "format",
  kind: "format",
  platform: "platform",
  name: "name",
};
const MODS = new Set(["upper", "lower", "nospace", "under", "dash", "dot", "short"]);

/** Formats a name can start from. */
export const DEFAULT_NAME_FORMATS = [
  "{title} [{titleid}]",
  "{title} [{titleid}] [v{version}]",
  "{title} [{titleid}] [{format}]",
  "{titleid} - {title}",
  "{titleid}",
];

/** The most bytes ShadowMount+ mounts before each extension. */
export function nameLimit(name: string): number | null {
  const lower = name.toLowerCase();
  if (lower.endsWith(".ffpfsc")) return 58;
  if (lower.endsWith(".exfat") || lower.endsWith(".ffpkg") || lower.endsWith(".ffpfs")) return 63;
  return null;
}

const bytes = (s: string) => new TextEncoder().encode(s).length;

function parts(name: string, folder: boolean): { hidden: string; plain: string; ext: string } {
  const hidden = name.length > 1 && name.startsWith(".") ? "." : "";
  const rest = name.slice(hidden.length);
  const dot = rest.lastIndexOf(".");
  return !folder && dot > 0
    ? { hidden, plain: rest.slice(0, dot), ext: rest.slice(dot) }
    : { hidden, plain: rest, ext: "" };
}

/** A name ShadowMount+ would fail to mount: the bytes over its limit, or 0. */
export function nameOverLimit(name: string): number {
  const limit = nameLimit(name);
  if (limit === null) return 0;
  return Math.max(0, bytes(parts(name, false).plain) - limit);
}

function parseField(spec: string): Node | string {
  const [head, ...mods] = spec.split(":");
  const field = ALIASES[(head ?? "").toLowerCase().replace(/[_-]/g, "")];
  if (!field) return `unknown field ${head}`;
  const lower = mods.map((m) => m.toLowerCase());
  const bad = lower.find((m) => !MODS.has(m) && !/^[1-9][0-9]{0,2}$/.test(m));
  if (bad !== undefined) return `unknown modifier ${bad}`;
  return { type: "field", field, mods: lower };
}

function parseNodes(p: string, at: { i: number }, inGroup: boolean): Node[] | string {
  const out: Node[] = [];
  const text = (c: string) => {
    const last = out[out.length - 1];
    if (last?.type === "text") last.value += c;
    else out.push({ type: "text", value: c });
  };
  while (at.i < p.length) {
    const c = p.charAt(at.i);
    if (c === "{" && p[at.i + 1] === "{") {
      text("{");
      at.i += 2;
    } else if (c === "}" && p[at.i + 1] === "}") {
      text("}");
      at.i += 2;
    } else if (c === "{") {
      const close = p.indexOf("}", at.i + 1);
      if (close < 0) return "a brace in the format isn't closed";
      const node = parseField(p.slice(at.i + 1, close));
      if (typeof node === "string") return node;
      out.push(node);
      at.i = close + 1;
    } else if (c === "}") {
      return "a closing brace in the format has no opening one";
    } else if (c === "[") {
      const close = p.indexOf("]", at.i + 1);
      const spec = close < 0 ? "" : p.slice(at.i + 1, close);
      const node = /^[A-Za-z0-9_:-]+$/.test(spec) ? parseField(spec) : "no";
      if (typeof node === "string") {
        text("[");
        at.i += 1;
      } else {
        out.push(node);
        at.i = close + 1;
      }
    } else if (c === "<") {
      at.i += 1;
      const nodes = parseNodes(p, at, true);
      if (typeof nodes === "string") return nodes;
      if (p[at.i] !== ">") return "a < in the format isn't closed";
      at.i += 1;
      out.push({ type: "group", nodes });
    } else if (c === ">") {
      if (inGroup) return out;
      return "a > in the format has no <";
    } else {
      text(c);
      at.i += 1;
    }
  }
  return out;
}

function parsePattern(pattern: string): Node[] | string {
  return parseNodes(pattern, { i: 0 }, false);
}

/** What is wrong with a format, or null. */
export function nameFormatProblem(pattern: string): string | null {
  if (!pattern.trim()) return "the format is empty";
  const nodes = parsePattern(pattern);
  return typeof nodes === "string" ? nodes : null;
}

function shortVersion(v: string): string {
  const [major, minor] = v.split(".");
  const n = (s: string | undefined) => String(Number.parseInt(s ?? "0", 10) || 0);
  return minor === undefined ? n(major) : `${n(major)}.${n(minor)}`;
}

function applyMods(value: string, mods: string[]): string {
  let s = value;
  for (const m of mods) {
    if (m === "upper") s = s.toUpperCase();
    else if (m === "lower") s = s.toLowerCase();
    else if (m === "nospace") s = s.replace(/ /g, "");
    else if (m === "under") s = s.replace(/ /g, "_");
    else if (m === "dash") s = s.replace(/ /g, "-");
    else if (m === "dot") s = s.replace(/ /g, ".");
    else if (m === "short") s = shortVersion(s);
    else s = [...s].slice(0, Number(m)).join("").trimEnd();
  }
  return s;
}

function render(nodes: Node[], v: NameValues): { text: string; missing: boolean } {
  let text = "";
  let missing = false;
  for (const n of nodes) {
    if (n.type === "text") text += n.value;
    else if (n.type === "field") {
      const value = v[n.field]?.trim();
      if (value) text += applyMods(value, n.mods);
      else missing = true;
    } else {
      const inner = render(n.nodes, v);
      if (!inner.missing) text += inner.text;
    }
  }
  return { text, missing };
}

/** What file systems refuse goes; ':' becomes " - ". */
function safeName(s: string): string {
  return s
    .replace(/:/g, " - ")
    // eslint-disable-next-line no-control-regex
    .replace(/[/\\*?"<>|\u0000-\u001f]/g, "")
    .replace(/ {2,}/g, " ")
    .trim()
    // A leading dot would hide the game from ShadowMount+'s scan.
    .replace(/^[. ]+/, "");
}

/** The name without extension, or null when a needed value is missing or the format is bad. */
export function renderName(pattern: string, v: NameValues): string | null {
  const nodes = parsePattern(pattern);
  if (typeof nodes === "string") return null;
  const out = render(nodes, v);
  const s = safeName(out.text);
  return out.missing || !s || bytes(s) > 240 ? null : s;
}

/** `entry` renamed with a format: a hidden name stays hidden and a file keeps its extension. */
export function suggestedName(
  pattern: string,
  v: NameValues,
  entry: string,
  folder: boolean,
): string | null {
  const stem = renderName(pattern, v);
  if (!stem) return null;
  const p = parts(entry, folder);
  return p.hidden + stem + p.ext;
}

const trimCut = (s: string) => s.replace(/[ .,_-]+$/, "");

/** The same name cut to what ShadowMount+ mounts: the title, at a word if it can; else the end. */
export function fittedName(
  pattern: string,
  v: NameValues,
  entry: string,
  folder: boolean,
): string | null {
  const full = suggestedName(pattern, v, entry, folder);
  const p = parts(entry, folder);
  const limit = nameLimit(entry);
  const stem = renderName(pattern, v);
  if (!full || !stem || limit === null || bytes(stem) <= limit) return null;
  const chars = [...safeName(v.title ?? "")];
  for (const words of [true, false]) {
    for (let n = chars.length - 1; n > 0; n--) {
      if (words && chars[n] !== " ") continue;
      const s = renderName(pattern, { ...v, title: trimCut(chars.slice(0, n).join("")) });
      if (s && bytes(s) <= limit) return p.hidden + s + p.ext;
    }
  }
  let kept = "";
  for (const ch of stem) {
    if (bytes(kept + ch) > limit) break;
    kept += ch;
  }
  const s = trimCut(kept);
  return s ? p.hidden + s + p.ext : null;
}

/** "name" and ".name", the name ShadowMount+'s scan skips. */
export const isHiddenName = (name: string) => name.length > 1 && name.startsWith(".");
export const hiddenName = (name: string) => (isHiddenName(name) ? name : `.${name}`);
export const shownName = (name: string) => (isHiddenName(name) ? name.slice(1) : name);

/** The word a format's {format} gives for a file or folder name. */
export function formatOfName(name: string, folder: boolean): string | null {
  if (folder) return null;
  const m = /\.(pkg|exfat|ffpfsc|ffpkg|ffpfs)$/i.exec(name);
  return m?.[1] ? m[1].toUpperCase() : null;
}
