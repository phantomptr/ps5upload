// no_user_visible_string_mentions_ftx2_or_old_ports
//
// The helper speaks one protocol (AVA1) on one port (9120). A string that still names the old
// protocol or its two ports tells people to look for something that no longer exists, in every
// language. The locale files are read the way the coverage gate reads them (the literal is
// evaluated in an isolated vm context), not regex-parsed.
import { describe, expect, it } from "vitest";
// The client has no @types/node; vitest runs under node, where this resolves.
// @ts-expect-error TS2591: no node types
import vm from "node:vm";

const RAW = import.meta.glob("./locales/*.ts", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

function evalLocale(src: string): Record<string, string> {
  const start = src.indexOf("= {");
  const end = src.lastIndexOf("};");
  if (start < 0 || end < 0) throw new Error("no locale literal");
  return vm.runInNewContext(`(${src.slice(start + 2, end + 1)})`) as Record<
    string,
    string
  >;
}

// The old protocol name (with or without a space), its two ports, and "transfer port" in the
// languages the catalog is written in. Port 9115 (the install daemon) and 9021 (the loader) stay.
const OLD = /ftx\s*2|\b911[34]\b|transfer[- ]port/i;

describe("no_user_visible_string_mentions_ftx2_or_old_ports", () => {
  const files = Object.entries(RAW);
  it("reads all 21 locales", () => {
    expect(files.length).toBe(21);
  });
  it.each(files)("%s", (name, src) => {
    const dict = evalLocale(src);
    expect(Object.keys(dict).length).toBeGreaterThan(1000);
    const bad = Object.entries(dict)
      .filter(([, v]) => OLD.test(v))
      .map(([k, v]) => `${name} ${k}: ${v}`);
    expect(bad).toEqual([]);
  });
  it("says the new port where the old one was", () => {
    const en = evalLocale(RAW["./locales/en.ts"]);
    expect(en.status_payload_tooltip).toContain(":9120");
  });
});
