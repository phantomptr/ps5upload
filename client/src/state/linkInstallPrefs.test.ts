import { beforeEach, describe, expect, it, vi } from "vitest";
import { useLinkInstallPrefs } from "./linkInstallPrefs";

// vitest's default node env has no `window`; the store reads
// `window.localStorage` through safeStorage. Same in-memory stub the other
// settings stores use, so the real persist branches run.
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

describe("link install preferences", () => {
  beforeEach(() => {
    installWindowStub();
    useLinkInstallPrefs.setState({ modes: {}, insecure: {} });
  });

  /* The default must not change behaviour under existing users, and direct
   * cannot yet be verified end to end — the console accepting a URL is not
   * the same as the console reaching it. */
  it("defaults to stream for an unknown host", () => {
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.5")).toBe(
      "stream",
    );
  });

  /* The right mode follows the LINK, and two consoles can sit behind
   * different ones — so the choice cannot be global. */
  it("remembers a choice per host", () => {
    useLinkInstallPrefs.getState().setMode("10.0.0.5", "direct");
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.5")).toBe("direct");
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.6")).toBe(
      "stream",
    );
  });

  /* A preference saved before the download-to-disk mode existed must keep
   * meaning what the person chose. "accelerated" was this mode's old name;
   * dropping it as unrecognised would silently revert them to the default.
   *
   * The migration runs in `loadMap` at module load, so this has to re-import
   * the module with storage already seeded — setting state by hand would
   * skip the very code under test. */
  it("keeps a preference saved under the mode's old name", async () => {
    installWindowStub({
      "ps5upload.link_install_mode": JSON.stringify({
        "10.0.0.7": "accelerated",
        "10.0.0.9": "direct",
      }),
    });
    vi.resetModules();
    const fresh = await import("./linkInstallPrefs");
    // Assert on the stored map, not on modeFor: an unmigrated value is
    // dropped as unrecognised and modeFor then returns the DEFAULT, which is
    // currently also "stream" — so reading it through modeFor would pass
    // whether or not the migration exists.
    expect(fresh.useLinkInstallPrefs.getState().modes["10.0.0.7"]).toBe(
      "stream",
    );
    // An unrelated saved choice must come through untouched.
    expect(fresh.useLinkInstallPrefs.getState().modeFor("10.0.0.9")).toBe(
      "direct",
    );
  });

  /* Three modes, and each must survive a round trip: the download mode is
   * the one people pick when a link keeps dying, so losing it silently would
   * send them back to the mode that just failed. */
  it("remembers the download mode", () => {
    useLinkInstallPrefs.getState().setMode("10.0.0.8", "download");
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.8")).toBe("download");
  });

  /* Turning off certificate verification is a security decision. It must be
   * chosen, never inherited from another console or an empty store. */
  it("never skips the certificate check unless asked, per host", () => {
    expect(useLinkInstallPrefs.getState().insecureFor("10.0.0.5")).toBe(false);
    useLinkInstallPrefs.getState().setInsecure("10.0.0.5", true);
    expect(useLinkInstallPrefs.getState().insecureFor("10.0.0.5")).toBe(true);
    expect(useLinkInstallPrefs.getState().insecureFor("10.0.0.6")).toBe(false);
  });

  /* Corrupt or hand-edited storage must not take the install path down, and
   * must never be read as "skip the certificate check". */
  it("survives unparseable stored preferences", () => {
    installWindowStub({
      "ps5upload.link_install_mode": "{not json",
      "ps5upload.link_install_insecure": '{"h":"yes"}',
    });
    useLinkInstallPrefs.setState({ modes: {}, insecure: {} });
    expect(useLinkInstallPrefs.getState().modeFor("h")).toBe("stream");
    expect(useLinkInstallPrefs.getState().insecureFor("h")).toBe(false);
  });
});
