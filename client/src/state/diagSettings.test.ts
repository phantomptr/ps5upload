import { describe, expect, it, beforeEach, afterEach } from "vitest";
import {
  LEVEL_RANK,
  LOG_LEVELS,
  DEFAULT_LOG_LEVEL,
  currentMinRank,
  useDiagSettingsStore,
} from "./diagSettings";

/**
 * The disk-log sink in `logs.ts` writes an entry only when its level rank is
 * ≥ the configured minimum. A regression in this ordering would silently drop
 * (or over-capture) log lines from bug reports, so pin it down.
 */
describe("diagSettings level ranks", () => {
  it("orders trace < debug < info < warn < error", () => {
    expect(LEVEL_RANK.trace).toBeLessThan(LEVEL_RANK.debug);
    expect(LEVEL_RANK.debug).toBeLessThan(LEVEL_RANK.info);
    expect(LEVEL_RANK.info).toBeLessThan(LEVEL_RANK.warn);
    expect(LEVEL_RANK.warn).toBeLessThan(LEVEL_RANK.error);
  });

  it("LOG_LEVELS lists most-verbose first and matches the rank keys", () => {
    expect(LOG_LEVELS).toEqual(["trace", "debug", "info", "warn", "error"]);
    expect(new Set(LOG_LEVELS)).toEqual(new Set(Object.keys(LEVEL_RANK)));
  });

  it("defaults to info", () => {
    expect(DEFAULT_LOG_LEVEL).toBe("info");
  });
});

// vitest's default node env has no `window`; the store reads
// `window.localStorage`. Install a tiny in-memory stub (mirrors the pattern
// in lib/fsLastPath.test.ts — cheaper than pulling in jsdom) so the real
// persist/clear branches run.
function installWindowStub() {
  const store = new globalThis.Map<string, string>();
  const localStorage = {
    getItem: (k: string) => (store.has(k) ? (store.get(k) as string) : null),
    setItem: (k: string, v: string) => void store.set(k, String(v)),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
  };
  (globalThis as { window?: unknown }).window = { localStorage };
}

describe("diagSettings store", () => {
  beforeEach(() => {
    installWindowStub();
    useDiagSettingsStore.setState({ logLevel: DEFAULT_LOG_LEVEL });
  });

  afterEach(() => {
    delete (globalThis as { window?: unknown }).window;
  });

  it("currentMinRank reflects the store", () => {
    useDiagSettingsStore.getState().setLogLevel("trace");
    expect(currentMinRank()).toBe(LEVEL_RANK.trace);
    useDiagSettingsStore.getState().setLogLevel("error");
    expect(currentMinRank()).toBe(LEVEL_RANK.error);
  });

  it("persists non-default levels and clears the key on default", () => {
    useDiagSettingsStore.getState().setLogLevel("debug");
    expect(window.localStorage.getItem("ps5upload.log_level")).toBe("debug");
    // Returning to the default removes the key (absence = default).
    useDiagSettingsStore.getState().setLogLevel("info");
    expect(window.localStorage.getItem("ps5upload.log_level")).toBeNull();
  });
});
