import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Cpu,
  Thermometer,
  Activity,
  Clock,
  RefreshCw,
  Loader2,
  Fan,
  Check,
  HardDrive,
  CalendarClock,
} from "lucide-react";

import { AlertTriangle } from "lucide-react";
import { PageHeader, EmptyState, ErrorCard, Button } from "../../components";
import { useTr } from "../../state/lang";
import PowerTelemetryPanel from "./PowerTelemetryPanel";
import NetworkPanel from "./NetworkPanel";
import PeripheralPanel from "./PeripheralPanel";
import SpeedTestPanel from "./SpeedTestPanel";
import { useDocumentVisible } from "../../lib/visibility";
import { transferAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";

import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import {
  psTimeToDate,
  formatUtcCompact,
  formatDrift,
  type PsTimeJson,
} from "../../lib/sysTimeFormat";
import {
  fetchHwInfo,
  fetchHwPower,
  fetchHwStorage,
  fetchHwTemps,
  setFanThreshold,
  FAN_THRESHOLD_MIN_C,
  FAN_THRESHOLD_MAX_C,
  type HwInfo,
  type HwPower,
  type HwStorage,
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

/** Friendlier "Console Storage" formatter that mirrors the GB units PS5
 *  Settings shows. Uses GB (decimal 10^9) deliberately — Sony's UI uses
 *  GB even though the underlying counts are GiB-sized; keep parity so
 *  users can compare to what their PS5 reports. */
function formatStorageGB(n: number): string {
  if (n <= 0) return "—";
  const gb = n / 1_000_000_000;
  if (gb >= 1000) return `${(gb / 1000).toFixed(2)} TB`;
  return `${gb.toFixed(0)} GB`;
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
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const guard = useStaleHostGuard();

  const [info, setInfo] = useState<HwInfo | null>(null);
  const [temps, setTemps] = useState<HwTemps | null>(null);
  const [power, setPower] = useState<HwPower | null>(null);
  const [storage, setStorage] = useState<HwStorage | null>(null);
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
    // Host-stale guard (2.9.0). `busy.current` prevents overlapping
    // polls AGAINST THE SAME HOST, but doesn't catch host-switch
    // mid-poll. Without this guard, the first tick after a switch
    // shows the OLD console's temps/power/storage/info under the
    // NEW host's identity (model/serial in the System card is
    // especially misleading). Auto-poll resolves it in ≤5s but the
    // first-after-switch render lies.
    // 2.12.0: migrated from local probedHost+isStale to canonical
    // useStaleHostGuard.
    const probe = guard.capture();
    setLoading(true);
    setError(null);
    try {
      const addr = transferAddr(probe.host);
      const [nextTemps, nextPower, nextStorage] = await Promise.all([
        fetchHwTemps(addr),
        fetchHwPower(addr),
        // Storage is essentially static at the per-second polling rate
        // (a 100 GB game install changes free-space slowly), but
        // refreshing it alongside temps keeps the card live without
        // a separate slow timer. Pre-2.2.26 payloads fail this with
        // unsupported_frame; treat that as "no data" and let the rest
        // of the tab keep working.
        fetchHwStorage(addr).catch(() => null),
      ]);
      if (probe.isStale()) return;
      setTemps(nextTemps);
      setPower(nextPower);
      setStorage(nextStorage);
      // info is static; fetch once if we don't have it yet.
      if (info === null) {
        const nextInfo = await fetchHwInfo(addr).catch(() => null);
        if (probe.isStale()) return;
        if (nextInfo) setInfo(nextInfo);
      }
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      busy.current = false;
      setLoading(false);
    }
  }, [host, payloadStatus, info, guard]);

  // Mount + auto-poll every POLL_INTERVAL_MS while payload is up AND
  // the window is visible. Pausing on minimize keeps idle laptops
  // from spamming the PS5's mgmt port (sensor reads are several
  // RPCs per tick). Resumes on visibility-change with a fresh
  // immediate refresh so the panel is up-to-date when the user
  // looks at it again.
  const visible = useDocumentVisible();
  useEffect(() => {
    if (payloadStatus !== "up") {
      setError(null);
      return;
    }
    if (!visible) return;
    refresh();
    const id = window.setInterval(refresh, POLL_INTERVAL_MS);
    return () => window.clearInterval(id);
  }, [payloadStatus, refresh, visible]);

  return (
    <div className="p-6">
      <PageHeader
        icon={Cpu}
        title={tr("hardware_title", undefined, "Hardware")}
        loading={loading}
        description={tr(
          "hardware_description",
          undefined,
          "Live system info, temperatures, and uptime for the PS5. Auto-refreshes every 5 seconds while the payload is connected.",
        )}
        right={
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={refresh}
            disabled={loading || !host?.trim() || payloadStatus !== "up"}
          >
            {tr("refresh", undefined, "Refresh")}
          </Button>
        }
      />

      {payloadStatus !== "up" && (
        <EmptyState
          icon={Cpu}
          size="hero"
          title={tr("payload_not_connected", undefined, "Payload not connected")}
          message={tr(
            "payload_not_connected_message",
            undefined,
            "Head to Connection and Send payload first — hardware info becomes available once the payload is running.",
          )}
        />
      )}

      {/* Only surface the error card when the payload IS up — otherwise
          the empty state above already explains the state. */}
      {error && payloadStatus === "up" && (
        <div className="mb-4">
          <ErrorCard
            title={tr(
              "hardware_read_error",
              undefined,
              "Couldn't read hardware info",
            )}
            detail={error}
          />
        </div>
      )}

      {payloadStatus === "up" && (
        <div className="grid gap-4 lg:grid-cols-2 xl:grid-cols-3">
          <SensorCard
            icon={<Thermometer size={14} />}
            title={tr("hardware_temperatures", undefined, "Temperatures")}
          >
            <StatRow
              label={tr("hardware_label_cpu", "CPU")}
              value={formatTemp(temps?.cpu_temp ?? 0)}
              hint={
                temps && temps.cpu_temp === 0
                  ? tr(
                      "hw_sensor_unavailable",
                      undefined,
                      "Sensor reading unavailable right now",
                    )
                  : undefined
              }
            />
            <StatRow
              label={tr("hardware_label_soc", "SoC")}
              value={formatTemp(temps?.soc_temp ?? 0)}
              hint={
                temps && temps.soc_temp === 0
                  ? tr(
                      "hw_sensor_unavailable",
                      undefined,
                      "Sensor reading unavailable right now",
                    )
                  : undefined
              }
            />
          </SensorCard>

          <SensorCard
            icon={<Activity size={14} />}
            title={tr("hardware_performance", undefined, "Performance")}
          >
            <StatRow
              label={tr("hw_cpu_freq", undefined, "CPU frequency")}
              value={formatFreq(temps?.cpu_freq_mhz ?? 0)}
              hint={
                temps && temps.cpu_freq_mhz > 0
                  ? tr(
                      "hw_from_kernel_tsc",
                      undefined,
                      "From kernel TSC",
                    )
                  : undefined
              }
            />
            <StatRow
              label={tr("hw_soc_power", undefined, "SoC power draw")}
              value={formatPower(temps?.soc_power_mw ?? 0)}
              hint={
                temps && temps.soc_power_mw === 0
                  ? tr(
                      "hw_sensor_unavailable",
                      undefined,
                      "Sensor reading unavailable right now",
                    )
                  : tr(
                      "hw_sampled_5s",
                      undefined,
                      "Sampled at most every 5s",
                    )
              }
            />
          </SensorCard>

          <SensorCard
            icon={<Clock size={14} />}
            title={tr("hardware_uptime", undefined, "Uptime")}
          >
            <StatRow
              label={tr("hw_running_since_boot", undefined, "Running since boot")}
              value={formatUptime(power?.operating_time_sec ?? 0)}
            />
          </SensorCard>

          <SensorCard
            icon={<Cpu size={14} />}
            title={tr("hardware_system", undefined, "System")}
          >
            <StatRow label={tr("hw_model", undefined, "Model")} value={info?.model ?? "—"} />
            <StatRow label={tr("hw_serial", undefined, "Serial")} value={info?.serial ?? "—"} />
            <StatRow label={tr("hw_os", undefined, "OS")} value={info?.os ?? "—"} />
            <StatRow
              label={tr("hw_ram", undefined, "RAM")}
              value={info ? formatBytes(info.physmem) : "—"}
            />
            <StatRow
              label={tr("hw_cpu_cores", undefined, "CPU cores")}
              value={info?.ncpu ? String(info.ncpu) : "—"}
            />
          </SensorCard>

          {storage && (
            <SensorCard
              icon={<HardDrive size={14} />}
              title={tr("hardware_storage", undefined, "Console Storage")}
            >
              <StatRow
                label={tr("hw_storage_total", undefined, "Total")}
                value={formatStorageGB(storage.total_bytes)}
              />
              <StatRow
                label={tr("hw_storage_free", undefined, "Free")}
                value={formatStorageGB(storage.free_bytes)}
              />
              <StatRow
                label={tr("hw_storage_used", undefined, "Used")}
                value={formatStorageGB(storage.used_bytes)}
              />
              {storage.reserved_bytes > 0 && (
                <StatRow
                  label={tr("hw_storage_reserved", undefined, "Reserved")}
                  value={formatStorageGB(storage.reserved_bytes)}
                  hint={tr(
                    "hw_storage_reserved_hint",
                    undefined,
                    "FS-kept slice not counted as free",
                  )}
                />
              )}
            </SensorCard>
          )}

          <FanThresholdCard
            host={host ?? ""}
            payloadUp={payloadStatus === "up"}
          />

          <SystemTimeCard
            host={host ?? ""}
            payloadUp={payloadStatus === "up"}
          />

          <DateTimeStateCard
            host={host ?? ""}
            payloadUp={payloadStatus === "up"}
          />

          {/* Lifetime ICC telemetry — fetched on mount + on demand,
              not on the live-poll interval. Different cadence from
              the sensor cards above because these values barely
              move (boot count increments once per boot, operating
              seconds tracks total uptime). */}
          {host?.trim() && payloadStatus === "up" && (
            <>
              <PowerTelemetryPanel mgmtAddr={`${host.trim()}:9114`} />
              <NetworkPanel mgmtAddr={`${host.trim()}:9114`} />
              <SpeedTestPanel mgmtAddr={`${host.trim()}:9114`} />
              <PeripheralPanel mgmtAddr={`${host.trim()}:9114`} />
            </>
          )}
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
  const tr = useTr();
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
        <span className="font-semibold">
          {tr("hardware_fan_threshold", "Fan threshold")}
        </span>
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

      <FanCurvePreview thresholdC={draftC} />

      <label className="mb-1 flex items-center justify-between text-xs text-[var(--color-muted)]">
        <span>{tr("hardware_fan_custom", "Custom")}</span>
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
          {tr("hardware_apply", "Apply")}
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
          <>
            {tr("hardware_fan_set_to", "Set to")}{" "}
            <span className="font-mono">{lastSetC}°C</span>.{" "}
          </>
        ) : null}
        {tr(
          "hardware_fan_persist_note",
          "Persists until PS5 reboot. Fan RPM can't be read back — only the threshold is writable.",
        )}
      </p>
    </section>
  );
}

/**
 * SVG visualisation of the fan ramp implied by the current threshold.
 *
 * The PS5's fan firmware doesn't expose a configurable curve — only
 * the turbo-engage threshold. This preview shows the *implied* shape:
 *   - Below threshold: gentle linear ramp from 25% to 70%
 *   - At threshold: jump to 95% (turbo)
 *   - Above threshold: held at 95–100%
 *
 * The exact curve Sony uses internally isn't documented; this is a
 * pedagogical approximation so users understand "lower threshold =
 * fan spins up more eagerly under temp." Honest about the limitation
 * via the caption.
 */
function FanCurvePreview({ thresholdC }: { thresholdC: number }) {
  const tr = useTr();
  const W = 280;
  const H = 80;
  const PADDING = 8;
  const innerW = W - PADDING * 2;
  const innerH = H - PADDING * 2;
  // X axis: 30°C → 90°C. Y axis: 0% → 100% fan duty.
  const tempMin = 30;
  const tempMax = 90;
  const xFor = (t: number) =>
    PADDING + ((t - tempMin) / (tempMax - tempMin)) * innerW;
  const yFor = (pct: number) => PADDING + (1 - pct / 100) * innerH;
  // Build polyline points for the implied curve.
  const t1 = thresholdC - 1;
  const t2 = thresholdC + 1;
  const points: Array<[number, number]> = [
    [tempMin, 25],
    [t1, 70],
    [t2, 95],
    [tempMax, 100],
  ];
  const polyPath = points
    .map(([t, p]) => `${xFor(t).toFixed(1)},${yFor(p).toFixed(1)}`)
    .join(" ");
  return (
    <div className="mb-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
      <div className="mb-1 flex items-center justify-between text-[10px] text-[var(--color-muted)]">
        <span>
          {tr("hardware_fan_curve_preview", "Fan curve preview (approximate)")}
        </span>
        <span className="font-mono tabular-nums text-[var(--color-accent)]">
          {tr("hardware_fan_turbo_at", "turbo @")} {thresholdC}°C
        </span>
      </div>
      <svg width={W} height={H} className="block">
        {/* Y-axis ticks at 0/50/100 */}
        {[0, 50, 100].map((p) => (
          <line
            key={p}
            x1={PADDING}
            y1={yFor(p)}
            x2={W - PADDING}
            y2={yFor(p)}
            stroke="var(--color-border)"
            strokeDasharray="2 3"
          />
        ))}
        {/* Curve */}
        <polyline
          points={polyPath}
          fill="none"
          stroke="var(--color-accent)"
          strokeWidth={2}
          strokeLinejoin="round"
        />
        {/* Threshold marker */}
        <line
          x1={xFor(thresholdC)}
          y1={PADDING}
          x2={xFor(thresholdC)}
          y2={H - PADDING}
          stroke="var(--color-good)"
          strokeDasharray="3 3"
        />
        {/* X-axis labels */}
        <text
          x={PADDING}
          y={H - 1}
          fontSize="9"
          fill="var(--color-muted)"
        >
          {tempMin}°C
        </text>
        <text
          x={W - PADDING}
          y={H - 1}
          fontSize="9"
          fill="var(--color-muted)"
          textAnchor="end"
        >
          {tempMax}°C
        </text>
      </svg>
    </div>
  );
}

/* ─── System time card ───────────────────────────────────────────────── */

/** Wire shape returned by ps5_time_sync. `stub_no_op` is the engine-
 *  side heuristic for "payload said ok but the clock didn't move". */
interface PsTimeSyncJson {
  ok: boolean;
  err_code: number;
  reason: string;
  prior_unix: number;
  new_unix: number;
  stub_no_op: boolean;
}

function SystemTimeCard({
  host,
  payloadUp,
}: {
  host: string;
  payloadUp: boolean;
}) {
  const tr = useTr();
  const guardSys = useStaleHostGuard();
  const [ps5Time, setPs5Time] = useState<PsTimeJson | null>(null);
  const [pcNowMs, setPcNowMs] = useState<number>(() => Date.now());
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [lastResult, setLastResult] = useState<PsTimeSyncJson | null>(null);
  const [error, setError] = useState<string | null>(null);

  const canSync = payloadUp && !!host.trim() && !busy;

  /* Fetch PS5 time on mount + after each sync. We re-poll the PS5
   * every 30s (which is fine for the drift indicator — sub-second
   * accuracy is irrelevant here). PC time is rendered from a fresh
   * Date.now() on every second-tick so the drift number stays live. */
  const refreshPs5 = useCallback(async () => {
    if (!payloadUp || !host.trim()) return;
    // Host-stale guard (2.9.0). Drift display is computed from
    // PS5 time minus PC time — attributing host A's clock to host B
    // produces a wildly-off "drift" the user might act on with the
    // Sync button (which uses click-time host, so the sync itself
    // hits the right console, but they'd be syncing based on bad
    // data). Belt-and-suspenders since the 30s poll resolves it,
    // but a 30s window of misleading drift is enough to mislead.
    // 2.12.0: canonical useStaleHostGuard.
    const probe = guardSys.capture();
    try {
      const addr = transferAddr(probe.host);
      const r = (await invoke("ps5_time_get", { addr })) as PsTimeJson;
      if (probe.isStale()) return;
      setPs5Time(r);
      setError(null);
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [host, payloadUp, guardSys]);

  useEffect(() => {
    if (!payloadUp) return;
    refreshPs5();
    const id = window.setInterval(refreshPs5, 30_000);
    return () => window.clearInterval(id);
  }, [payloadUp, refreshPs5]);

  /* Tick PC clock every second so the drift display updates live.
   * Light enough we don't bother gating on document visibility. */
  useEffect(() => {
    const id = window.setInterval(() => setPcNowMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);

  const ps5Date = psTimeToDate(ps5Time);
  const pcDate = new Date(pcNowMs);

  const handleSync = useCallback(async () => {
    if (!canSync) return;
    setBusy(true);
    setError(null);
    setLastResult(null);
    try {
      const targetUnixSeconds = Math.floor(Date.now() / 1000);
      const addr = `${host}:${PS5_PAYLOAD_PORT}`;
      const r = (await invoke("ps5_time_sync", {
        addr,
        targetUnixSeconds,
      })) as PsTimeSyncJson;
      setLastResult(r);
      setConfirming(false);
      /* Re-fetch PS5 time so the card reflects the new clock right
       * away, not 30s later when the next poll lands. */
      await refreshPs5();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [canSync, host, refreshPs5]);

  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center gap-2 text-xs uppercase tracking-wide text-[var(--color-muted)]">
        <CalendarClock size={14} />
        <span>{tr("hardware_systime_title", "System time")}</span>
      </header>

      <div className="space-y-2 text-xs">
        <StatRow
          label={tr("hardware_systime_ps5", "PS5 time")}
          value={formatUtcCompact(ps5Date)}
        />
        <StatRow
          label={tr("hardware_systime_pc", "Your PC")}
          value={formatUtcCompact(pcDate)}
        />
        <StatRow
          label={tr("hardware_systime_drift", "Drift")}
          value={formatDrift(ps5Date, pcNowMs)}
          hint={tr(
            "hardware_systime_drift_hint",
            "PS5 minus PC, in seconds. Positive = PS5 is ahead.",
          )}
        />
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        {!confirming && !busy && (
          <Button
            variant="secondary"
            size="sm"
            disabled={!canSync}
            onClick={() => setConfirming(true)}
          >
            {tr("hardware_systime_sync", "Sync PS5 to PC time")}
          </Button>
        )}
        {confirming && !busy && (
          <>
            <span className="text-[11px] text-[var(--color-muted)]">
              {tr(
                "hardware_systime_confirm",
                "Sets the PS5 system clock. This can affect trophies, save timestamps, and DRM checks.",
              )}
            </span>
            <Button variant="primary" size="sm" onClick={handleSync}>
              {tr("hardware_systime_confirm_yes", "Set clock")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setConfirming(false)}
            >
              {tr("hardware_systime_confirm_no", "Cancel")}
            </Button>
          </>
        )}
        {busy && (
          <span className="inline-flex items-center gap-1 text-[11px] text-[var(--color-muted)]">
            <Loader2 size={12} className="animate-spin" />
            {tr("hardware_systime_syncing", "Syncing…")}
          </span>
        )}
      </div>

      {/* Result line — success / stub-no-op / failure */}
      {lastResult && !busy && (
        <div className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-[11px]">
          {lastResult.stub_no_op ? (
            <div className="text-[var(--color-bad)]">
              {tr(
                "hardware_systime_stub_no_op",
                "PS5 reported success but the clock didn't actually move. Usually means the loader didn't grant the payload kernel R/W — reload via kstuff and try again.",
              )}
            </div>
          ) : lastResult.ok ? (
            <div className="text-[var(--color-good)]">
              {tr("hardware_systime_synced", "Synced.")}
              {lastResult.new_unix > 0 && (
                <>
                  {" "}
                  <span className="text-[var(--color-muted)]">
                    {tr("hardware_systime_new_label", "Now:")}{" "}
                    {formatUtcCompact(new Date(lastResult.new_unix * 1000))}
                  </span>
                </>
              )}
            </div>
          ) : (
            <div className="text-[var(--color-bad)]">
              {tr("hardware_systime_failed", "Couldn't sync time")}
              {lastResult.reason && (
                <div className="mt-1 text-[var(--color-muted)]">
                  {lastResult.reason}
                </div>
              )}
              {lastResult.err_code !== 0 && (
                <code className="mt-1 inline-block font-mono text-[10px] opacity-75">
                  0x{lastResult.err_code.toString(16).padStart(8, "0")}
                </code>
              )}
            </div>
          )}
        </div>
      )}

      {error && (
        <div className="mt-2 text-[11px] text-[var(--color-bad)]">{error}</div>
      )}
    </section>
  );
}

// ── PS5 Date & Time state card (registry-backed, new in 2.10.0) ─────────
//
// Sits beneath the existing SystemTimeCard. Reads the full PS5 Date
// & Time registry surface (timezone, DST, date/time format, NTP
// auto-sync flag, tzdata version, NTP-error counter) plus the
// cached NTP-derived tick for drift comparison. Write side is
// EXPERIMENTAL — we are (per the 2.10.0 research agent) the first
// public PS5 homebrew to write these keys, and per-firmware
// behavior isn't catalogued yet. Every write field carries a small
// "experimental" badge so users know.
//
// The 6 gotchas the research agent surfaced are baked into an
// expandable "Important to know" section at the top:
//   1. Two clocks: wall vs secure RTC (trophies use secure RTC)
//   2. Far-past wall clock breaks PSN sign-in (TLS cert notBefore)
//   3. Far-future wall clock breaks game cert validation
//   4. set_auto=1 silently re-syncs on reboot
//   5. tzdata is bundled in firmware — old DST rules possible
//   6. Don't touch _dbg / _ad NTP slots on retail (we don't expose)

/** Wire shape returned by ps5_time_state_get. Flat — mirrors the
 *  payload's JSON exactly. `*_avail` flags let us grey out fields
 *  the payload couldn't read on this firmware. */
interface PsTimeStateJson {
  ok: boolean;
  truncated?: boolean;
  tz_index: number;
  tz_index_avail: boolean;
  tz_index_err: number;
  date_format: number;
  date_format_avail: boolean;
  date_format_err: number;
  time_format: number;
  time_format_avail: boolean;
  time_format_err: number;
  summer_policy: number;
  summer_policy_avail: boolean;
  summer_policy_err: number;
  set_auto: number;
  set_auto_avail: boolean;
  set_auto_err: number;
  is_summer_time: number;
  is_summer_time_avail: boolean;
  is_summer_time_err: number;
  utc_offset_sec: number;
  utc_offset_sec_avail: boolean;
  utc_offset_sec_err: number;
  tz_offset_min: number;
  tz_offset_min_avail: boolean;
  tz_offset_min_err: number;
  rtc_error_count: number;
  rtc_error_count_avail: boolean;
  rtc_error_count_err: number;
  tzdata: string;
  tzdata_avail: boolean;
  tzdata_err: number;
  ntp_tick_unix: number;
  ntp_tick_avail: boolean;
  ntp_tick_err: number;
  wall_clock_unix: number;
  wall_clock_avail: boolean;
  wall_clock_err: number;
}

interface PsTimeStateSetJson {
  ok: boolean;
  any_attempted: boolean;
  truncated?: boolean;
  tz_index_attempted: boolean;
  tz_index_rc: number;
  tz_index_err: number;
  date_format_attempted: boolean;
  date_format_rc: number;
  date_format_err: number;
  time_format_attempted: boolean;
  time_format_rc: number;
  time_format_err: number;
  summer_policy_attempted: boolean;
  summer_policy_rc: number;
  summer_policy_err: number;
  set_auto_attempted: boolean;
  set_auto_rc: number;
  set_auto_err: number;
}

/** Format the cached NTP-derived tick vs wall clock as a drift
 *  string. Returns null when either side is missing so the caller
 *  can hide the row entirely instead of showing "n/a". */
function formatNtpDrift(state: PsTimeStateJson): string | null {
  if (!state.ntp_tick_avail || !state.wall_clock_avail) return null;
  if (state.ntp_tick_unix < 0 || state.wall_clock_unix < 0) return null;
  const diff = state.wall_clock_unix - state.ntp_tick_unix;
  const abs = Math.abs(diff);
  const sign = diff >= 0 ? "+" : "−";
  if (abs < 60) return `${sign}${abs}s`;
  if (abs < 3600) return `${sign}${Math.floor(abs / 60)}m ${abs % 60}s`;
  return `${sign}${Math.floor(abs / 3600)}h ${Math.floor((abs % 3600) / 60)}m`;
}

function DateTimeStateCard({
  host,
  payloadUp,
}: {
  host: string;
  payloadUp: boolean;
}) {
  const tr = useTr();
  const guardDt = useStaleHostGuard();
  const [state, setState] = useState<PsTimeStateJson | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [writeBusy, setWriteBusy] = useState(false);
  const [writeResult, setWriteResult] = useState<PsTimeStateSetJson | null>(
    null,
  );
  const [showGotchas, setShowGotchas] = useState(false);

  // Pending-edit buffer: each control writes to here on change, then
  // a single Apply button POSTs the whole batch. Lets the user stage
  // multiple changes (tz + format + dst) and commit atomically
  // instead of one per field — same UX as Sony's Settings screen.
  const [pendingTz, setPendingTz] = useState<number | null>(null);
  const [pendingDateFormat, setPendingDateFormat] = useState<number | null>(
    null,
  );
  const [pendingTimeFormat, setPendingTimeFormat] = useState<number | null>(
    null,
  );
  const [pendingSummer, setPendingSummer] = useState<number | null>(null);
  const [pendingSetAuto, setPendingSetAuto] = useState<number | null>(null);

  const hasPending =
    pendingTz !== null ||
    pendingDateFormat !== null ||
    pendingTimeFormat !== null ||
    pendingSummer !== null ||
    pendingSetAuto !== null;

  const refresh = useCallback(async () => {
    if (!payloadUp || !host.trim()) return;
    // 2.12.0: canonical useStaleHostGuard (was hand-rolled per Hardware).
    const probe = guardDt.capture();
    setLoading(true);
    try {
      const addr = transferAddr(probe.host);
      const r = (await invoke("ps5_time_state_get", { addr })) as PsTimeStateJson;
      if (probe.isStale()) return;
      setState(r);
      setError(null);
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [host, payloadUp, guardDt]);

  useEffect(() => {
    if (!payloadUp) return;
    void refresh();
  }, [payloadUp, refresh]);

  const handleApply = useCallback(async () => {
    if (!hasPending || writeBusy) return;
    setWriteBusy(true);
    setWriteResult(null);
    try {
      const addr = `${host}:${PS5_PAYLOAD_PORT}`;
      const args: Record<string, unknown> = { addr };
      if (pendingTz !== null) args.tzIndex = pendingTz;
      if (pendingDateFormat !== null) args.dateFormat = pendingDateFormat;
      if (pendingTimeFormat !== null) args.timeFormat = pendingTimeFormat;
      if (pendingSummer !== null) args.summerPolicy = pendingSummer;
      if (pendingSetAuto !== null) args.setAuto = pendingSetAuto;
      const r = (await invoke("ps5_time_state_set", args)) as PsTimeStateSetJson;
      setWriteResult(r);
      // Clear pending only for fields that succeeded; leave the
      // others so the user sees what's still uncommitted.
      if (r.tz_index_attempted && r.tz_index_rc === 0) setPendingTz(null);
      if (r.date_format_attempted && r.date_format_rc === 0)
        setPendingDateFormat(null);
      if (r.time_format_attempted && r.time_format_rc === 0)
        setPendingTimeFormat(null);
      if (r.summer_policy_attempted && r.summer_policy_rc === 0)
        setPendingSummer(null);
      if (r.set_auto_attempted && r.set_auto_rc === 0) setPendingSetAuto(null);
      // Re-read so the panel reflects what actually landed.
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setWriteBusy(false);
    }
  }, [
    hasPending,
    writeBusy,
    host,
    pendingTz,
    pendingDateFormat,
    pendingTimeFormat,
    pendingSummer,
    pendingSetAuto,
    refresh,
  ]);

  const ntpDrift = state ? formatNtpDrift(state) : null;

  // Derived display values: current effective value = pending if set,
  // otherwise the read-back value from state.
  const effectiveTz = pendingTz ?? state?.tz_index ?? 0;
  const effectiveDateFormat = pendingDateFormat ?? state?.date_format ?? 0;
  const effectiveTimeFormat = pendingTimeFormat ?? state?.time_format ?? 0;
  const effectiveSummer = pendingSummer ?? state?.summer_policy ?? 0;
  const effectiveSetAuto = pendingSetAuto ?? state?.set_auto ?? 0;

  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center justify-between gap-2 text-xs uppercase tracking-wide text-[var(--color-muted)]">
        <span className="flex items-center gap-2">
          <CalendarClock size={14} />
          <span>
            {tr("hardware_dtstate_title", undefined, "Date & Time settings")}
          </span>
          <span className="rounded-full border border-[var(--color-warn)] px-1.5 py-0.5 text-[9px] uppercase tracking-wider text-[var(--color-warn)]">
            {tr("hardware_dtstate_experimental", undefined, "experimental")}
          </span>
        </span>
        <button
          type="button"
          onClick={() => setShowGotchas((v) => !v)}
          className="text-[10px] underline decoration-dotted hover:text-[var(--color-text)]"
        >
          {showGotchas
            ? tr("hardware_dtstate_hide_warnings", undefined, "Hide warnings")
            : tr("hardware_dtstate_show_warnings", undefined, "Important warnings")}
        </button>
      </header>

      {/* Gotcha panel — collapsible because most users will read it
          once. Six warnings: two clocks, past wall clock breaks PSN,
          future wall clock breaks game certs, set_auto re-syncs on
          reboot, tzdata bundled in firmware, secure RTC owns trophy
          times. */}
      {showGotchas && (
        <div className="mb-3 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-muted)]">
          <ul className="list-disc space-y-1 pl-4">
            <li>
              {tr(
                "hardware_dtstate_warn_two_clocks",
                undefined,
                "PS5 has two clocks: this user-visible wall clock, plus a kernel-protected secure RTC that signs trophies and licenses. Setting this clock CAN'T fake trophy timestamps.",
              )}
            </li>
            <li>
              {tr(
                "hardware_dtstate_warn_past",
                undefined,
                "Setting the clock far in the past breaks PSN sign-in (TLS certificate validation fails).",
              )}
            </li>
            <li>
              {tr(
                "hardware_dtstate_warn_future",
                undefined,
                "Setting the clock far in the future breaks signed-game validation (GTAV-stuck-at-90%-load class).",
              )}
            </li>
            <li>
              {tr(
                "hardware_dtstate_warn_set_auto",
                undefined,
                "If \"Use Sony's NTP\" is ON, your PS5 silently re-syncs the wall clock on every reboot — manual time doesn't persist unless you turn it OFF first.",
              )}
            </li>
            <li>
              {tr(
                "hardware_dtstate_warn_tzdata",
                undefined,
                "DST rules ship in the firmware's tzdata. Recently-changed regions (Lebanon 2023, Mexico 2022) may be wrong until the next firmware update.",
              )}
            </li>
            <li>
              {tr(
                "hardware_dtstate_warn_novel",
                undefined,
                "Write side of this panel is novel territory — ps5upload is the first homebrew to set these registry keys. Sony's Settings will reset anything that misbehaves.",
              )}
            </li>
          </ul>
        </div>
      )}

      {!state && !error && (
        <div className="text-[11px] text-[var(--color-muted)]">
          {loading
            ? tr("hardware_dtstate_loading", undefined, "Loading…")
            : tr(
                "hardware_dtstate_no_payload",
                undefined,
                "Connect to a PS5 with ps5upload payload running to see Date & Time settings.",
              )}
        </div>
      )}

      {state && (
        <div className="space-y-3 text-xs">
          {/* NTP drift row — shown only when both NTP tick and wall
              clock read successfully. Positive = wall clock is
              AHEAD of what NTP would say. */}
          {ntpDrift !== null && (
            <StatRow
              label={tr(
                "hardware_dtstate_ntp_drift",
                undefined,
                "Wall vs NTP",
              )}
              value={ntpDrift}
              hint={tr(
                "hardware_dtstate_ntp_drift_hint",
                undefined,
                "Difference between the PS5's wall clock and what Sony's NTP says (cached from last sync). Large drift = manual time has wandered.",
              )}
            />
          )}

          {/* Timezone — index editor + offset display. The tz_index
              is an enum into Sony's bundled tzdata table; the
              user-friendly mapping isn't documented per-firmware so
              we expose the raw index plus the offset for confirmation. */}
          <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
            <div className="mb-1 flex items-center justify-between gap-2">
              <span className="text-[11px] font-medium">
                {tr("hardware_dtstate_tz", undefined, "Timezone")}
              </span>
              {state.tz_offset_min_avail && (
                <span className="text-[10px] text-[var(--color-muted)]">
                  {tr(
                    "hardware_dtstate_tz_offset",
                    {
                      sign: state.tz_offset_min >= 0 ? "+" : "−",
                      h: Math.floor(Math.abs(state.tz_offset_min) / 60),
                      m: (Math.abs(state.tz_offset_min) % 60)
                        .toString()
                        .padStart(2, "0"),
                    },
                    `UTC ${state.tz_offset_min >= 0 ? "+" : "−"}${Math.floor(Math.abs(state.tz_offset_min) / 60)}:${(Math.abs(state.tz_offset_min) % 60).toString().padStart(2, "0")}`,
                  )}
                </span>
              )}
            </div>
            {state.tz_index_avail ? (
              <input
                type="number"
                value={effectiveTz}
                onChange={(e) => {
                  const n = parseInt(e.target.value, 10);
                  setPendingTz(Number.isFinite(n) && n >= 0 ? n : null);
                }}
                className="w-24 rounded border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-xs"
                min={0}
                max={500}
              />
            ) : (
              <span className="text-[11px] text-[var(--color-muted)]">
                {tr(
                  "hardware_dtstate_field_unavailable",
                  undefined,
                  "(not readable on this firmware)",
                )}
              </span>
            )}
            {pendingTz !== null && (
              <span className="ml-2 text-[10px] text-[var(--color-accent)]">
                {tr("hardware_dtstate_pending", undefined, "(pending)")}
              </span>
            )}
          </div>

          {/* Date format + Time format pair */}
          <div className="grid grid-cols-2 gap-2">
            <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
              <div className="mb-1 text-[11px] font-medium">
                {tr(
                  "hardware_dtstate_date_format",
                  undefined,
                  "Date format",
                )}
              </div>
              <select
                value={effectiveDateFormat}
                disabled={!state.date_format_avail}
                onChange={(e) => setPendingDateFormat(parseInt(e.target.value, 10))}
                className="w-full rounded border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-xs"
              >
                <option value={0}>
                  {tr("hardware_dtstate_date_fmt_ymd", undefined, "YYYY/MM/DD")}
                </option>
                <option value={1}>
                  {tr("hardware_dtstate_date_fmt_dmy", undefined, "DD/MM/YYYY")}
                </option>
                <option value={2}>
                  {tr("hardware_dtstate_date_fmt_mdy", undefined, "MM/DD/YYYY")}
                </option>
              </select>
              {pendingDateFormat !== null && (
                <span className="text-[10px] text-[var(--color-accent)]">
                  {tr("hardware_dtstate_pending", undefined, "(pending)")}
                </span>
              )}
            </div>
            <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
              <div className="mb-1 text-[11px] font-medium">
                {tr(
                  "hardware_dtstate_time_format",
                  undefined,
                  "Time format",
                )}
              </div>
              <select
                value={effectiveTimeFormat}
                disabled={!state.time_format_avail}
                onChange={(e) => setPendingTimeFormat(parseInt(e.target.value, 10))}
                className="w-full rounded border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-xs"
              >
                <option value={0}>{tr("hardware_dtstate_24h", undefined, "24-hour")}</option>
                <option value={1}>{tr("hardware_dtstate_12h", undefined, "12-hour")}</option>
              </select>
              {pendingTimeFormat !== null && (
                <span className="text-[10px] text-[var(--color-accent)]">
                  {tr("hardware_dtstate_pending", undefined, "(pending)")}
                </span>
              )}
            </div>
          </div>

          {/* DST policy + currently-in-DST status */}
          <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
            <div className="mb-1 flex items-center justify-between gap-2">
              <span className="text-[11px] font-medium">
                {tr(
                  "hardware_dtstate_dst",
                  undefined,
                  "Daylight saving time",
                )}
              </span>
              {state.is_summer_time_avail && (
                <span className="text-[10px] text-[var(--color-muted)]">
                  {state.is_summer_time
                    ? tr(
                        "hardware_dtstate_currently_dst",
                        undefined,
                        "currently observing DST",
                      )
                    : tr(
                        "hardware_dtstate_not_currently_dst",
                        undefined,
                        "not observing DST",
                      )}
                </span>
              )}
            </div>
            <select
              value={effectiveSummer}
              disabled={!state.summer_policy_avail}
              onChange={(e) => setPendingSummer(parseInt(e.target.value, 10))}
              className="w-full rounded border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-xs"
            >
              <option value={0}>
                {tr("hardware_dtstate_dst_off", undefined, "Off")}
              </option>
              <option value={1}>
                {tr("hardware_dtstate_dst_auto", undefined, "Auto (tzdata-driven)")}
              </option>
              <option value={2}>
                {tr("hardware_dtstate_dst_on", undefined, "Always on (manual)")}
              </option>
            </select>
            {pendingSummer !== null && (
              <span className="text-[10px] text-[var(--color-accent)]">
                {tr("hardware_dtstate_pending", undefined, "(pending)")}
              </span>
            )}
          </div>

          {/* NTP auto-sync toggle + error-count diagnostic */}
          <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
            <label className="flex items-center justify-between gap-2 text-[11px]">
              <span>
                <span className="font-medium">
                  {tr(
                    "hardware_dtstate_set_auto",
                    undefined,
                    "Use Sony's NTP (auto-sync wall clock)",
                  )}
                </span>
                {pendingSetAuto !== null && (
                  <span className="ml-1 text-[10px] text-[var(--color-accent)]">
                    {tr("hardware_dtstate_pending", undefined, "(pending)")}
                  </span>
                )}
              </span>
              <input
                type="checkbox"
                checked={effectiveSetAuto === 1}
                disabled={!state.set_auto_avail}
                onChange={(e) => setPendingSetAuto(e.target.checked ? 1 : 0)}
                className="h-4 w-4"
              />
            </label>
            {state.rtc_error_count_avail && (
              <div className="mt-1 text-[10px] text-[var(--color-muted)]">
                {tr(
                  "hardware_dtstate_ntp_errors",
                  { n: state.rtc_error_count },
                  `NTP sync failures: ${state.rtc_error_count}`,
                )}
                {state.rtc_error_count > 5 && (
                  <span className="ml-1 text-[var(--color-warn)]">
                    {tr(
                      "hardware_dtstate_ntp_errors_hint",
                      undefined,
                      "(DNS broken? UDP 123 blocked?)",
                    )}
                  </span>
                )}
              </div>
            )}
          </div>

          {/* tzdata version footer — lets users notice when a firmware
              update silently shipped new DST rules. */}
          {state.tzdata_avail && state.tzdata.length > 0 && (
            <div className="text-[10px] text-[var(--color-muted)]">
              {tr(
                "hardware_dtstate_tzdata_version",
                { version: state.tzdata },
                `tzdata version: ${state.tzdata}`,
              )}
            </div>
          )}

          {/* Apply button — only shown when there's something pending. */}
          {hasPending && (
            <div className="flex items-center gap-2 pt-2">
              <Button
                variant="primary"
                size="sm"
                disabled={writeBusy}
                onClick={handleApply}
              >
                {writeBusy
                  ? tr("hardware_dtstate_applying", undefined, "Applying…")
                  : tr("hardware_dtstate_apply", undefined, "Apply changes")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setPendingTz(null);
                  setPendingDateFormat(null);
                  setPendingTimeFormat(null);
                  setPendingSummer(null);
                  setPendingSetAuto(null);
                }}
              >
                {tr("hardware_dtstate_discard", undefined, "Discard")}
              </Button>
            </div>
          )}

          {/* Result line — surface per-field outcome when any field
              was rejected so the user sees which writes took and
              which didn't. */}
          {writeResult && (
            <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-[11px]">
              {writeResult.ok ? (
                <div className="text-[var(--color-good)]">
                  {tr(
                    "hardware_dtstate_apply_ok",
                    undefined,
                    "Applied. Sony's Settings will reflect the change after the next foreground render.",
                  )}
                </div>
              ) : (
                <>
                  <div className="text-[var(--color-bad)]">
                    {tr(
                      "hardware_dtstate_apply_partial",
                      undefined,
                      "Some writes were rejected — see below.",
                    )}
                  </div>
                  <ul className="mt-1 space-y-0.5 text-[10px] text-[var(--color-muted)]">
                    {writeResult.tz_index_attempted &&
                      writeResult.tz_index_rc !== 0 && (
                        <li>
                          {tr(
                            "hardware_dtstate_field_err",
                            {
                              field: "tz_index",
                              rc: writeResult.tz_index_rc,
                              err: writeResult.tz_index_err
                                .toString(16)
                                .padStart(8, "0"),
                            },
                            `tz_index rc=${writeResult.tz_index_rc} err=0x${writeResult.tz_index_err.toString(16).padStart(8, "0")}`,
                          )}
                        </li>
                      )}
                    {writeResult.date_format_attempted &&
                      writeResult.date_format_rc !== 0 && (
                        <li>
                          {tr(
                            "hardware_dtstate_field_err",
                            {
                              field: "date_format",
                              rc: writeResult.date_format_rc,
                              err: writeResult.date_format_err
                                .toString(16)
                                .padStart(8, "0"),
                            },
                            `date_format rc=${writeResult.date_format_rc} err=0x${writeResult.date_format_err.toString(16).padStart(8, "0")}`,
                          )}
                        </li>
                      )}
                    {writeResult.time_format_attempted &&
                      writeResult.time_format_rc !== 0 && (
                        <li>
                          {tr(
                            "hardware_dtstate_field_err",
                            {
                              field: "time_format",
                              rc: writeResult.time_format_rc,
                              err: writeResult.time_format_err
                                .toString(16)
                                .padStart(8, "0"),
                            },
                            `time_format rc=${writeResult.time_format_rc} err=0x${writeResult.time_format_err.toString(16).padStart(8, "0")}`,
                          )}
                        </li>
                      )}
                    {writeResult.summer_policy_attempted &&
                      writeResult.summer_policy_rc !== 0 && (
                        <li>
                          {tr(
                            "hardware_dtstate_field_err",
                            {
                              field: "summer_policy",
                              rc: writeResult.summer_policy_rc,
                              err: writeResult.summer_policy_err
                                .toString(16)
                                .padStart(8, "0"),
                            },
                            `summer_policy rc=${writeResult.summer_policy_rc} err=0x${writeResult.summer_policy_err.toString(16).padStart(8, "0")}`,
                          )}
                        </li>
                      )}
                    {writeResult.set_auto_attempted &&
                      writeResult.set_auto_rc !== 0 && (
                        <li>
                          {tr(
                            "hardware_dtstate_field_err",
                            {
                              field: "set_auto",
                              rc: writeResult.set_auto_rc,
                              err: writeResult.set_auto_err
                                .toString(16)
                                .padStart(8, "0"),
                            },
                            `set_auto rc=${writeResult.set_auto_rc} err=0x${writeResult.set_auto_err.toString(16).padStart(8, "0")}`,
                          )}
                        </li>
                      )}
                  </ul>
                </>
              )}
            </div>
          )}
        </div>
      )}

      {error && (
        <div className="mt-2 text-[11px] text-[var(--color-bad)]">{error}</div>
      )}
    </section>
  );
}
