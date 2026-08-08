import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const sensorMocks = vi.hoisted(() => ({
  fetchTemps: vi.fn(),
  fetchPower: vi.fn(),
  transferBusy: vi.fn(),
}));

vi.mock("../api/ps5", () => ({
  fetchHwTemps: sensorMocks.fetchTemps,
  fetchHwPower: sensorMocks.fetchPower,
}));

vi.mock("../lib/ps5Transfers", () => ({
  transferScreenBusy: sensorMocks.transferBusy,
}));

import {
  useSensorsStore,
  subscribeSensor,
  unsubscribeSensor,
  _refcountForTest,
  type SensorSample,
} from "./sensors";
import { EMPTY_HOST_RUNTIME, useConnectionStore } from "./connection";
import { useTaskStore } from "./tasks";

function resetStore() {
  useSensorsStore.setState({ byHost: {} });
  useConnectionStore.setState({
    host: "",
    runtimeByHost: {},
    payloadStatus: "unknown",
    payloadStatusHost: null,
  });
  sensorMocks.fetchTemps.mockReset();
  sensorMocks.fetchPower.mockReset();
  sensorMocks.transferBusy.mockReset();
  sensorMocks.transferBusy.mockReturnValue(false);
  useTaskStore.setState({ tasks: [] });
}

const fakeTemps = (cpu: number, fan = 30) =>
  ({
    cpu_temp: cpu,
    soc_temp: cpu - 5,
    m2_temp: 0,
    cpu_freq_mhz: 3000,
    soc_clock_mhz: 2000,
    soc_power_mw: 5000,
    cpu_usage_pct: 20,
    fan_duty_pct: fan,
    product_shape: 1,
    fan_pinned_c: 0,
  }) as never;

const fakePower = (hours = 100, boots = 50) =>
  ({
    operating_time_sec: hours * 3600,
    operating_time_hours: hours,
    operating_time_minutes: hours * 60,
    boot_count: boots,
    power_consumption_mw: 80_000,
    load_avg_1m: 0.5,
    load_avg_5m: 0.4,
    load_avg_15m: 0.3,
  }) as never;

function makeSample(cpu: number, ts: number): SensorSample {
  return { ts, temps: fakeTemps(cpu), power: fakePower() };
}

describe("sensors store — record()", () => {
  beforeEach(() => resetStore());
  afterEach(() => resetStore());

  it("stores the first sample for a host", () => {
    const sample = makeSample(65, 1000);
    useSensorsStore.getState().record("192.168.1.1:9090", sample);
    const bucket = useSensorsStore.getState().byHost["192.168.1.1"];
    expect(bucket).toBeDefined();
    expect(bucket!.samples).toHaveLength(1);
    expect(bucket!.latest).toBe(sample);
  });

  it("appends to existing history", () => {
    useSensorsStore.getState().record("host:9090", makeSample(60, 1000));
    useSensorsStore.getState().record("host:9090", makeSample(65, 2000));
    const bucket = useSensorsStore.getState().byHost["host"]!;
    expect(bucket.samples).toHaveLength(2);
    expect(bucket.latest!.temps!.cpu_temp).toBe(65);
  });

  it("normalizes host:port → host key", () => {
    useSensorsStore.getState().record("192.168.1.1:9090", makeSample(60, 1000));
    expect(useSensorsStore.getState().byHost["192.168.1.1"]).toBeDefined();
    expect(useSensorsStore.getState().byHost["192.168.1.1:9090"]).toBeUndefined();
  });

  it("ring buffer trims to MAX_SAMPLES (120)", () => {
    for (let i = 0; i < 150; i++) {
      useSensorsStore.getState().record("host:9090", makeSample(60 + i, i * 1000));
    }
    const bucket = useSensorsStore.getState().byHost["host"]!;
    expect(bucket.samples).toHaveLength(120);
    // Oldest 30 trimmed — first sample is index 30
    expect(bucket.samples[0].ts).toBe(30_000);
    expect(bucket.latest!.temps!.cpu_temp).toBe(60 + 149);
  });

  it("isolates samples per host", () => {
    useSensorsStore.getState().record("hostA:9090", makeSample(60, 1000));
    useSensorsStore.getState().record("hostB:9090", makeSample(70, 1000));
    const state = useSensorsStore.getState();
    expect(state.byHost["hostA"]!.latest!.temps!.cpu_temp).toBe(60);
    expect(state.byHost["hostB"]!.latest!.temps!.cpu_temp).toBe(70);
  });
});

describe("sensors store — clear()", () => {
  beforeEach(() => resetStore());
  afterEach(() => resetStore());

  it("removes a host's bucket", () => {
    useSensorsStore.getState().record("host:9090", makeSample(60, 1000));
    expect(useSensorsStore.getState().byHost["host"]).toBeDefined();
    useSensorsStore.getState().clear("host:9090");
    expect(useSensorsStore.getState().byHost["host"]).toBeUndefined();
  });

  it("is a no-op for unknown hosts (state reference unchanged)", () => {
    const before = useSensorsStore.getState();
    useSensorsStore.getState().clear("never-seen");
    expect(useSensorsStore.getState()).toBe(before);
  });
});

describe("subscribeSensor / unsubscribeSensor refcounting", () => {
  beforeEach(() => resetStore());
  afterEach(() => {
    resetStore();
    // Drain any handles we might have left
    for (const h of ["hostA", "hostB", "192.168.1.1", "never-subscribed"]) {
      while (_refcountForTest(h) > 0) unsubscribeSensor(h);
    }
  });

  it("subscribe increments refcount; unsubscribe decrements", () => {
    subscribeSensor("hostA:9090");
    expect(_refcountForTest("hostA")).toBe(1);
    subscribeSensor("hostA:9090");
    expect(_refcountForTest("hostA")).toBe(2);
    unsubscribeSensor("hostA:9090");
    expect(_refcountForTest("hostA")).toBe(1);
    unsubscribeSensor("hostA:9090");
    expect(_refcountForTest("hostA")).toBe(0);
  });

  it("unsubscribe with no subscription is a no-op (does not go negative)", () => {
    expect(() => unsubscribeSensor("never-subscribed")).not.toThrow();
    expect(_refcountForTest("never-subscribed")).toBe(0);
  });

  it("subscribe normalizes host:port → host key", () => {
    subscribeSensor("192.168.1.1:9090");
    // _refcountForTest normalizes via hostOf, so both forms resolve
    // to the same key "192.168.1.1"
    expect(_refcountForTest("192.168.1.1")).toBe(1);
    expect(_refcountForTest("192.168.1.1:9090")).toBe(1);
    unsubscribeSensor("192.168.1.1:9090");
    expect(_refcountForTest("192.168.1.1")).toBe(0);
  });

  it("multiple unsubscribe calls below zero stay at zero", () => {
    subscribeSensor("hostA:9090");
    unsubscribeSensor("hostA:9090");
    expect(_refcountForTest("hostA")).toBe(0);
    unsubscribeSensor("hostA:9090");
    expect(_refcountForTest("hostA")).toBe(0);
  });
});

describe("sensor polling safety", () => {
  beforeEach(() => resetStore());
  afterEach(() => {
    for (const host of ["hostA", "hostB"]) {
      while (_refcountForTest(host) > 0) unsubscribeSensor(host);
    }
    resetStore();
  });

  it("uses the polled host's runtime instead of the active console mirror", async () => {
    sensorMocks.fetchTemps.mockResolvedValue(fakeTemps(64));
    sensorMocks.fetchPower.mockResolvedValue(fakePower());
    useConnectionStore.setState({
      host: "hostA",
      payloadStatus: "down",
      payloadStatusHost: "hostA",
      runtimeByHost: {
        hostA: { ...EMPTY_HOST_RUNTIME, payloadStatus: "down" },
        hostB: { ...EMPTY_HOST_RUNTIME, payloadStatus: "up" },
      },
    });

    subscribeSensor("hostB:9113");

    await vi.waitFor(() => {
      expect(sensorMocks.fetchTemps).toHaveBeenCalledOnce();
      expect(sensorMocks.fetchPower).toHaveBeenCalledOnce();
    });
  });

  it("does not borrow an up status from another console", async () => {
    useConnectionStore.setState({
      host: "hostA",
      payloadStatus: "up",
      payloadStatusHost: "hostA",
      runtimeByHost: {
        hostA: { ...EMPTY_HOST_RUNTIME, payloadStatus: "up" },
        hostB: { ...EMPTY_HOST_RUNTIME, payloadStatus: "down" },
      },
    });

    subscribeSensor("hostB:9113");
    await Promise.resolve();

    expect(sensorMocks.fetchTemps).not.toHaveBeenCalled();
    expect(sensorMocks.fetchPower).not.toHaveBeenCalled();
  });

  it("pauses all sensor RPCs while that console has an active transfer", async () => {
    sensorMocks.transferBusy.mockReturnValue(true);
    useConnectionStore.setState({
      host: "hostA",
      payloadStatus: "up",
      payloadStatusHost: "hostA",
      runtimeByHost: {
        hostA: { ...EMPTY_HOST_RUNTIME, payloadStatus: "up" },
      },
    });

    subscribeSensor("hostA:9113");
    await Promise.resolve();

    expect(sensorMocks.transferBusy).toHaveBeenCalledWith("hostA:9113");
    expect(sensorMocks.fetchTemps).not.toHaveBeenCalled();
    expect(sensorMocks.fetchPower).not.toHaveBeenCalled();
  });

  it("pauses sensor RPCs while that console has an active package install", async () => {
    useConnectionStore.setState({
      host: "hostA",
      payloadStatus: "up",
      payloadStatusHost: "hostA",
      runtimeByHost: {
        hostA: { ...EMPTY_HOST_RUNTIME, payloadStatus: "up" },
      },
    });
    useTaskStore.getState().registerTask({
      kind: "pkg-install",
      origin: "test",
      label: "Installing package",
      consoleId: "hostA",
    });

    subscribeSensor("hostA:9113");
    await Promise.resolve();

    expect(sensorMocks.fetchTemps).not.toHaveBeenCalled();
    expect(sensorMocks.fetchPower).not.toHaveBeenCalled();
  });

  it("records a temperature sample when the power endpoint is unavailable", async () => {
    const temps = fakeTemps(67);
    sensorMocks.fetchTemps.mockResolvedValue(temps);
    sensorMocks.fetchPower.mockRejectedValue(new Error("unsupported"));
    useConnectionStore.setState({
      host: "hostA",
      payloadStatus: "up",
      payloadStatusHost: "hostA",
      runtimeByHost: {
        hostA: { ...EMPTY_HOST_RUNTIME, payloadStatus: "up" },
      },
    });

    subscribeSensor("hostA:9113");

    await vi.waitFor(() => {
      const latest = useSensorsStore.getState().byHost.hostA?.latest;
      expect(latest?.temps).toBe(temps);
      expect(latest?.power).toBeNull();
    });
  });
});
