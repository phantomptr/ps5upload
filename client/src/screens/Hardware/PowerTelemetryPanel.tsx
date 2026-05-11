import { useEffect, useState } from "react";
import { Battery, RefreshCw, Loader2 } from "lucide-react";
import { powerTelemetryGet, type PowerTelemetry } from "../../api/ps5";
import { Button } from "../../components";
import { useTr } from "../../state/lang";

/**
 * Lifetime ICC telemetry — operating seconds, boot cycles, thermal
 * alert flags, power-up cause. Static-ish (changes slowly), so we
 * fetch once on mount + offer a manual refresh. Different cadence
 * from the live sensor panel above.
 */
export default function PowerTelemetryPanel({ mgmtAddr }: { mgmtAddr: string }) {
  const tr = useTr();
  const [data, setData] = useState<PowerTelemetry | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    if (!mgmtAddr) return;
    setLoading(true);
    setError(null);
    try {
      const t = await powerTelemetryGet(mgmtAddr);
      setData(t);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mgmtAddr]);

  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center gap-2">
        <Battery size={14} />
        <h3 className="flex-1 text-sm font-semibold">
          {tr("power_telemetry_title", undefined, "Console health")}
        </h3>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={
            loading ? (
              <Loader2 size={11} className="animate-spin" />
            ) : (
              <RefreshCw size={11} />
            )
          }
          onClick={refresh}
          disabled={loading}
        >
          {tr("power_telemetry_refresh", undefined, "Refresh")}
        </Button>
      </header>
      {error && (
        <div className="rounded-md border border-[var(--color-bad)] p-2 text-[11px] text-[var(--color-bad)]">
          {error}
        </div>
      )}
      {data && (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
          <dt className="text-[var(--color-muted)]">
            {tr("power_telemetry_uptime", undefined, "Lifetime power-on")}
          </dt>
          <dd>{formatSeconds(data.operating_seconds)}</dd>
          <dt className="text-[var(--color-muted)]">
            {tr("power_telemetry_cycles", undefined, "Boot cycles")}
          </dt>
          <dd>
            {data.boot_cycles !== null
              ? data.boot_cycles.toLocaleString()
              : "—"}
          </dd>
          <dt className="text-[var(--color-muted)]">
            {tr("power_telemetry_thermal", undefined, "Thermal alerts")}
          </dt>
          <dd>{formatThermal(data.thermal_alert_flags)}</dd>
          <dt className="text-[var(--color-muted)]">
            {tr("power_telemetry_powerup", undefined, "Last power-up cause")}
          </dt>
          <dd>{data.power_up_cause !== null ? `code ${data.power_up_cause}` : "—"}</dd>
        </dl>
      )}
      <p className="mt-3 text-[10px] text-[var(--color-muted)]">
        {tr(
          "power_telemetry_explainer",
          undefined,
          "Read from the Integrated Circuit Controller (ICC). High thermal-alert counts on a relatively new console can signal a failing fan or thermal paste; high boot-cycle counts may indicate frequent power loss.",
        )}
      </p>
    </section>
  );
}

function formatSeconds(s: number | null): string {
  if (s === null) return "—";
  const hours = Math.floor(s / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  if (hours > 24) {
    const days = Math.floor(hours / 24);
    return `${days}d ${hours % 24}h (${hours.toLocaleString()} hours)`;
  }
  return `${hours}h ${minutes}m`;
}

function formatThermal(flags: number | null): string {
  if (flags === null) return "—";
  if (flags === 0) return "no alerts";
  return `0x${flags.toString(16)} (${flags} bit${flags === 1 ? "" : "s"} set)`;
}
