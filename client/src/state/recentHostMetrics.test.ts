import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { MAX_AGE_MS, useRecentHostMetricsStore } from "./recentHostMetrics";

function resetStore() {
  useRecentHostMetricsStore.setState({ entries: {} });
}

describe("useRecentHostMetricsStore", () => {
  beforeEach(() => {
    resetStore();
    if (typeof window !== "undefined") {
      window.localStorage.clear();
    }
  });

  afterEach(() => {
    resetStore();
  });

  it("lookup returns undefined when no measurement exists", () => {
    expect(
      useRecentHostMetricsStore.getState().lookup("192.168.1.10:9113"),
    ).toBeUndefined();
  });

  it("record + lookup round-trips on the same addr", () => {
    useRecentHostMetricsStore.getState().record("192.168.1.10:9113", {
      throughputMibps: 95.5,
      commitMsPerFile: 12,
      measuredAtMs: 1_700_000_000_000,
    });
    const got = useRecentHostMetricsStore
      .getState()
      .lookup("192.168.1.10:9113");
    expect(got).toBeDefined();
    expect(got!.throughputMibps).toBe(95.5);
    expect(got!.commitMsPerFile).toBe(12);
    expect(got!.measuredAtMs).toBe(1_700_000_000_000);
  });

  it("lookup ignores the port — same host, different port resolves the same record", () => {
    // Keyed on host so a payload-port change (e.g. a future PS5 firmware
    // that moves the transfer port) doesn't orphan the measurement.
    useRecentHostMetricsStore.getState().record("192.168.1.10:9113", {
      throughputMibps: 100,
      measuredAtMs: 1,
    });
    expect(
      useRecentHostMetricsStore.getState().lookup("192.168.1.10:9114"),
    ).toBeDefined();
    expect(
      useRecentHostMetricsStore.getState().lookup("192.168.1.10:65535"),
    ).toBeDefined();
  });

  it("record overwrites a prior measurement for the same host", () => {
    useRecentHostMetricsStore.getState().record("192.168.1.10:9113", {
      throughputMibps: 50,
      measuredAtMs: 1,
    });
    useRecentHostMetricsStore.getState().record("192.168.1.10:9113", {
      throughputMibps: 110,
      commitMsPerFile: 8,
      measuredAtMs: 2,
    });
    const got = useRecentHostMetricsStore
      .getState()
      .lookup("192.168.1.10:9113");
    expect(got!.throughputMibps).toBe(110);
    expect(got!.commitMsPerFile).toBe(8);
    expect(got!.measuredAtMs).toBe(2);
  });

  it("isolates measurements per host", () => {
    useRecentHostMetricsStore.getState().record("192.168.1.10:9113", {
      throughputMibps: 100,
      measuredAtMs: 1,
    });
    useRecentHostMetricsStore.getState().record("10.0.0.5:9113", {
      throughputMibps: 30,
      measuredAtMs: 1,
    });
    expect(
      useRecentHostMetricsStore.getState().lookup("192.168.1.10:9113")!
        .throughputMibps,
    ).toBe(100);
    expect(
      useRecentHostMetricsStore.getState().lookup("10.0.0.5:9113")!
        .throughputMibps,
    ).toBe(30);
  });

  it("clear() removes every record", () => {
    useRecentHostMetricsStore.getState().record("192.168.1.10:9113", {
      throughputMibps: 100,
      measuredAtMs: 1,
    });
    useRecentHostMetricsStore.getState().record("10.0.0.5:9113", {
      throughputMibps: 30,
      measuredAtMs: 1,
    });
    useRecentHostMetricsStore.getState().clear();
    expect(
      useRecentHostMetricsStore.getState().lookup("192.168.1.10:9113"),
    ).toBeUndefined();
    expect(
      useRecentHostMetricsStore.getState().lookup("10.0.0.5:9113"),
    ).toBeUndefined();
  });

  it("MAX_AGE_MS pins the documented 7-day window", () => {
    // The exact value matters for the consumer's staleness check; if a
    // future PR changes this constant the consumer logic that reads it
    // needs corresponding updates. Pin so the change is intentional.
    expect(MAX_AGE_MS).toBe(7 * 24 * 60 * 60 * 1000);
  });
});
