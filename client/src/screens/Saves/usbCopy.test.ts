// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import en from "../../i18n/locales/en";

// The USB backup copies the save to this computer, zips it and uploads it back,
// so the old "without leaving the PS5" was false. Keep the shown copy honest.
const screen: string = readFileSync(new URL("./index.tsx", import.meta.url).pathname, "utf8");

describe("Save data USB copy", () => {
  it("says the save goes through this computer", () => {
    for (const key of ["saves_backup_usb_tooltip_v2", "saves_restore_usb_tooltip_v2"]) {
      expect(en[key]).toMatch(/this computer/);
      expect(en[key]).not.toMatch(/without leaving/i);
      expect(screen).toContain(`"${key}"`);
    }
  });

  it("no longer shows the old claims", () => {
    expect(screen).not.toContain('"saves_backup_usb_tooltip"');
    expect(screen).not.toContain('"saves_restore_usb_tooltip"');
  });

  it("does not promise zip backups in the browser build", () => {
    expect(en.saves_description_browser).toMatch(/desktop app/);
    expect(screen).toContain('"saves_description_browser"');
  });
});
