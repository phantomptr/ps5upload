import { useEffect, useRef, useState } from "react";
import { Battery, RefreshCw } from "lucide-react";
import { powerTelemetryGet, type PowerTelemetry } from "../../api/ps5";
import { Button, ErrorCard, Spinner } from "../../components";
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

  // Token-guard so a slow in-flight fetch that completes after the
  // addr changes (console switch) or the panel unmounts cannot write
  // stale data over fresh data, or setState on an unmounted component.
  const reqIdRef = useRef(0);

  useEffect(() => {
    return () => {
      // eslint-disable-next-line react-hooks/exhaustive-deps
      reqIdRef.current++;
    };
  }, []);

  async function refresh() {
    if (!mgmtAddr) return;
    const myId = ++reqIdRef.current;
    setLoading(true);
    setError(null);
    try {
      const t = await powerTelemetryGet(mgmtAddr);
      if (myId !== reqIdRef.current) return;
      setData(t);
    } catch (e) {
      if (myId !== reqIdRef.current) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (myId === reqIdRef.current) setLoading(false);
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
              <Spinner size={12} tone="inherit" />
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
      {error && <ErrorCard title={error} />}
      {data && (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
          <dt className="text-[var(--color-muted)]">
            {tr("power_telemetry_uptime", undefined, "Lifetime power-on")}
          </dt>
          <dd>{formatSeconds(data.operating_seconds)}</dd>
          <dt className="text-[var(--color-muted)]">
            {tr("power_telemetry_cycles", undefined, "Boot cycles")}
          </dt>
          <dd>{formatBootCycles(data.boot_cycles)}</dd>
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
      {/* Say WHY the readout is empty. Four em-dashes under a paragraph about
          failing fans reads as a broken feature; on most retail firmware the
          console simply does not expose these counters. The payload reports
          which of the four ICC symbols resolved, so use it rather than
          guessing from the nulls. */}
      {data && unavailableNote(data) && (
        <p className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-muted)]">
          {tr(
            `power_telemetry_note_${data.status}`,
            undefined,
            unavailableNote(data) as string,
          )}
        </p>
      )}
      <p className="mt-3 text-xs text-[var(--color-muted)]">
        {tr(
          "power_telemetry_explainer",
          undefined,
          "Read from the Integrated Circuit Controller (ICC). Where a console reports them, high thermal-alert counts on a relatively new console can signal a failing fan or thermal paste, and high boot-cycle counts may indicate frequent power loss.",
        )}
      </p>
    </section>
  );
}

/** A boot count the console cannot plausibly have reached.
 *
 *  FW 5.10 returns 0x01010000 (16,842,752) here — roughly 500 boots a day for
 *  a century. That is a misread field, not a reading, and showing it as fact
 *  is worse than showing nothing: it is the one number on this panel a user
 *  might act on. Anything above the cap renders as unavailable. */
const BOOT_CYCLES_MAX = 1_000_000;

export function formatBootCycles(n: number | null | undefined): string {
  if (n === null || n === undefined) return "—";
  if (n < 0 || n > BOOT_CYCLES_MAX) return "—";
  return n.toLocaleString();
}

/** One sentence explaining an empty readout, or null when there is nothing to
 *  explain (everything read, or the payload is too old to tell us). */
export function unavailableNote(data: {
  status?: string | null;
}): string | null {
  switch (data.status) {
    case "unsupported_firmware":
      return "This console's firmware doesn't expose the ICC health counters, so there is nothing to read. Nothing is wrong with the console or with ps5upload.";
    case "calls_failed":
      return "The console exposes these counters but refused to read them. This usually clears after a reboot.";
    case "partial":
      return "This console reports only some of these counters. The blank rows aren't errors — its firmware doesn't provide them.";
    default:
      // "ok", or an older payload that cannot tell us. Saying nothing beats
      // inventing a reason.
      return null;
  }
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
  // Count the SET bits, not the raw flag value — `${flags} bits set` labelled
  // e.g. flag 0x4 as "4 bits set" when only one bit is set.
  let n = flags;
  let count = 0;
  while (n) {
    count += n & 1;
    n >>>= 1;
  }
  return `0x${flags.toString(16)} (${count} bit${count === 1 ? "" : "s"} set)`;
}
