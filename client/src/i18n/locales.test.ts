import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

/**
 * Locale-file integrity guards. The per-locale dictionaries are plain TS
 * object literals; a DUPLICATE key is valid JS (the later one silently wins),
 * so a copy-paste slip could quietly override a translation with no error
 * from tsc or the i18n-coverage gate. This catches that class of bug.
 */
const LOCALES_DIR = join(dirname(fileURLToPath(import.meta.url)), "locales");

function localeFiles(): string[] {
  return readdirSync(LOCALES_DIR).filter(
    (f) => f.endsWith(".ts") && !f.endsWith(".d.ts") && !f.endsWith(".test.ts"),
  );
}

/** Extract top-level-ish key names from a locale file. Matches `key:` and
 *  `"key":` at the start of a line — good enough to spot duplicates in the
 *  flat dictionaries these files contain. */
function keysOf(src: string): string[] {
  const keys: string[] = [];
  const re = /^\s*"?([A-Za-z][A-Za-z0-9_.-]*)"?\s*:/gm;
  let m: RegExpExecArray | null;
  while ((m = re.exec(src))) keys.push(m[1]);
  return keys;
}

describe("locale files", () => {
  it("has at least the 18 expected locales", () => {
    expect(localeFiles().length).toBeGreaterThanOrEqual(18);
  });

  it("contains no duplicate keys (a dup silently overrides a translation)", () => {
    const offenders: string[] = [];
    for (const f of localeFiles()) {
      const keys = keysOf(readFileSync(join(LOCALES_DIR, f), "utf8"));
      const seen = new Set<string>();
      const dups = new Set<string>();
      for (const k of keys) {
        if (seen.has(k)) dups.add(k);
        seen.add(k);
      }
      if (dups.size) offenders.push(`${f}: ${[...dups].join(", ")}`);
    }
    expect(offenders).toEqual([]);
  });
});
