import { describe, expect, it } from "vitest";

import type { SensorSample } from "../state/sensors";
import {
  buildTelemetrySeries,
  temperatureTone,
  usablePowerWatts,
  usableTemperature,
} from "./TelemetryDashboard";

const temps = (cpu: number, fan = -1) =>
  ({
    cpu_temp: cpu,
    soc_temp: cpu - 4,
    m2_temp: 0,
    cpu_freq_mhz: 800,
    soc_clock_mhz: 0,
    soc_power_mw: 0,
    cpu_usage_pct: -1,
    fan_duty_pct: fan,
    product_shape: -1,
    fan_pinned_c: 0,
  }) as never;

const power = (mw: number) =>
  ({
    operating_time_sec: 1,
    operating_time_hours: 0,
    operating_time_minutes: 0,
    boot_count: 1,
    power_consumption_mw: mw,
    load_avg_1m: -1,
    load_avg_5m: -1,
    load_avg_15m: -1,
  }) as never;

describe("telemetry safety helpers", () => {
  it("makes the critical temperature band reachable", () => {
    expect(temperatureTone(84.9)).toBeUndefined();
    expect(temperatureTone(85)).toBe("warn");
    expect(temperatureTone(94.9)).toBe("warn");
    expect(temperatureTone(95)).toBe("bad");
  });

  it("treats zero/negative power as unavailable", () => {
    expect(usablePowerWatts(undefined)).toBeNull();
    expect(usablePowerWatts(0)).toBeNull();
    expect(usablePowerWatts(-1)).toBeNull();
    expect(usablePowerWatts(80_000)).toBe(80);
  });

  it("treats zero/negative temperatures as unavailable", () => {
    expect(usableTemperature(undefined)).toBeNull();
    expect(usableTemperature(0)).toBeNull();
    expect(usableTemperature(-1)).toBeNull();
    expect(usableTemperature(67)).toBe(67);
  });

  it("keeps independent series when one endpoint fails", () => {
    const history: SensorSample[] = [
      { ts: 1, temps: temps(60), power: null },
      { ts: 2, temps: null, power: power(75_000) },
      { ts: 3, temps: temps(62, 35), power: power(0) },
      { ts: 4, temps: temps(0), power: null },
    ];

    expect(buildTelemetrySeries(history)).toEqual({
      cpuTemp: [60, 62],
      socTemp: [56, 58],
      fanDuty: [35],
      power: [75],
    });
  });
});
