import { describe, expect, it } from "vitest";

import en from "../i18n/locales/en";
import { libraryRefusesToo, registerAfterUploadWarning } from "./registerAfterUpload";

const LIBRARY = "You can still do it from the Library";

describe("the warning after an upload that could not be added to the home screen", () => {
  it("points to the Library when the Library could still do it", () => {
    for (const raw of ["engine HTTP 502: timed out", "0x80990004", "connection reset"]) {
      expect(libraryRefusesToo(raw)).toBe(false);
      const text = registerAfterUploadWarning(raw);
      expect(text).toContain("Couldn't add it to the home screen automatically");
      expect(text).toContain(LIBRARY);
    }
  });

  it("a system app's title ID: the reason alone, no Library", () => {
    const raw = "payload rejected APP_REGISTER(/data/homebrew/x): register_system_title";
    expect(libraryRefusesToo(raw)).toBe(true);
    const text = registerAfterUploadWarning(raw);
    expect(text).toBe(en.err_register_system_title);
    expect(text).not.toContain(LIBRARY);
  });

  it("a title ID that is not one: the reason alone", () => {
    const text = registerAfterUploadWarning("register failed: register_title_id_unsafe");
    expect(text).toBe(en.err_register_title_id_unsafe);
    expect(text).not.toContain(LIBRARY);
  });

  it("a game the console installed itself: says where it is, no Library", () => {
    const raw = "payload rejected APP_REGISTER(/mnt/usb0/d/PPSA03595): register_title_installed:m2";
    expect(libraryRefusesToo(raw)).toBe(true);
    const text = registerAfterUploadWarning(raw, "Astro Bot");
    expect(text).toContain("Astro Bot is already installed on M.2 storage");
    expect(text).not.toContain(LIBRARY);
    // An unregister refusal is not this one.
    expect(libraryRefusesToo("unregister_title_installed:usb")).toBe(false);
  });
});
