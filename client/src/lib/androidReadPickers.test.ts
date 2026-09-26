import { describe, expect, it } from "vitest";

/**
 * Structural guard for issue #278.
 *
 * `plugin-dialog`'s `open()` returns a real filesystem path on desktop and a
 * `content://` Storage Access Framework URI on Android. Everything downstream
 * of a picked READ path — the engine especially — walks real `std::fs` paths,
 * so a `content://` URI fails the moment it is read. Issue #278 was exactly
 * that: the avatar picker called `open()` unguarded, and the engine answered
 * `HTTP 400 read image content://com.android.providers.downloads.documents`.
 *
 * `lib/pickPath.ts` exists to make this impossible — it routes Android to the
 * in-app real-path browser — but nothing stopped a screen from reaching past
 * it to `open()` directly, which is how the bug got in. This test is that
 * stop: any module CALLING the read dialog must also make an Android
 * decision, whether via `pickPath`, `pickLocalPath`, or its own `isAndroid()`
 * branch.
 *
 * Deliberately not about `save()`. Save dialogs also hand back `content://`
 * URIs, but those are resolved on the Rust side by `save_dest.rs`, so they
 * are correct as written and out of scope here.
 *
 * Sources are read through `import.meta.glob(..., '?raw')` rather than
 * `node:fs` because the client tsconfig has no node types.
 */

const SOURCES = import.meta.glob("../**/*.{ts,tsx}", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

/** `pickPath.ts` IS the abstraction, so it necessarily calls the dialog. */
const ALLOWED = new Set(["../lib/pickPath.ts"]);

/**
 * Whether a module actually invokes the read dialog, under whatever name it
 * imports it as. `open as openDialog` is the convention here, but resolving
 * the alias from the import statement means the guard does not depend on
 * everyone keeping to it.
 */
export function callsReadDialog(text: string): boolean {
  const imp =
    /import\s*\{([^}]*)\}\s*from\s*["']@tauri-apps\/plugin-dialog["']/.exec(text);
  if (!imp) return false;
  const alias = /(^|,)\s*open\s*(?:as\s+(\w+))?\s*(?=,|$)/.exec(imp[1]);
  if (!alias) return false;
  const name = alias[2] ?? "open";
  return new RegExp(`\\b${name}\\s*\\(`).test(text);
}

describe("Android read-path pickers (issue #278)", () => {
  it("no module opens a read dialog without deciding what Android does", () => {
    const offenders = Object.entries(SOURCES)
      .filter(([path]) => !/\.test\.tsx?$/.test(path))
      .filter(([path]) => !ALLOWED.has(path))
      .filter(([, text]) => callsReadDialog(text))
      // An Android decision, however it is spelled.
      .filter(([, text]) => !/\bisAndroid\b|\bpickPath\b|\bpickLocalPath\b/.test(text))
      .map(([path]) => path)
      .sort();

    // On Android an unguarded open() yields a content:// URI that the engine
    // cannot read. Route through `pickPath` (lib/pickPath.ts) instead.
    expect(offenders).toEqual([]);
  });

  it("actually scanned the source tree", () => {
    // Without this, a glob that silently matched nothing would make the
    // assertion above pass forever regardless of the code.
    expect(Object.keys(SOURCES).length).toBeGreaterThan(50);
    expect(Object.keys(SOURCES)).toContain("../screens/Profile/index.tsx");
  });

  it("still detects the pattern it is meant to catch", () => {
    const bad = `import { open as openDialog } from "@tauri-apps/plugin-dialog";
      const sel = await openDialog({ multiple: false });`;
    expect(callsReadDialog(bad)).toBe(true);

    const unaliased = `import { open } from "@tauri-apps/plugin-dialog";
      const sel = await open({ multiple: false });`;
    expect(callsReadDialog(unaliased)).toBe(true);

    const saveOnly = `import { save as saveDialog } from "@tauri-apps/plugin-dialog";
      const dest = await saveDialog({});`;
    expect(callsReadDialog(saveOnly)).toBe(false);

    const importedNotCalled = `import { open as openDialog, confirm } from "@tauri-apps/plugin-dialog";
      await confirm("sure?");`;
    expect(callsReadDialog(importedNotCalled)).toBe(false);
  });
});
