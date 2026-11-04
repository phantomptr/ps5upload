import { describe, expect, it, beforeEach, afterEach } from "vitest";
import {
  LEVEL_RANK,
  LOG_LEVELS,
  DEFAULT_LOG_LEVEL,
  currentMinRank,
  useDiagSettingsStore,
  DETAILED_LOGS_MS,
  KEY_DETAILED_UNTIL,
  detailedActive,
} from "./diagSettings";
import { installWindowStub } from "../test-utils/storage";

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

describe("diagSettings store", () => {
  beforeEach(() => {
    installWindowStub();
    useDiagSettingsStore.setState({ logLevel: DEFAULT_LOG_LEVEL, detailedUntil: null });
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

  it("Detailed logs record debug lines for a day, then stop by themselves", () => {
    const now = 1_800_000_000_000;
    useDiagSettingsStore.getState().setDetailed(true, now);
    const until = useDiagSettingsStore.getState().detailedUntil;
    expect(until).toBe(now + DETAILED_LOGS_MS);
    expect(window.localStorage.getItem(KEY_DETAILED_UNTIL)).toBe(String(now + DETAILED_LOGS_MS));
    // info is the chosen level, but debug lines are recorded while it is on…
    expect(currentMinRank(now + 1000)).toBe(LEVEL_RANK.debug);
    // …and a more verbose choice stays.
    useDiagSettingsStore.getState().setLogLevel("trace");
    expect(currentMinRank(now + 1000)).toBe(LEVEL_RANK.trace);
    useDiagSettingsStore.getState().setLogLevel("info");
    // Its time is up: back to the chosen level even before the timer turns it off.
    expect(detailedActive(until, now + DETAILED_LOGS_MS)).toBe(false);
    expect(currentMinRank(now + DETAILED_LOGS_MS + 1)).toBe(LEVEL_RANK.info);
    useDiagSettingsStore.getState().setDetailed(false);
    expect(useDiagSettingsStore.getState().detailedUntil).toBeNull();
    expect(window.localStorage.getItem(KEY_DETAILED_UNTIL)).toBeNull();
  });
});
