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
].join("|");
const SECRET_JSON = new RegExp(`("(?:${SECRET_KEYS})"\\s*:\\s*")([^"]*)(")`, "gi");
const SECRET_KV = new RegExp(`\\b((?:${SECRET_KEYS})=)([^\\s&"]+)`, "gi");
const SERIAL_JSON = /("serial"\s*:\s*")([^"]*)(")/gi;
const HOME_POSIX = /\/(?:Users|home)\/[^/\s"\\]+/g;
const HOME_WIN = /[A-Za-z]:\\Users\\[^\\\s"]+/g;
const MAC = /\b[0-9a-fA-F]{2}(?::[0-9a-fA-F]{2}){5}\b/g;
const IPV6_BRACKETED = /\[([0-9a-fA-F]*:[0-9a-fA-F:]*)\]/g;
const IPV4 = /(^|[^0-9.])(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})(?=$|[^0-9.])/g;

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
      let out = s.replace(SECRET_JSON, "$1<removed>$3").replace(SECRET_KV, "$1<removed>");
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
