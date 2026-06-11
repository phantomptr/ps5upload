import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";
import { useInstallSettingsStore } from "./installSettings";

// vitest's default node env has no `window`; the store reads
// `window.localStorage`. Install a tiny in-memory stub (same pattern as
// diagSettings.test.ts) so the real persist branches run.
function installWindowStub(seed?: Record<string, string>) {
  const store = new globalThis.Map<string, string>(
    seed ? Object.entries(seed) : [],
  );
  const localStorage = {
    getItem: (k: string) => (store.has(k) ? (store.get(k) as string) : null),
    setItem: (k: string, v: string) => void store.set(k, String(v)),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
  };
  (globalThis as { window?: unknown }).window = { localStorage };
}

const KEY = "ps5upload.auto_install_after_upload";

describe("installSettings — autoInstallAfterUpload", () => {
  beforeEach(() => {
    installWindowStub();
  });

  afterEach(() => {
    delete (globalThis as { window?: unknown }).window;
    vi.resetModules();
  });

  it("setter persists '1' when on and '0' when off, and mirrors state", () => {
    const s = useInstallSettingsStore.getState();
    s.setAutoInstallAfterUpload(false);
    expect(window.localStorage.getItem(KEY)).toBe("0");
    expect(useInstallSettingsStore.getState().autoInstallAfterUpload).toBe(
      false,
    );

    s.setAutoInstallAfterUpload(true);
    expect(window.localStorage.getItem(KEY)).toBe("1");
    expect(useInstallSettingsStore.getState().autoInstallAfterUpload).toBe(true);
  });

  // The default is opt-OUT: a fresh user (no stored key) gets auto-install ON,
  // which is the maintainer-requested "upload → installed" hands-off flow.
  // Only an explicit "0" turns it off. Pin this so a future refactor of the
  // loader (e.g. flipping to `=== "1"`) can't silently make it opt-in.
  it("defaults ON when the key is absent (opt-out)", async () => {
    installWindowStub(); // no seeded key
    vi.resetModules();
    const mod = await import("./installSettings");
    expect(mod.useInstallSettingsStore.getState().autoInstallAfterUpload).toBe(
      true,
    );
  });

  it("loads OFF only for an explicit '0'", async () => {
    installWindowStub({ [KEY]: "0" });
    vi.resetModules();
    const mod = await import("./installSettings");
    expect(mod.useInstallSettingsStore.getState().autoInstallAfterUpload).toBe(
      false,
    );
  });

  it("treats any non-'0' stored value as ON", async () => {
    installWindowStub({ [KEY]: "1" });
    vi.resetModules();
    const mod = await import("./installSettings");
    expect(mod.useInstallSettingsStore.getState().autoInstallAfterUpload).toBe(
      true,
    );
  });
});
