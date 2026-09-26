/**
 * Telemetry dashboard (v5 §11.4).
 *
 * Pulls from the shared, transfer-aware sensor store. Temperature and power
 * reads are independent because not every firmware exposes both endpoints.
 * Unsupported values render as an em dash instead of a plausible-looking 0.
 */
import { useMemo } from "react";
import { Activity, Thermometer, Fan, Zap, Clock } from "lucide-react";

import { Card, EmptyState, Sparkline, Badge } from "./index";
import { useTr } from "../state/lang";
import { useConnectionStore } from "../state/connection";
import { useSensors, type SensorSample } from "../state/sensors";
import { formatDuration } from "../lib/format";

export type TelemetryTone = "warn" | "bad" | undefined;

/** Temperature severity, ordered hottest-first so the critical band is
 * reachable. Exported to keep the safety thresholds directly unit-testable. */
export function temperatureTone(
  value: number | null | undefined,
): TelemetryTone {
  if (value == null) return undefined;
  if (value >= 95) return "bad";
  if (value >= 85) return "warn";
  return undefined;
}

/** Basic firmware reads commonly return zero for unsupported live power.
 * Treat it as unavailable; a real powered-on console cannot consume 0 W. */
export function usablePowerWatts(
  milliwatts: number | null | undefined,
): number | null {
  return milliwatts != null && milliwatts > 0 ? milliwatts / 1000 : null;
}

/** A zero/negative temperature is the payload's unsupported/error sentinel,
 * not a physically plausible reading from a powered-on console. */
export function usableTemperature(
  value: number | null | undefined,
): number | null {
  return value != null && value > 0 ? value : null;
}

export interface TelemetrySeries {
  cpuTemp: number[];
  socTemp: number[];
  fanDuty: number[];
  power: number[];
}

/** Build independent series so a failed/unsupported power read never erases a
 * valid temperature trend (and vice versa). */
export function buildTelemetrySeries(history: SensorSample[]): TelemetrySeries {
  return {
    cpuTemp: history.flatMap((sample) => {
      const value = usableTemperature(sample.temps?.cpu_temp);
      return value == null ? [] : [value];
    }),
    socTemp: history.flatMap((sample) => {
      const value = usableTemperature(sample.temps?.soc_temp);
      return value == null ? [] : [value];
    }),
    fanDuty: history.flatMap((sample) =>
      sample.temps && sample.temps.fan_duty_pct >= 0
        ? [sample.temps.fan_duty_pct]
        : [],
    ),
    power: history.flatMap((sample) => {
      const watts = usablePowerWatts(sample.power?.power_consumption_mw);
      return watts == null ? [] : [watts];
    }),
  };
}

function temperatureColor(value: number | null): string {
  const tone = temperatureTone(value);
  if (tone === "bad") return "var(--color-bad)";
  if (tone === "warn") return "var(--color-warn)";
  return "var(--color-text)";
}

export function TelemetryDashboard() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const { sample, history } = useSensors(host);

  const connected = payloadStatus === "up";
  const series = useMemo(() => buildTelemetrySeries(history), [history]);

  if (!connected) {
    return (
      <EmptyState
        icon={Activity}
        title={tr("telemetry_not_connected", undefined, "Not connected")}
        message={tr(
          "telemetry_not_connected_desc",
          undefined,
          "Connect to your PS5 to see live telemetry.",
        )}
      />
    );
  }

  if (!sample || (!sample.temps && !sample.power)) {
    return (
      <EmptyState
        icon={Activity}
        title={tr("telemetry_waiting", undefined, "Waiting for data")}
        message={tr(
          "telemetry_waiting_desc",
          undefined,
          "Sensor readings will appear here within a few seconds.",
        )}
      />
    );
  }

  const temps = sample.temps;
  const power = sample.power;
  const cpuTemp = usableTemperature(temps?.cpu_temp);
  const socTemp = usableTemperature(temps?.soc_temp);
  const fanDuty = temps?.fan_duty_pct ?? null;
  const powerW = usablePowerWatts(power?.power_consumption_mw);

  return (
    <div className="space-y-4">
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <MetricCard
          icon={Thermometer}
          label={tr("telemetry_cpu_temp", undefined, "CPU Temp")}
          value={cpuTemp == null ? "—" : `${cpuTemp.toFixed(0)}°C`}
          tone={temperatureTone(cpuTemp)}
          sparkData={series.cpuTemp}
          sparkColor={temperatureColor(cpuTemp)}
        />
        <MetricCard
          icon={Thermometer}
          label={tr("telemetry_soc_temp", undefined, "SoC Temp")}
          value={socTemp == null ? "—" : `${socTemp.toFixed(0)}°C`}
          tone={temperatureTone(socTemp)}
          sparkData={series.socTemp}
          sparkColor={temperatureColor(socTemp)}
        />
        <MetricCard
          icon={Fan}
          label={tr("telemetry_fan_duty", undefined, "Fan Duty")}
          value={
            fanDuty != null && fanDuty >= 0 ? `${fanDuty.toFixed(0)}%` : "—"
          }
          tone={fanDuty != null && fanDuty >= 80 ? "warn" : undefined}
          sparkData={series.fanDuty}
          sparkColor={
            fanDuty != null && fanDuty >= 80
              ? "var(--color-warn)"
              : "var(--color-text)"
          }
          minY={0}
          maxY={100}
        />
        <MetricCard
          icon={Zap}
          label={tr("telemetry_power", undefined, "Power")}
          value={
            powerW == null
              ? "—"
              : `${powerW.toFixed(1)} ${tr("telemetry_watts", undefined, "W")}`
          }
          sparkData={series.power}
        />
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <h2 className="mb-3 flex items-center gap-2 text-sm font-semibold">
            <Clock size={16} className="text-[var(--color-muted)]" />
            {tr("telemetry_lifetime", undefined, "Lifetime")}
          </h2>
          <dl className="space-y-2 text-sm">
            <MetricRow
              label={tr(
                "telemetry_operating_hours",
                undefined,
                "Operating time",
              )}
              value={power ? formatDuration(power.operating_time_sec) : "—"}
            />
            <MetricRow
              label={tr("telemetry_boot_count", undefined, "Boot count")}
              value={power ? String(power.boot_count) : "—"}
            />
            <MetricRow
              label={tr("telemetry_load_avg", undefined, "Load avg (1m)")}
              value={
                power && power.load_avg_1m >= 0
                  ? power.load_avg_1m.toFixed(2)
                  : "—"
              }
            />
          </dl>
        </Card>

        <Card>
          <h2 className="mb-3 flex items-center gap-2 text-sm font-semibold">
            <Activity size={16} className="text-[var(--color-muted)]" />
            {tr("telemetry_overview", undefined, "Overview")}
          </h2>
          <dl className="space-y-2 text-sm">
            <MetricRow
              label={tr("telemetry_cpu_usage", undefined, "CPU usage")}
              value={
                temps && temps.cpu_usage_pct >= 0
                  ? `${temps.cpu_usage_pct.toFixed(0)}%`
                  : "—"
              }
            />
            <MetricRow
              label={tr("telemetry_cpu_freq", undefined, "CPU freq")}
              value={
                temps && temps.cpu_freq_mhz > 0
                  ? `${(temps.cpu_freq_mhz / 1000).toFixed(2)} ${tr(
                      "telemetry_ghz",
                      undefined,
                      "GHz",
                    )}`
                  : "—"
              }
            />
            <MetricRow
              label={tr("telemetry_soc_power", undefined, "SoC power")}
              value={
                temps && temps.soc_power_mw > 0
                  ? `${(temps.soc_power_mw / 1000).toFixed(2)} W`
                  : "—"
              }
            />
            {temps && temps.m2_temp > 0 && (
              <MetricRow
                label={tr("telemetry_m2_temp", undefined, "M.2 SSD")}
                value={`${temps.m2_temp.toFixed(0)}°C`}
              />
            )}
          </dl>
        </Card>
      </div>

      <div className="flex items-center justify-end gap-2 text-xs text-[var(--color-muted)]">
        <Badge tone="neutral" variant="soft">
          {history.length} {tr("telemetry_samples", undefined, "samples")}
        </Badge>
        <span>
          {tr("telemetry_window", undefined, "Last ~")}~
          {Math.round((history.length * 5) / 60)}{" "}
          {tr("telemetry_minutes", undefined, "min")}
        </span>
      </div>
    </div>
  );
}

function MetricRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-3">
      <dt className="text-[var(--color-muted)]">{label}</dt>
      <dd className="text-right tabular-nums">{value}</dd>
    </div>
  );
}

function MetricCard({
  icon: Icon,
  label,
  value,
  tone,
  sparkData,
  sparkColor,
  minY,
  maxY,
}: {
  icon: typeof Activity;
  label: string;
  value: string;
  tone?: "warn" | "bad";
  sparkData?: number[];
  sparkColor?: string;
  minY?: number;
  maxY?: number;
}) {
  return (
    <Card>
      <div className="flex items-start justify-between">
        <div>
          <div className="flex items-center gap-1.5 text-xs text-[var(--color-muted)]">
            <Icon size={14} />
            {label}
          </div>
          <div
            className={`mt-1 text-2xl font-bold tabular-nums ${
              tone === "warn"
                ? "text-[var(--color-warn)]"
                : tone === "bad"
                  ? "text-[var(--color-bad)]"
                  : ""
            }`}
          >
            {value}
          </div>
        </div>
        {sparkData && sparkData.length >= 2 && (
          <Sparkline
            data={sparkData}
            width={90}
            height={36}
            color={sparkColor ?? "var(--color-text)"}
            fill
            minY={minY}
            maxY={maxY}
          />
        )}
      </div>
    </Card>
  );
}
