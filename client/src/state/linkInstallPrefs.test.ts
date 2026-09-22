import { beforeEach, describe, expect, it } from "vitest";
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

  /* A first-time user gets the mode that needs no explanation and no awake
   * computer, and which is faster on a healthy source. */
  it("defaults to direct for an unknown host", () => {
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.5")).toBe("direct");
  });

  /* The right mode follows the LINK, and two consoles can sit behind
   * different ones — so the choice cannot be global. */
  it("remembers a choice per host", () => {
    useLinkInstallPrefs.getState().setMode("10.0.0.5", "accelerated");
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.5")).toBe(
      "accelerated",
    );
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.6")).toBe("direct");
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
    expect(() => useLinkInstallPrefs.getState().modeFor("h")).not.toThrow();
    expect(useLinkInstallPrefs.getState().insecureFor("h")).toBe(false);
  });
});
