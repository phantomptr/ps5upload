import { useCallback, useEffect, useRef, useState } from "react";
import {
  Cpu,
  Thermometer,
  Activity,
  Clock,
  RefreshCw,
  Loader2,
  Zap,
  Fan,
  Check,
} from "lucide-react";

import { AlertTriangle } from "lucide-react";
import { PageHeader, EmptyState, ErrorCard, Button } from "../../components";

import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import {
  fetchHwInfo,
  fetchHwPower,
  fetchHwTemps,
  setFanThreshold,
  FAN_THRESHOLD_MIN_C,
  FAN_THRESHOLD_MAX_C,
  type HwInfo,
  type HwPower,
  type HwTemps,
} from "../../api/ps5";

/** Hardware Monitor tab — live sensor + uptime view.
 *  The payload side is in payload/src/hw_info.c with
 *  1s sensor cache and 5s power API throttle -- the UI polls every 5s
 *  to keep the refresh rate well inside both caps.
 *
 *  Auto-polling stops when:
 *    - the user leaves this tab (component unmount clears the interval)
 *    - the payload goes down (the connection store's payloadStatus
 *      flips to "down"; our refresh() early-returns on that)
 *
 *  Static info (model, serial, OS, RAM) is fetched once per mount;
 *  live temps and uptime are polled every 5s. */
const POLL_INTERVAL_MS = 5_000;

function formatBytes(n: number): string {
  if (n <= 0) return "—";
  const gib = n / (1024 ** 3);
  if (gib >= 1) return `${gib.toFixed(2)} GiB`;
  const mib = n / (1024 ** 2);
  return `${mib.toFixed(0)} MiB`;
}

function formatUptime(sec: number): string {
  if (sec <= 0) return "—";
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function formatTemp(c: number): string {
  if (c <= 0) return "—";
  return `${c}°C`;
}

function formatFreq(mhz: number): string {
  if (mhz <= 0) return "—";
  if (mhz >= 1000) return `${(mhz / 1000).toFixed(2)} GHz`;
  return `${mhz} MHz`;
}

function formatPower(mw: number): string {
  if (mw <= 0) return "—";
  return `${(mw / 1000).toFixed(1)} W`;
}

export default function HardwareScreen() {
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);

  const [info, setInfo] = useState<HwInfo | null>(null);
  const [temps, setTemps] = useState<HwTemps | null>(null);
  const [power, setPower] = useState<HwPower | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Guard flag: if a previous refresh is still pending, skip new
  // ticks. Prevents overlapping requests if the PS5 is slow to
  // respond.
  const busy = useRef(false);

  const refresh = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    if (busy.current) return;
    busy.current = true;
    setLoading(true);
    setError(null);
    try {
      const addr = `${host}:${PS5_PAYLOAD_PORT}`;
      const [nextTemps, nextPower] = await Promise.all([
        fetchHwTemps(addr),
        fetchHwPower(addr),
      ]);
      setTemps(nextTemps);
      setPower(nextPower);
      // info is static; fetch once if we don't have it yet.
      if (info === null) {
        const nextInfo = await fetchHwInfo(addr).catch(() => null);
        if (nextInfo) setInfo(nextInfo);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      busy.current = false;
      setLoading(false);
    }
  }, [host, payloadStatus, info]);

  // Mount + auto-poll every POLL_INTERVAL_MS while payload is up.
  // Timer is torn down on unmount, disconnect, or tab switch. When the
  // payload flips to down, also clear any stale error + sensor data so
  // the UI goes back to a clean "not connected" state instead of
  // showing last-known readings that are no longer true.
  useEffect(() => {
    if (payloadStatus !== "up") {
      setError(null);
      return;
    }
    refresh();
    const id = window.setInterval(refresh, POLL_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, [payloadStatus, refresh]);

  return (
    <div className="p-6">
      <PageHeader
        icon={Cpu}
        title="Hardware"
        loading={loading}
        description="Live system info, temperatures, and uptime for the PS5. Auto-refreshes every 5 seconds while the payload is connected."
        right={
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={refresh}
            disabled={loading || !host?.trim() || payloadStatus !== "up"}
          >
            Refresh
          </Button>
        }
      />

      {payloadStatus !== "up" && (
        <EmptyState
          icon={Cpu}
          size="hero"
          title="Payload not connected"
          message="Head to Connection and Send payload first — hardware info becomes available once the payload is running."
        />
      )}

      {/* Only surface the error card when the payload IS up — otherwise
          the empty state above already explains the state. */}
      {error && payloadStatus === "up" && (
        <div className="mb-4">
          <ErrorCard title="Couldn't read hardware info" detail={error} />
        </div>
      )}

      {payloadStatus === "up" && (
        <div className="grid gap-4 lg:grid-cols-2 xl:grid-cols-3">
          <SensorCard
            icon={<Thermometer size={14} />}
            title="Temperatures"
          >
            <StatRow
              label="CPU"
              value={formatTemp(temps?.cpu_temp ?? 0)}
              hint={temps && temps.cpu_temp === 0 ? "Sensor API disabled on this firmware" : undefined}
            />
            <StatRow
              label="SoC"
              value={formatTemp(temps?.soc_temp ?? 0)}
              hint={temps && temps.soc_temp === 0 ? "Sensor API disabled on this firmware" : undefined}
            />
          </SensorCard>

          <SensorCard
            icon={<Activity size={14} />}
            title="Performance"
          >
            <StatRow
              label="CPU frequency"
              value={formatFreq(temps?.cpu_freq_mhz ?? 0)}
              hint={temps && temps.cpu_freq_mhz > 0 ? "From kernel TSC" : undefined}
            />
            <StatRow
              label="SoC power draw"
              value={formatPower(temps?.soc_power_mw ?? 0)}
              hint={temps && temps.soc_power_mw === 0 ? "Sensor API disabled on this firmware" : "Sampled at most every 5s"}
            />
          </SensorCard>

          <SensorCard
            icon={<Clock size={14} />}
            title="Uptime"
          >
            <StatRow
              label="Running since boot"
              value={formatUptime(power?.operating_time_sec ?? 0)}
            />
          </SensorCard>

          <SensorCard
            icon={<Cpu size={14} />}
            title="System"
          >
            <StatRow label="Model" value={info?.model ?? "—"} />
            <StatRow label="Serial" value={info?.serial ?? "—"} />
            <StatRow label="OS" value={info?.os ?? "—"} />
            <StatRow
              label="RAM"
              value={info ? formatBytes(info.physmem) : "—"}
            />
            <StatRow
              label="CPU cores"
              value={info?.ncpu ? String(info.ncpu) : "—"}
            />
          </SensorCard>

          <FanThresholdCard
            host={host ?? ""}
            payloadUp={payloadStatus === "up"}
          />

          <div className="mt-2 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-xs text-[var(--color-muted)]">
            <Zap size={12} className="mt-0.5" />
            <div>
              CPU/SoC temperature readings aren't available from this
              payload. The <code>sceKernelGet*</code> sensor APIs only
              behave safely when called from inside Sony's own ShellUI
              process — a standalone userland payload like ours hits a
              different code path in the kernel stubs and the handler
              thread wedges. This is independent of PS5 firmware
              version (observed the same on 9.x and 11.x; likely the
              same on 12.x) and not fixable from our side without
              turning ps5upload into a ShellUI hook, which is out of
              scope. Uptime comes from <code>kern.boottime</code>, CPU
              frequency from <code>machdep.tsc_freq</code>, and static
              info (model / serial / RAM / CPU count) from the kernel
              APIs that DO work from any process.
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function SensorCard({
  icon,
  title,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-2 flex items-center gap-2 text-xs uppercase tracking-wide text-[var(--color-muted)]">
        {icon}
        <span className="font-semibold">{title}</span>
      </header>
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        {children}
      </dl>
    </section>
  );
}

function StatRow({
  label,
  value,
  hint,
}: {
  label: string;
  value: string;
  hint?: string;
}) {
  return (
    <>
      <dt className="text-[var(--color-muted)]">{label}</dt>
      <dd className="font-mono tabular-nums" title={hint}>
        {value}
        {hint && (
          <span className="ml-2 text-xs text-[var(--color-muted)] font-sans">
            · {hint}
          </span>
        )}
      </dd>
    </>
  );
}

/* Presets are named for what the user is optimizing for, not raw
 * numbers. 55 °C = fan always running quietly but never at turbo;
 * 65 °C ~ Sony's default behavior; 75 °C = only turbo under real
 * load. Values must be inside [FAN_THRESHOLD_MIN_C, _MAX_C]. */
const FAN_PRESETS: ReadonlyArray<{ label: string; c: number; hint: string }> = [
  { label: "Quiet", c: 55, hint: "Fan engages earlier — cooler, louder" },
  { label: "Balanced", c: 65, hint: "Close to Sony's default" },
  { label: "Performance", c: 75, hint: "Fan ramps only under load — quieter idle" },
];

function FanThresholdCard({
  host,
  payloadUp,
}: {
  host: string;
  payloadUp: boolean;
}) {
  /* No read-back is possible (see FTX2 protocol note on
   * HwSetFanThreshold). This component only reflects values the
   * user has set via this UI during the current session — after a
   * reboot or app restart, we don't know what's actually in effect. */
  const [draftC, setDraftC] = useState<number>(65);
  const [lastSetC, setLastSetC] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSet = payloadUp && !!host.trim() && !busy;

  const applyThreshold = useCallback(
    async (targetC: number) => {
      if (!canSet) return;
      const clamped = Math.max(
        FAN_THRESHOLD_MIN_C,
        Math.min(FAN_THRESHOLD_MAX_C, Math.round(targetC)),
      );
      setBusy(true);
      setError(null);
      try {
        const addr = `${host}:${PS5_PAYLOAD_PORT}`;
        await setFanThreshold(addr, clamped);
        setLastSetC(clamped);
        setDraftC(clamped);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
      }
    },
    [canSet, host],
  );

  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center gap-2 text-xs uppercase tracking-wide text-[var(--color-muted)]">
        <Fan size={14} />
        <span className="font-semibold">Fan threshold</span>
        {busy && (
          <Loader2 size={12} className="animate-spin text-[var(--color-accent)]" />
        )}
      </header>

      <div className="mb-3 grid grid-cols-3 gap-2">
        {FAN_PRESETS.map((p) => {
          const active = lastSetC === p.c;
          return (
            <button
              key={p.label}
              type="button"
              onClick={() => applyThreshold(p.c)}
              disabled={!canSet}
              title={p.hint}
              className={`flex flex-col items-center gap-0.5 rounded-md border px-2 py-2 text-xs transition ${
                active
                  ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)] text-[var(--color-accent)]"
                  : "border-[var(--color-border)] hover:bg-[var(--color-surface-3)]"
              } disabled:opacity-50`}
            >
              <span className="flex items-center gap-1 font-medium">
                {active && <Check size={10} />}
                {p.label}
              </span>
              <span className="font-mono tabular-nums">{p.c}°C</span>
            </button>
          );
        })}
      </div>

      <label className="mb-1 flex items-center justify-between text-xs text-[var(--color-muted)]">
        <span>Custom</span>
        <span className="font-mono tabular-nums">
          {draftC}°C
        </span>
      </label>
      <div className="flex items-center gap-2">
        <input
          type="range"
          min={FAN_THRESHOLD_MIN_C}
          max={FAN_THRESHOLD_MAX_C}
          step={1}
          value={draftC}
          disabled={!canSet}
          onChange={(e) => setDraftC(Number(e.target.value))}
          className="flex-1 accent-[var(--color-accent)] disabled:opacity-50"
        />
        <button
          type="button"
          onClick={() => applyThreshold(draftC)}
          disabled={!canSet || lastSetC === draftC}
          className="rounded-md border border-[var(--color-border)] px-3 py-1 text-xs hover:bg-[var(--color-surface-3)] disabled:opacity-50"
        >
          Apply
        </button>
      </div>

      {error && (
        <div className="mt-3 flex items-start gap-2 text-xs text-[var(--color-bad)]">
          <AlertTriangle size={12} className="mt-0.5" />
          <span>{error}</span>
        </div>
      )}

      <p className="mt-3 text-xs text-[var(--color-muted)]">
        {lastSetC !== null ? (
          <>Set to <span className="font-mono">{lastSetC}°C</span>. </>
        ) : null}
        Persists until PS5 reboot. Fan RPM can't be read back — only
        the threshold is writable.
      </p>
    </section>
  );
}
