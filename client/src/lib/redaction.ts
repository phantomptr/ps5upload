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
const SECRET_JSON = new RegExp(`("(?:${SECRET_KEYS})"\\s*:\\s*")([^"]*)(")`, "gi");
// The same, inside JSON stored as a string (localStorage values): `\"key\":\"value\"`.
const SECRET_JSON_ESCAPED = new RegExp(`(\\\\"(?:${SECRET_KEYS})\\\\"\\s*:\\s*\\\\")((?:(?!\\\\").)*)(\\\\")`, "gi");
const SECRET_KV = new RegExp(`\\b((?:${SECRET_KEYS})=)([^\\s&"]+)`, "gi");
// A pairing PIN after the word PIN, in any spelling a log has used: "Remote Play PIN: 14928144",
// "pin":"14928144", "pin": 14928144. Helpers before 11.5 wrote it to stderr.log.
const PIN = /(^|[^A-Za-z0-9_])(pin)([:="\\ \t]{1,8})(\d{4,})/gi;
const SERIAL_JSON = /("serial"\s*:\s*")([^"]*)(")/gi;
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
      let out = s
        .replace(PIN, "$1$2$3<removed>")
        .replace(SECRET_JSON, "$1<removed>$3")
        .replace(SECRET_JSON_ESCAPED, "$1<removed>$3")
        .replace(SECRET_KV, "$1<removed>");
      if (!opts.redact) return out;
      out = out
        .replace(SERIAL_JSON, "$1<serial>$3")
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
