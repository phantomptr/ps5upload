import { describe, expect, it, beforeEach, afterEach } from "vitest";
import {
  LEVEL_RANK,
  LOG_LEVELS,
  LOG_CATEGORIES,
  DEFAULT_LOG_LEVEL,
  categoryOf,
  currentMinRank,
  effectiveLevel,
  levelsSummary,
  logEnabled,
  raiseActive,
  useDiagSettingsStore,
  type EngineLevels,
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

describe("categories of app log sources", () => {
  it("map the app's sources onto the engine's categories", () => {
    expect(categoryOf("remote-play")).toBe("rp");
    expect(categoryOf("upload")).toBe("transfer");
    expect(categoryOf("install")).toBe("install");
    expect(categoryOf("console")).toBe("ui");
    expect(categoryOf("engine")).toBe("general");
    expect(categoryOf("engine:rp")).toBe("rp");
    expect(categoryOf("engine:nope")).toBe("general");
    expect(categoryOf("something-else")).toBe("general");
    for (const s of ["remote-play", "upload", "install", "cleanup", "wake"])
      expect(LOG_CATEGORIES).toContain(categoryOf(s));
  });
});

describe("diagSettings store", () => {
  const now = 1_800_000_000_000;
  beforeEach(() => {
    installWindowStub();
    useDiagSettingsStore.setState({ logLevel: DEFAULT_LOG_LEVEL, engine: null });
  });

  afterEach(() => {
    delete (globalThis as { window?: unknown }).window;
  });

  it("without the engine's levels the local level gates, never above info", () => {
    useDiagSettingsStore.getState().setLogLevel("error");
    expect(currentMinRank()).toBe(LEVEL_RANK.error);
    // A raise needs an end, which only the engine keeps: the local level stays at info.
    useDiagSettingsStore.getState().setLogLevel("trace");
    expect(currentMinRank()).toBe(LEVEL_RANK.info);
    expect(levelsSummary().source).toBe("app");
  });

  it("persists non-default levels and clears the key on default", () => {
    useDiagSettingsStore.getState().setLogLevel("warn");
    expect(window.localStorage.getItem("ps5upload.log_level")).toBe("warn");
    useDiagSettingsStore.getState().setLogLevel("info");
    expect(window.localStorage.getItem("ps5upload.log_level")).toBeNull();
  });

  it("the engine's levels gate by category, and a raise ends on time", () => {
    const levels: EngineLevels = { level: "warn", categories: { rp: "trace" }, until_ms: now + 60_000 };
    useDiagSettingsStore.getState().setEngineLevels(levels);
    expect(currentMinRank(now, "remote-play")).toBe(LEVEL_RANK.trace);
    expect(currentMinRank(now, "upload")).toBe(LEVEL_RANK.warn);
    expect(logEnabled("debug", "engine:rp", now)).toBe(true);
    expect(logEnabled("info", "install", now)).toBe(false);
    expect(raiseActive(levels, now)).toBe(true);
    // The raise ran out: rp writes info again; the lowered global level stays.
    expect(effectiveLevel(levels, "rp", now + 60_000)).toBe("info");
    expect(effectiveLevel(levels, "install", now + 60_000)).toBe("warn");
    expect(raiseActive(levels, now + 60_000)).toBe(false);
  });

  it("a report says which levels were in force", () => {
    useDiagSettingsStore.getState().setEngineLevels({
      level: "debug",
      categories: {},
      until_ms: now + 1000,
    });
    const s = levelsSummary(now);
    expect(s).toMatchObject({ level: "debug", raised: true, source: "engine" });
    expect(s.until).toBe(new Date(now + 1000).toISOString());
    expect(levelsSummary(now + 2000).raised).toBe(false);
  });
});
