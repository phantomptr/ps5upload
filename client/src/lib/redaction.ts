/**
 * Redaction for bug reports (spec §3.4). One `Redactor` per report, so an address gets the same
 * placeholder (`<ip-1>`, `<ip-2>`) in every file and ".100 vs .99" stays readable. Secrets are
 * removed whatever the switch says; the rest only when `redact` is on. The desktop builder
 * (client/src-tauri/src/commands/bug_report.rs) implements the same rules, and both are tested
 * against redaction.vectors.json.
 */
export interface Redactor {
  text(s: string): string;
}

const SECRET_KEYS = [
  "pairing_key",
  "psk",
  "token",
  "access_token",
  "refresh_token",
  "rp_key",
  "regist_key",
  "account_id",
  "psn_account_id",
  "secret",
  "password",
  "wake_credential",
  "wake_regist_key",
  "wake_rp_key",
  "pin",
  "morning",
  "passcode",
  "credential",
  "launch_token",
  "stream_key",
  "pad_token",
].join("|");
// `"key": "` up to where the value starts. The value's end is found by reading it (below), not by
// the pattern: "everything up to the next quote" stopped at a quote the value escapes (`\"`) and
// left the rest of a secret in the report. The desktop builder reads values the same way
// (client/src-tauri/src/commands/redact_json.rs).
const WS = "[ \\t\\n\\f\\r]*";
const SECRET_JSON_OPEN = new RegExp(`"(?:${SECRET_KEYS})"${WS}:${WS}"`, "gi");
// The same, inside JSON stored as a string (localStorage values): `\"key\":\"value\"`.
const SECRET_JSON_ESCAPED_OPEN = new RegExp(`\\\\"(?:${SECRET_KEYS})\\\\"${WS}:${WS}\\\\"`, "gi");
const SERIAL_JSON_OPEN = new RegExp(`"serial"${WS}:${WS}"`, "gi");

/** Where a JSON string value that starts at `from` ends: its first quote that no backslash
 *  escapes (`\"` stays inside, `\\"` ends it). A value cut short ends at the line's end. */
function plainValueEnd(s: string, from: number): number {
  let i = from;
  while (i < s.length) {
    const c = s[i];
    if (c === '"' || c === "\n") return i;
    i += c === "\\" ? 2 : 1;
  }
  return s.length;
}

/** The same for a value inside JSON that is itself stored in a JSON string, where the inner
 *  text's `"` reads `\"` and its `\` reads `\\`: the value ends at the first `\"` the inner text
 *  does not escape (`\\\"` stays inside, `\\\\\"` ends it). A bare `"` ends the outer string, and
 *  so the value. */
function escapedValueEnd(s: string, from: number): number {
  let i = from;
  while (i < s.length) {
    const c = s[i];
    if (c === '"' || c === "\n") return i;
    if (c !== "\\") {
      i += 1;
      continue;
    }
    const next = s[i + 1];
    // The inner text's closing quote.
    if (next === '"') return i;
    if (next !== "\\") {
      // An escape of the outer string (`\n`, `é`).
      i += 2;
      continue;
    }
    // One backslash of the inner text: it escapes what follows there.
    const a = s[i + 2];
    const b = s[i + 3];
    if (a === undefined) return s.length;
    if (a === "\\" && (b === "\\" || b === '"')) i += 4;
    else if (a === '"' || a === "\n") return i + 2;
    else i += 3;
  }
  return s.length;
}

/** Every value that follows a match of `open` becomes `to`. */
function replaceJsonValues(
  s: string,
  open: RegExp,
  to: string,
  valueEnd: (s: string, from: number) => number,
): string {
  open.lastIndex = 0;
  let out = "";
  let cursor = 0;
  for (let m = open.exec(s); m; m = open.exec(s)) {
    const start = m.index + m[0].length;
    const end = Math.min(valueEnd(s, start), s.length);
    out += s.slice(cursor, start) + to;
    cursor = end;
    open.lastIndex = Math.max(end, m.index + 1);
  }
  return out + s.slice(cursor);
}

const SECRET_KV = new RegExp(`\\b((?:${SECRET_KEYS})=)([^\\s&"]+)`, "gi");
// A pairing PIN after the word PIN, in any spelling a log has used: "Remote Play PIN: 14928144",
// "pin":"14928144", "pin": 14928144. Helpers before 11.5 wrote it to stderr.log.
const PIN = /(^|[^A-Za-z0-9_])(pin)([:="\\ \t]{1,8})(\d{4,})/gi;
const HOME_POSIX = /\/(?:Users|home)\/[^/\s"\\]+/g;
// The separator is one backslash, or the same run of them on both sides of `Users` when the path
// is JSON-escaped (`C:\\Users\\bob` in report.json, `C:\\\\Users\\\\bob` in a stored JSON string).
const HOME_WIN = /[A-Za-z]:(\\+)Users\1[^\\\s"]+/g;
const MAC = /\b[0-9a-fA-F]{2}(?::[0-9a-fA-F]{2}){5}\b/g;
const IPV6_BRACKETED = /\[([0-9a-fA-F]*:[0-9a-fA-F:]*)\]/g;
// An address is four 1-3 digit parts not inside a longer dotted number ("1.2.3.4.5", "6.5.2.1.0"):
// it may follow a word and a dot ("Draft.192.168.0.5") or end before one ("…100.json").
const IPV4 = /(^|[^0-9.]|[A-Za-z_-]\.)(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})(?=$|[^0-9.]|\.(?![0-9]))/g;

export function createRedactor(opts: { redact: boolean }): Redactor {
  const ips = new Map<string, number>();
  const ip = (addr: string) => {
    let n = ips.get(addr);
    if (n === undefined) {
      n = ips.size + 1;
      ips.set(addr, n);
    }
    return `<ip-${n}>`;
  };
  return {
    text(s: string): string {
      let out = s.replace(PIN, "$1$2$3<removed>");
      out = replaceJsonValues(out, SECRET_JSON_OPEN, "<removed>", plainValueEnd);
      out = replaceJsonValues(out, SECRET_JSON_ESCAPED_OPEN, "<removed>", escapedValueEnd);
      out = out.replace(SECRET_KV, "$1<removed>");
      if (!opts.redact) return out;
      out = replaceJsonValues(out, SERIAL_JSON_OPEN, "<serial>", plainValueEnd)
        .replace(HOME_WIN, "~")
        .replace(HOME_POSIX, "~")
        .replace(MAC, "<mac>")
        .replace(IPV6_BRACKETED, (_m, a: string) => `[${ip(a)}]`)
        .replace(IPV4, (_m, pre: string, a: string) => `${pre}${ip(a)}`);
      return out;
    },
  };
}

const secretsOnly = createRedactor({ redact: false });

/** Secrets out of a log line where it is written (state/logs.ts): keys, tokens, passwords, pairing
 *  PINs. Addresses stay; a report numbers them when it is made. */
export function scrubSecrets(s: string): string {
  return secretsOnly.text(s);
}
