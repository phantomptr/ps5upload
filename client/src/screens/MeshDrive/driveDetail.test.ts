import { describe, expect, it } from "vitest";

import en from "../../i18n/locales/en";
import { driveDetailText } from "./driveDetail";

const tr = ((key: string, vars: Record<string, unknown> | undefined, fallback?: string) =>
  Object.entries(vars ?? {}).reduce(
    (t, [k, v]) => t.replace(`{${k}}`, String(v)),
    key.startsWith("drive_detail_") ? `[${key}] ${fallback}` : (fallback ?? ""),
  )) as never;

describe("driveDetailText", () => {
  it("words the engine's own sentences by their code, with what goes in them", () => {
    expect(driveDetailText(tr, "other_syncing", { name: "Pixel" }, "Pixel is syncing it")).toBe(
      "[drive_detail_other_syncing] Pixel is syncing it.",
    );
    expect(
      driveDetailText(tr, "changed_there", { path: "a.txt", console: "Pro" }, "x"),
    ).toContain("a.txt changed on Pro");
  });

  it("shows a raw error, and a code from a newer engine, as the engine said it", () => {
    expect(driveDetailText(tr, "", undefined, "upload a.txt: no space")).toBe("upload a.txt: no space");
    expect(driveDetailText(tr, undefined, undefined, "plain")).toBe("plain");
    expect(driveDetailText(tr, "a_code_from_later", {}, "English")).toBe("English");
  });

  it("every code's English is the English the app ships", () => {
    const table = en as unknown as Record<string, string>;
    for (const code of [
      "side_no_marker",
      "hub_no_marker",
      "install_running",
      "other_syncing",
      "lock_taken",
      "helper_old",
      "pass_stopped",
      "changed_here",
      "changed_there",
      "confirm_deletes",
      "too_many_files",
      "list_unreadable",
      "link_record_damaged",
      "too_many_for_console",
    ]) {
      const said = driveDetailText(
        ((key: string, _: unknown, fallback: string) => `${key}|${fallback}`) as never,
        code,
        {},
        "",
      );
      const [key, english] = said.split("|");
      expect(table[key], key).toBe(english);
    }
  });
});
