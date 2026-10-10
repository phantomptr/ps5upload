// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import en from "../../i18n/locales/en";

const read = (rel: string): string => readFileSync(new URL(rel, import.meta.url).pathname, "utf8");

// Three different things were all called "autoloader": the playlist run when a
// console connects, the USB stick the wizard writes, and the browser-menu
// payload in the catalogue. Each now has its own name.
describe("the three 'autoloader' features", () => {
  const names = [
    en.autoloader_title_v2,
    en.usb_wizard_title_v2,
    read("../../../src-tauri/src/commands/payloads.rs").match(
      /id: "webkit-autoloader",\s*\/\/[^\n]*\n\s*\/\/[^\n]*\n\s*display_name: "([^"]+)"/,
    )?.[1],
  ];

  it("have distinct names", () => {
    expect(names.every(Boolean)).toBe(true);
    expect(new Set(names).size).toBe(3);
  });

  it("are shown under those names", () => {
    expect(read("./PlaylistsPanel.tsx")).toContain('"autoloader_title_v2"');
    expect(read("./UsbAutoloaderModal.tsx")).toContain('"usb_wizard_title_v2"');
    expect(read("./CatalogPanel.tsx")).toContain('"payloads_open_usb_wizard_v2"');
  });
});
