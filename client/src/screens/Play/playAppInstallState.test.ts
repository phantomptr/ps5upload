import { describe, expect, it } from "vitest";

import { playAppErrorText, playAppStatusText, readPlayAppStatus } from "./playAppInstallState";

const tr = (_key: string, _vars: Record<string, string | number> | undefined, fallback: string) => fallback;

const base = {
  title_id: "FAKE99261",
  bundled: { version: "13.0.0", content_version: "13.000.000" },
  installed: null,
  running: false,
  store: { managed: false, busy: false },
  action: "install",
};

describe("PS5Upload Play on a PS5", () => {
  it("reads the engine's answer, and nothing from an older engine", () => {
    expect(readPlayAppStatus(null)).toBeNull();
    expect(readPlayAppStatus({ apps: [] })).toBeNull();
    const s = readPlayAppStatus(base)!;
    expect(s.bundled).toEqual({ version: "13.0.0" });
    expect(s.installed).toBeNull();
    expect(s.action).toBe("install");
    expect(playAppStatusText(tr, s)).toBe("Not on this PS5 yet. PS5Upload can put version 13.0.0 on it.");
  });

  it("says when this build has no copy", () => {
    const s = readPlayAppStatus({ ...base, bundled: null, bundled_missing: "not_included", action: "unavailable" })!;
    expect(s.bundledMissing).toBe("not_included");
    expect(playAppStatusText(tr, s)).toMatch(/doesn't include PS5Upload Play/);
  });

  it("offers an update for an older install, and tells another installer's install apart", () => {
    const s = readPlayAppStatus({
      ...base,
      bundled: { version: "13.1.0" },
      installed: { version: "13.0.0", path: "/data/homebrew/FAKE99261", registered: true, registered_by: null },
      store: { managed: true, busy: false },
      action: "update",
    })!;
    expect(s.action).toBe("update");
    expect(s.otherInstaller).toBe(true);
    expect(playAppStatusText(tr, s)).toBe("Version 13.0.0 is on this PS5. Version 13.1.0 is ready to install.");
    // Our own install writes the same record: not "another app" then.
    const ours = readPlayAppStatus({
      ...base,
      installed: { version: "13.0.0", registered: true, registered_by: "shadowmount" },
      store: { managed: true, busy: false },
      action: "none",
    })!;
    expect(ours.otherInstaller).toBe(false);
    expect(playAppStatusText(tr, ours)).toBe("Version 13.0.0 is on this PS5.");
  });

  it("an unknown action offers nothing", () => {
    expect(readPlayAppStatus({ ...base, action: "explode" })!.action).toBe("unavailable");
  });

  it("puts refusals in plain words", () => {
    expect(playAppErrorText(tr, "store_busy", null)).toMatch(/Another app is changing apps/);
    expect(playAppErrorText(tr, "app_running", null)).toMatch(/wouldn't close/);
    expect(playAppErrorText(tr, null, "boom")).toBe("Didn't work: boom");
    expect(playAppErrorText(tr, null, null)).toMatch(/connected/);
  });
});
