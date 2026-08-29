import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../../lib/invokeLogged";
import {
  Cpu,
  Thermometer,
  Activity,
  Clock,
  RefreshCw,
  Fan,
  Check,
  HardDrive,
  Image as ImageIcon,
  CalendarClock,
  Globe,
  Loader2,
} from "lucide-react";

import { AlertTriangle } from "lucide-react";
import {
  PageHeader,
  ErrorCard,
  Button,
  ConnectionGate,
  Spinner,
} from "../../components";
import { useTr } from "../../state/lang";
import PowerTelemetryPanel from "./PowerTelemetryPanel";
import NetworkPanel from "./NetworkPanel";
import PeripheralPanel from "./PeripheralPanel";
import SpeedTestPanel from "./SpeedTestPanel";
import { useDocumentVisible } from "../../lib/visibility";
import { mgmtAddr, transferAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";

import { useConnectionStore } from "../../state/connection";
import {
  psTimeToDate,
  formatUtcCompact,
  formatDrift,
  classifySyncResult,
  formatSyncSource,
  type PsTimeJson,
  type PsTimeSyncJson,
} from "../../lib/sysTimeFormat";
import {
  fetchHwInfo,
  fetchHwPower,
  fetchHwStorage,
  fetchHwTemps,
  fetchDriveSensors,
  setFanThreshold,
  smpMetaControl,
  smpMetaStats,
  FAN_THRESHOLD_MIN_C,
  FAN_THRESHOLD_MAX_C,
  type HwInfo,
  type HwPower,
  type HwStorage,
  type HwTemps,
  type DriveSensorList,
  type SmpMetaStats,
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
 *  uptime + storage are polled every 5s. Live temps/clock are NOT
 *  polled and never auto-fire — the payload reads them via direct
 *  sceKernel* sensor calls, and on firmware we can't vet ahead of time
 *  such a call can WEDGE (the SoC-power export hangs on FW 9.60 PS5 Pro,
 *  confirmed), taking the mgmt worker — and thus the helper — down with
 *  it. So they're read on demand only, behind an explicit "Read sensors"
 *  click. See payload/src/hw_info.c::hw_temps_get_text. */
const POLL_INTERVAL_MS = 5_000;

/** Minimum gap between on-demand sensor reads. Rate-limits the manual
 *  "Read sensors" button so an impatient double-click can't fire two
 *  back-to-back sensor reads on firmware where a direct sensor call is
 *  slow or wedges. Each read is a fresh TCP RPC + sensor syscalls. */
const SENSOR_READ_COOLDOWN_MS = 2_000;

/**
 * Safety gate for a live-sensor read. Returns true when a fresh read
 * should be allowed right now.
 *
 * The cooldown rate-limits the manual "Read sensors" button so a
 * double-click can't compound two reads. There is no auto-poller for
 * this path (a direct sensor call can wedge on firmware we can't vet
 * ahead of time and drop the helper), so this is purely a button
 * debounce.
 */
function shouldAllowSensorRead(
  lastReadAt: number | null,
  now: number,
): boolean {
  if (lastReadAt !== null && now - lastReadAt < SENSOR_READ_COOLDOWN_MS) {
    return false;
  }
  return true;
}

function formatBytes(n: number): string {
  if (n <= 0) return "—";
  const gib = n / 1024 ** 3;
  if (gib >= 1) return `${gib.toFixed(2)} GiB`;
  const mib = n / 1024 ** 2;
  return `${mib.toFixed(0)} MiB`;
}

/** Format three load-average values as "X.XX / Y.YY / Z.ZZ".
 *  Negative inputs render as "—" — the payload uses -1.00 to mean
 *  "getloadavg unavailable on this firmware". Two decimals matches
 *  what BSD/Linux `uptime` shows; finer precision is noise. */
function formatLoadAvg(a: number, b: number, c: number): string {
  const fmt = (n: number) => (n < 0 ? "—" : n.toFixed(2));
  return `${fmt(a)} / ${fmt(b)} / ${fmt(c)}`;
}

/** Format Sony's packed kernel firmware version word.
 *
 *  The encoding is the same one Sony's libkernel uses internally:
 *  the high 16 bits hold the family ("9.60" = 0x0960), the low 16
 *  bits hold the point release. We render it as `MAJOR.MINOR.POINT`
 *  so users can compare against the FW number their console reports
 *  in Settings → System.
 *
 *  Example: 0x09600000 → "9.60.0000", 0x09601000 → "9.60.1000".
 */
function formatKernelFw(v: number): string {
  if (v <= 0) return "—";
  const major = (v >>> 24) & 0xff;
  const minor = (v >>> 16) & 0xff;
  const point = v & 0xffff;
  return `${major.toString(16).padStart(2, "0")}.${minor
    .toString(16)
    .padStart(2, "0")}.${point.toString(16).padStart(4, "0")}`;
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
  // Show both scales — the sensors read Celsius; Fahrenheit is for users who
  // think in °F. Rounded to a whole degree (sensor precision doesn't warrant
  // decimals). e.g. "62°C / 144°F".
  const f = Math.round(c * 1.8 + 32);
  return `${c}°C / ${f}°F`;
}

function formatFreq(mhz: number): string {
  if (mhz <= 0) return "—";
  if (mhz >= 1000) return `${(mhz / 1000).toFixed(2)} GHz`;
  return `${mhz} MHz`;
}

/** 0–100 percentage; `-1` (or any negative) = "unavailable" → dash. Used
 *  for CPU usage and fan duty, which the payload reports as -1 when the
 *  API isn't available or on a basic (non-extended) read. */
function formatPct(n: number): string {
  if (n < 0) return "—";
  return `${n}%`;
}

/** Sony's raw "basic product shape" code. The numeric→family mapping
 *  isn't officially documented and we deliberately do NOT guess a label
 *  (mislabelling a console is worse than showing a number) — the model
 *  string (CFI-xxxx) is the authoritative identifier; this raw code is a
 *  secondary cross-check to calibrate against known hardware. `-1` =
 *  unavailable. */
function formatProductShape(n: number): string {
  if (n < 0) return "—";
  return `Code ${n}`;
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
  const [drives, setDrives] = useState<DriveSensorList | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Live sensors (temps/clock/power) are read ON DEMAND only, never on a
  // timer — see the readSensors effect below for the full rationale. The
  // payload reads them via the direct sceKernel* APIs, which can hang on
  // some firmware; gating behind an explicit "Read sensors" click means a
  // read never fires unattended. sensorBusy prevents overlapping reads if
  // the PS5 is slow; sensorReadAt enforces the SENSOR_READ_COOLDOWN_MS
  // minimum gap so an impatient double-click can't fire two back-to-back.
  const sensorBusy = useRef(false);
  const [sensorLoading, setSensorLoading] = useState(false);
  const [sensorReadAt, setSensorReadAt] = useState<number | null>(null);

  // Guard flag: if a previous refresh is still pending, skip new
  // ticks. Prevents overlapping requests if the PS5 is slow to
  // respond.
  const busy = useRef(false);
  // Shadow ref for `info` so `refresh` can check "have we fetched
  // static info yet?" without listing `info` in its deps. Listing
  // it caused the 5s interval to be torn down + re-armed every
  // time info changed, producing a double-poll burst on first
  // load (Hardware crash audit, 2.12.0).
  const infoRef = useRef<HwInfo | null>(null);

  // All four cards show data belonging to ONE console. On tab switch,
  // drop everything immediately — without this, the previous console's
  // model/serial/temps render under the new console's name, and worse,
  // the infoRef shadow means the NEW console's static info would never
  // be fetched at all (refresh skips HW_INFO once infoRef is non-null).
  useEffect(() => {
    infoRef.current = null;
    setInfo(null);
    setTemps(null);
    setPower(null);
    setStorage(null);
    setDrives(null);
    setError(null);
    setSensorReadAt(null);
  }, [host]);

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
      // NOTE: fetchHwTemps is deliberately NOT here — its direct sensor
      // calls can wedge on untested firmware and must never run on a
      // timer (that would drop the helper unattended). It moved to
      // readSensors() (on-demand). Everything left in this poll is
      // hang-free: power = sysctl uptime/loadavg, storage = statfs,
      // info = in-process dlsym.
      const [nextPower, nextStorage] = await Promise.all([
        fetchHwPower(addr),
        // Storage is essentially static at the per-second polling rate
        // (a 100 GB game install changes free-space slowly), but
        // refreshing it alongside uptime keeps the card live without
        // a separate slow timer. Pre-2.2.26 payloads fail this with
        // unsupported_frame; treat that as "no data" and let the rest
        // of the tab keep working.
        fetchHwStorage(addr).catch(() => null),
      ]);
      if (probe.isStale()) return;
      setPower(nextPower);
      setStorage(nextStorage);
      // info is static; fetch once if we don't have it yet. Read
      // via the ref shadow so this useCallback doesn't depend on
      // `info` — depending on it caused refresh to be recreated on
      // first-load, which re-armed the 5s interval mid-tick and
      // produced a double-poll burst (Hardware crash audit, 2.12.0).
      // Net effect: HW_INFO request only fires once per tab-mount.
      if (infoRef.current === null) {
        const nextInfo = await fetchHwInfo(addr).catch(() => null);
        if (probe.isStale()) return;
        if (nextInfo) {
          infoRef.current = nextInfo;
          setInfo(nextInfo);
        }
      }
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      busy.current = false;
      setLoading(false);
    }
  }, [host, payloadStatus, guard]);

  // On-demand live-sensor read. Deliberately separate from refresh()
  // and never armed on a timer: this is the one path whose direct sensor
  // calls can wedge on untested firmware (see shouldAllowSensorRead's
  // safety note). A single user click → a single read, gated by the cooldown.
  const readSensors = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    if (sensorBusy.current) return;
    if (!shouldAllowSensorRead(sensorReadAt, Date.now())) return;
    sensorBusy.current = true;
    setSensorLoading(true);
    setError(null);
    const probe = guard.capture();
    try {
      const addr = transferAddr(probe.host);
      // extended=true: this explicit user action is the ONLY caller allowed
      // to trigger the on-demand telemetry (SoC power / CPU usage / fan duty
      // / product shape) on the payload. The Dashboard's auto-poll calls
      // fetchHwTemps without extended, so its timer never fires those.
      const [next, nextDrives] = await Promise.all([
        fetchHwTemps(addr, true),
        fetchDriveSensors(addr).catch(() => null),
      ]);
      if (probe.isStale()) return;
      setTemps(next);
      if (nextDrives) setDrives(nextDrives);
      setSensorReadAt(Date.now());
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      sensorBusy.current = false;
      setSensorLoading(false);
    }
  }, [host, payloadStatus, guard, sensorReadAt]);

  // Mount + auto-poll every POLL_INTERVAL_MS while payload is up AND
  // the window is visible. Pausing on minimize keeps idle laptops
  // from spamming the PS5's mgmt port. Resumes on visibility-change
  // with a fresh immediate refresh so the panel is up-to-date when the
  // user looks at it again.
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

  // The live-sensor read (temps / clock) is ON-DEMAND ONLY — it is
  // deliberately NEVER armed on a timer and never auto-fires on mount.
  // Rationale (regression report, FW 9.60 PS5 Pro): a direct sceKernel*
  // sensor call can WEDGE on firmware we can't vet ahead of time — the
  // SoC-power export hangs outright on FW 9.60, and Sony's caller-pid
  // check can stall other sensor pointers. A wedged call never returns,
  // so the mgmt worker servicing it is lost and the helper appears to
  // "disconnect" — the original symptom was "each time I open Hardware
  // the helper disconnects." The payload now calls ONLY the direct
  // exports that return cleanly and reports anything else as 0 =
  // unavailable (no ptrace fallback), but auto-polling a read that might
  // wedge on an untested SKU/FW is still unsafe, so we gate it behind an
  // explicit user click. The hang-free reads (power = sysctl
  // uptime/loadavg, storage = statfs, info = in-process dlsym) keep
  // their safe 5s auto-poll above.

  return (
    <div className="p-6">
      <PageHeader
        icon={Cpu}
        title={tr("hardware_title", undefined, "Hardware")}
        loading={loading}
        description={tr(
          "hardware_description",
          undefined,
          "System info, uptime and storage auto-refresh every 5 seconds while the helper is connected. Live temperatures and CPU clock are read on demand — click Read sensors. Some firmware doesn't expose every sensor; any unavailable reading shows as a dash.",
        )}
        right={
          <div className="flex items-center gap-2">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={
                sensorLoading ? (
                  <Spinner size={12} tone="inherit" />
                ) : (
                  <Thermometer size={12} />
                )
              }
              onClick={readSensors}
              disabled={
                sensorLoading || !host?.trim() || payloadStatus !== "up"
              }
            >
              {tr("hardware_read_sensors", undefined, "Read sensors")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RefreshCw size={12} />}
              onClick={refresh}
              disabled={loading || !host?.trim() || payloadStatus !== "up"}
            >
              {tr("refresh", undefined, "Refresh")}
            </Button>
          </div>
        }
      />

      {/* Only surface the error card when the payload IS up — otherwise
          the ConnectionGate below already explains the state. */}
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

      <ConnectionGate require="payload">
        <div className="grid gap-4 lg:grid-cols-2 xl:grid-cols-3">
          <SensorCard
            icon={<Thermometer size={14} />}
            title={tr("hardware_temperatures", undefined, "Temperatures")}
          >
            <StatRow
              label={tr("hardware_label_cpu", "CPU")}
              value={formatTemp(temps?.cpu_temp ?? 0)}
              hint={
                !temps
                  ? tr(
                      "hw_sensor_on_demand",
                      undefined,
                      "Click Read sensors for a live reading",
                    )
                  : temps.cpu_temp === 0
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
            <StatRow
              label={tr("hardware_label_m2", "M.2 SSD")}
              value={formatTemp(temps?.m2_temp ?? 0)}
              hint={
                temps && temps.m2_temp === 0
                  ? tr(
                      "hw_m2_not_installed",
                      undefined,
                      "No M.2 drive installed or sensor unavailable",
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
                  ? tr("hw_from_kernel_tsc", undefined, "From kernel TSC")
                  : undefined
              }
            />
            <StatRow
              label={tr("hw_cpu_usage", undefined, "CPU usage")}
              value={formatPct(temps?.cpu_usage_pct ?? -1)}
              hint={
                !temps
                  ? tr(
                      "hw_sensor_on_demand",
                      undefined,
                      "Click Read sensors for a live reading",
                    )
                  : undefined
              }
            />
            {/* SoC power draw is intentionally not shown: the only Sony
                API for it (sceKernelGetSocPowerConsumption) hangs the
                helper on FW 9.60 (Pro, HW-confirmed) and returns a useless
                0 on FW 5.10 (phat), so the payload never reads it. */}
            <StatRow
              label={tr("hw_fan_duty", undefined, "Fan duty")}
              value={formatPct(temps?.fan_duty_pct ?? -1)}
              hint={
                temps && temps.fan_duty_pct >= 0
                  ? tr("hw_fan_duty_approx", undefined, "Approximate")
                  : undefined
              }
            />
          </SensorCard>

          <SensorCard
            icon={<Clock size={14} />}
            title={tr("hardware_uptime", undefined, "Uptime")}
          >
            <StatRow
              label={tr(
                "hw_running_since_boot",
                undefined,
                "Running since boot",
              )}
              value={formatUptime(power?.operating_time_sec ?? 0)}
            />
            {/* System load avg comes from the SDK's getloadavg (added
                via the 2026-05 ps5-payload-dev/sdk refresh). Negative
                value = kernel didn't return a reading on this FW; we
                hide the row instead of showing a confusing "-1.00". */}
            {power && power.load_avg_1m >= 0 && (
              <StatRow
                label={tr("hw_load_avg", undefined, "Load avg (1m / 5m / 15m)")}
                value={formatLoadAvg(
                  power.load_avg_1m,
                  power.load_avg_5m,
                  power.load_avg_15m,
                )}
                hint={
                  info?.ncpu
                    ? tr(
                        "hw_load_avg_hint_ncpu",
                        { ncpu: info.ncpu },
                        `Full load on this PS5 ≈ ${info.ncpu}.00`,
                      )
                    : undefined
                }
              />
            )}
          </SensorCard>

          <SensorCard
            icon={<Cpu size={14} />}
            title={tr("hardware_system", undefined, "System")}
          >
            <StatRow
              label={tr("hw_model", undefined, "Model")}
              value={info?.model ?? "—"}
            />
            <StatRow
              label={tr("hw_serial", undefined, "Serial")}
              value={info?.serial ?? "—"}
            />
            <StatRow
              label={tr("hw_os", undefined, "OS")}
              value={info?.os ?? "—"}
            />
            {/* Precise FW word via kernel R/W (added via the 2026-05
                SDK refresh, kernel_get_fw_version). 0 means kernel
                R/W wasn't available on this loader — fall back to
                the kern.version string already in info.os. */}
            {info && info.kernel_fw_version > 0 && (
              <StatRow
                label={tr("hw_fw_precise", undefined, "FW (kernel)")}
                value={formatKernelFw(info.kernel_fw_version)}
                hint={tr(
                  "hw_fw_precise_hint",
                  undefined,
                  "Read via kernel R/W (kstuff/etaHEN). Precise point release.",
                )}
              />
            )}
            <StatRow
              label={tr("hw_ram", undefined, "RAM")}
              value={info ? formatBytes(info.physmem) : "—"}
            />
            <StatRow
              label={tr("hw_cpu_cores", undefined, "CPU cores")}
              value={info?.ncpu ? String(info.ncpu) : "—"}
            />
            {/* Raw Sony product-shape code — a secondary cross-check on the
                hardware family (the model string above is authoritative).
                Read on demand (Read sensors); shows "—" until then or where
                the API isn't available. */}
            {temps && temps.product_shape >= 0 && (
              <StatRow
                label={tr("hw_product_shape", undefined, "Product shape")}
                value={formatProductShape(temps.product_shape)}
                hint={tr(
                  "hw_product_shape_hint",
                  undefined,
                  "Raw Sony code · model name is authoritative",
                )}
              />
            )}
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

          {/* Drive sensors — USB / extended storage temperatures and
              capacity via SCSI LOG SENSE. Read on demand alongside temps
              (same "Read sensors" click); shows nothing until then. The
              internal SSD + M.2 summaries below come from the same call's
              `storage` array (statvfs, always safe) so they're available
              even without a Read sensors click if the payload responds. */}
          {drives && drives.drives.length > 0 && (
            <SensorCard
              icon={<HardDrive size={14} />}
              title={tr("hw_drives_title", undefined, "Drives")}
            >
              {drives.drives.map((d) => (
                <div key={d.device} className="py-1">
                  <StatRow
                    label={d.ident || d.device}
                    value={
                      d.access_denied
                        ? tr("hw_drive_access_denied", undefined, "Access denied")
                        : d.temp_c != null
                          ? formatTemp(d.temp_c)
                          : d.temp_err != null
                            ? tr("hw_drive_no_temp", undefined, "No temp sensor")
                            : "—"
                    }
                    hint={d.size_bytes > 0 ? formatStorageGB(d.size_bytes) : undefined}
                  />
                  {d.fs_total_bytes != null && d.fs_total_bytes > 0 && (
                    <StatRow
                      label={tr("hw_drive_free", undefined, "Free")}
                      value={formatStorageGB(d.fs_free_bytes ?? 0)}
                    />
                  )}
                </div>
              ))}
            </SensorCard>
          )}

          <FanThresholdCard
            // key on host so the card's session-local state (lastSetC/draftC,
            // active preset, busy/error) resets on console switch — otherwise
            // console B shows console A's "Set to X°C" / active preset.
            key={host ?? ""}
            host={host ?? ""}
            payloadUp={payloadStatus === "up"}
            pinnedC={temps?.fan_pinned_c ?? 0}
          />

          <SmpMetaCard host={host ?? ""} payloadUp={payloadStatus === "up"} />

          <SystemTimeCard
            // Keyed on host so a console switch clears the previous
            // console's sync result and drift snapshot, rather than
            // showing console A's outcome under console B's name.
            key={host ?? ""}
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
              {/* Canonical mgmtAddr() (from lib/addr.ts) handles
                  the "user pasted ip:port" edge case — raw string
                  concat would produce ip:9113:9114 which Connection
                  ::connect's SocketAddr parse rejects with a BAD_
                  GATEWAY, leaving the panel silently broken. */}
              <PowerTelemetryPanel mgmtAddr={mgmtAddr(host)} />
              <NetworkPanel mgmtAddr={mgmtAddr(host)} />
              <SpeedTestPanel mgmtAddr={mgmtAddr(host)} />
              <PeripheralPanel mgmtAddr={mgmtAddr(host)} />
            </>
          )}
        </div>
      </ConnectionGate>

      {/* Kernel log lives OUTSIDE the sensor grid: the <pre> renders
       * hundreds of monospace lines and would get squeezed into a 1/3-
       * width column at xl breakpoint (the grid container above is
       * `lg:grid-cols-2 xl:grid-cols-3`), making the log unreadable.
       * Full-width section below the grid gives it the horizontal room
       * it needs without disturbing the responsive sensor layout. */}
      {host?.trim() && payloadStatus === "up" && (
        <SystemLogSection host={host} payloadStatus={payloadStatus} />
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
const FAN_PRESETS: ReadonlyArray<{ labelKey: string; labelFallback: string; c: number; hintKey: string; hintFallback: string }> = [
  { labelKey: "hw_fan_preset_quiet", labelFallback: "Quiet", c: 55, hintKey: "hw_fan_preset_quiet_hint", hintFallback: "Fan engages earlier — cooler, louder" },
  { labelKey: "hw_fan_preset_balanced", labelFallback: "Balanced", c: 65, hintKey: "hw_fan_preset_balanced_hint", hintFallback: "Close to Sony's default" },
  {
    labelKey: "hw_fan_preset_performance",
    labelFallback: "Performance",
    c: 75,
    hintKey: "hw_fan_preset_performance_hint",
    hintFallback: "Fan ramps only under load — quieter idle",
  },
];

function FanThresholdCard({
  host,
  payloadUp,
  pinnedC,
}: {
  host: string;
  payloadUp: boolean;
  pinnedC: number;
}) {
  const tr = useTr();
  /* pinnedC comes from the payload's hw_fan_pinned_threshold() — an
   * in-process value that reflects the last threshold set this session
   * OR loaded from /data/ps5upload/fan_threshold.conf at boot. It's
   * always available (even on a basic auto-poll read) and tells us
   * whether a permanent fan speed is active. lastSetC tracks what the
   * user has set via THIS UI during this session, so the preset
   * highlight stays in sync with the user's own actions. */
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
        const addr = transferAddr(host);
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
          <Spinner size={12} tone="accent" />
        )}
      </header>

      <div className="mb-3 grid grid-cols-2 gap-2 sm:grid-cols-3">
        {FAN_PRESETS.map((p) => {
          const active = lastSetC === p.c;
          return (
            <button
              key={p.labelKey}
              type="button"
              onClick={() => applyThreshold(p.c)}
              disabled={!canSet}
              title={tr(p.hintKey, p.hintFallback)}
              className={`flex flex-col items-center gap-0.5 rounded-md border px-2 py-2 text-xs transition ${
                active
                  ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)] text-[var(--color-accent)]"
                  : "border-[var(--color-border)] hover:bg-[var(--color-surface-3)]"
              } disabled:opacity-50`}
            >
              <span className="flex items-center gap-1 font-medium">
                {active && <Check size={10} />}
                {tr(p.labelKey, p.labelFallback)}
              </span>
              <span className="font-mono tabular-nums">{p.c}°C</span>
            </button>
          );
        })}
      </div>

      <FanCurvePreview thresholdC={draftC} />

      <label className="mb-1 flex items-center justify-between text-xs text-[var(--color-muted)]">
        <span>{tr("hardware_fan_custom", "Custom")}</span>
        <span className="font-mono tabular-nums">{draftC}°C</span>
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
        ) : pinnedC > 0 ? (
          <>
            {tr("hardware_fan_active", undefined, "Permanent fan speed active at")}{" "}
            <span className="font-mono">{pinnedC}°C</span>.{" "}
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
      <div className="mb-1 flex items-center justify-between text-xs text-[var(--color-muted)]">
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
        <text x={PADDING} y={H - 1} fontSize="9" fill="var(--color-muted)">
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

/* ─── SMP metadata self-healer card ──────────────────────────────────── */

/** Off-by-default control surface for the payload's appmeta heal worker.
 *
 *  This is the "set it and forget it" companion to the per-game manual
 *  heal on the Library screen: once enabled, the PS5-side pthread
 *  re-sweeps every poll_seconds (default 30s, range 5-600) and copies
 *  any missing icon0.png / pic*.png / param.json from each game's
 *  sce_sys into /user/appmeta. Stats refresh on a slow 15s tick while
 *  the worker is running.
 *
 *  Why opt-in rather than always-on: most ps5upload sessions are
 *  transfer-only, so paying for a pthread + 30s wake-up tick on every
 *  payload boot would be wasted work. The user is the right signal —
 *  if they're seeing blank tiles, they'll come here and toggle it on. */
/* ─── System time card ───────────────────────────────────────────────── */

/**
 * PS5 clock display and sync.
 *
 * Two sources, because they fail in different ways. Internet time (NTP)
 * is the accurate one and the default — it does not inherit whatever
 * drift this PC has. Syncing to the PC is the fallback for a console on
 * a network with no route to UDP 123, where the PC's clock, drift and
 * all, still beats a console that thinks it is 2013.
 *
 * Setting the clock is deliberately behind a confirm step. It is not a
 * cosmetic setting: a far-past clock fails PSN sign-in (the TLS chain
 * is not yet valid), a far-future one fails game certificate checks,
 * and trophy timestamps are written from it.
 */
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
  /* `ps5SnapshotPcMs` captures the PC clock at the moment we received
   * `ps5Time`. The displayed PS5 time is then DERIVED as
   * `ps5Snapshot + (pcNow - ps5SnapshotPcMs)`. Without this the drift
   * reading visibly counts down by 1/sec between the 30s PS5 polls —
   * the PS5 value sits frozen while the PC value advances — which
   * reads as an unstable panel rather than a stable clock. Both are
   * quartz wall clocks, so interpolating forward is accurate to well
   * under the seconds-granularity we display. */
  const [ps5SnapshotPcMs, setPs5SnapshotPcMs] = useState<number | null>(null);
  const [pcNowMs, setPcNowMs] = useState<number>(() => Date.now());
  const [confirming, setConfirming] = useState<"ntp" | "pc" | null>(null);
  const [busy, setBusy] = useState(false);
  const [lastResult, setLastResult] = useState<PsTimeSyncJson | null>(null);
  const [error, setError] = useState<string | null>(null);

  const canSync = payloadUp && !!host.trim() && !busy;

  const refreshPs5 = useCallback(async () => {
    if (!payloadUp || !host.trim()) return;
    /* Host-stale guard: drift is PS5 time minus PC time, so attributing
     * console A's clock to console B produces a wildly wrong number the
     * user might act on. The 30s poll would resolve it, but a 30s
     * window of misleading drift is enough to mislead. */
    const probe = guardSys.capture();
    try {
      const addr = transferAddr(probe.host);
      const r = (await invoke("ps5_time_get", { addr })) as PsTimeJson;
      if (probe.isStale()) return;
      setPs5Time(r);
      setPs5SnapshotPcMs(Date.now());
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

  /* Tick the PC clock every second so drift updates live. Light enough
   * that gating it on document visibility is not worth the code. */
  useEffect(() => {
    const id = window.setInterval(() => setPcNowMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);

  const ps5DateSnapshot = psTimeToDate(ps5Time);
  const ps5DateLive =
    ps5DateSnapshot && ps5SnapshotPcMs !== null
      ? new Date(ps5DateSnapshot.getTime() + (pcNowMs - ps5SnapshotPcMs))
      : ps5DateSnapshot;
  const pcDate = new Date(pcNowMs);

  const handleSync = useCallback(
    async (source: "ntp" | "pc") => {
      if (!canSync) return;
      /* Capture the host up front so a mid-sync console switch cannot
       * render console A's result under console B's panel. */
      const probe = guardSys.capture();
      setBusy(true);
      setError(null);
      setLastResult(null);
      try {
        const addr = transferAddr(probe.host);
        /* For NTP the engine resolves the target itself — we must not
         * send this PC's clock as the target, since not trusting it is
         * the entire point of the NTP path. */
        const args =
          source === "ntp"
            ? { addr, useNtp: true }
            : { addr, targetUnixSeconds: Math.floor(Date.now() / 1000) };
        const r = (await invoke("ps5_time_sync", args)) as PsTimeSyncJson;
        if (probe.isStale()) return;
        setLastResult(r);
        setConfirming(null);
        /* Re-read so the card shows the new clock immediately rather
         * than up to 30s later. Clearing the snapshot first stops the
         * drift line interpolating from the stale pre-sync reading
         * during the round trip. */
        setPs5SnapshotPcMs(null);
        await refreshPs5();
      } catch (e) {
        if (probe.isStale()) return;
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
      }
    },
    [canSync, guardSys, refreshPs5],
  );

  const outcome = lastResult ? classifySyncResult(lastResult) : null;

  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center gap-2 text-xs uppercase tracking-wide text-[var(--color-muted)]">
        <CalendarClock size={14} />
        <span>{tr("hardware_systime_title", "System time")}</span>
      </header>

      <div className="space-y-2 text-xs">
        <StatRow
          label={tr("hardware_systime_ps5", "PS5 time")}
          value={formatUtcCompact(ps5DateLive)}
        />
        <StatRow
          label={tr("hardware_systime_pc", "Your PC")}
          value={formatUtcCompact(pcDate)}
        />
        <StatRow
          label={tr("hardware_systime_drift", "Drift")}
          value={formatDrift(ps5DateLive, pcNowMs)}
          hint={tr(
            "hardware_systime_drift_hint",
            "PS5 minus PC, in seconds. Positive = PS5 is ahead.",
          )}
        />
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        {!confirming && !busy && (
          <>
            <Button
              variant="secondary"
              size="sm"
              disabled={!canSync}
              onClick={() => setConfirming("ntp")}
            >
              <Globe size={12} />
              {tr("hardware_systime_sync_ntp", "Sync from internet time")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              disabled={!canSync}
              onClick={() => setConfirming("pc")}
            >
              {tr("hardware_systime_sync", "Sync PS5 to PC time")}
            </Button>
          </>
        )}
        {confirming && !busy && (
          <>
            <span className="text-[11px] text-[var(--color-muted)]">
              {tr(
                "hardware_systime_confirm",
                "Sets the PS5 system clock. This can affect trophies, save timestamps, and DRM checks.",
              )}
            </span>
            <Button
              variant="primary"
              size="sm"
              onClick={() => handleSync(confirming)}
            >
              {tr("hardware_systime_confirm_yes", "Set clock")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setConfirming(null)}
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

      {/* Result — success / success-via-fallback / no-op / failure */}
      {lastResult && !busy && (
        <div className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-[11px]">
          {outcome === "stub_no_op" ? (
            <div className="text-[var(--color-bad)]">
              {tr(
                "hardware_systime_stub_no_op",
                "PS5 reported success but the clock didn't actually move. Usually means the loader didn't grant the payload kernel R/W — reload via kstuff and try again.",
              )}
            </div>
          ) : outcome === "failed" ? (
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
          ) : (
            <div className="text-[var(--color-good)]">
              {tr("hardware_systime_synced", "Synced.")}{" "}
              <span className="text-[var(--color-muted)]">
                {tr("hardware_systime_via", {
                  source: formatSyncSource(
                    lastResult.source,
                    lastResult.ntp_server,
                  ),
                })}
              </span>
              {lastResult.new_unix > 0 && (
                <div className="mt-1 text-[var(--color-muted)]">
                  {tr("hardware_systime_new_label", "Now:")}{" "}
                  {formatUtcCompact(new Date(lastResult.new_unix * 1000))}
                </div>
              )}
              {outcome === "synced_fallback" && (
                <div className="mt-1 text-[var(--color-muted)]">
                  {tr(
                    "hardware_systime_fallback_note",
                    "Set directly through the system clock. Sony's Settings screen may keep showing the old time until you reopen it.",
                  )}
                </div>
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

function SmpMetaCard({
  host,
  payloadUp,
}: {
  host: string;
  payloadUp: boolean;
}) {
  const tr = useTr();
  const [stats, setStats] = useState<SmpMetaStats | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pollDraft, setPollDraft] = useState<number>(30);
  const visible = useDocumentVisible();
  const guard = useStaleHostGuard();

  const canTalk = payloadUp && !!host.trim();

  const refresh = useCallback(async () => {
    if (!canTalk) return;
    const probe = guard.capture();
    try {
      const s = await smpMetaStats(transferAddr(probe.host));
      if (probe.isStale()) return;
      setStats(s);
      setError(null);
      if (s.poll_seconds > 0) setPollDraft(s.poll_seconds);
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [canTalk, guard]);

  /* Initial fetch + slow poll. 15s cadence is well inside the 30s
   * default sweep interval, so the user sees a fresh row within one
   * sweep of any healing activity. Pauses when the tab is hidden so
   * a background window doesn't generate idle traffic. */
  useEffect(() => {
    if (!canTalk || !visible) return;
    void refresh();
    const id = setInterval(() => void refresh(), 15_000);
    return () => clearInterval(id);
  }, [canTalk, visible, refresh]);

  const start = useCallback(async () => {
    if (!canTalk || busy) return;
    const probe = guard.capture();
    setBusy(true);
    setError(null);
    try {
      const ack = await smpMetaControl(transferAddr(probe.host), "start");
      if (probe.isStale()) return;
      if (!ack.ok) {
        setError(ack.err || "start_failed");
      }
      await refresh();
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      // Always clear the local busy flag — gating it on !isStale() left the
      // panel permanently disabled when the host switched mid-RPC (busy is
      // component-local UI state; the stale guard already blocks stale
      // data/error writes above).
      setBusy(false);
    }
  }, [canTalk, busy, guard, refresh]);

  const runNow = useCallback(async () => {
    if (!canTalk || busy) return;
    const probe = guard.capture();
    setBusy(true);
    try {
      const ack = await smpMetaControl(transferAddr(probe.host), "run_now");
      if (probe.isStale()) return;
      /* Surface payload-side failure: previously this branch was
       * silently dropped, so a future payload error mode would have
       * left the user watching a spinner with no banner. Mirrors the
       * `start` action's ack handling. */
      if (!ack.ok) {
        setError(ack.err || "run_now_failed");
      }
      await refresh();
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      // Always clear the local busy flag — gating it on !isStale() left the
      // panel permanently disabled when the host switched mid-RPC (busy is
      // component-local UI state; the stale guard already blocks stale
      // data/error writes above).
      setBusy(false);
    }
  }, [canTalk, busy, guard, refresh]);

  /* Last-applied tracker prevents redundant set_poll RPCs when the
   * user clicks the slider track at the current thumb position. The
   * payload clamps + is idempotent, but pegging busy=true on every
   * click disables the rest of the panel for one round-trip and
   * spams trace logs. */
  const lastAppliedPoll = useRef<number | null>(null);

  const applyPoll = useCallback(
    async (seconds: number) => {
      if (!canTalk || busy) return;
      const clamped = Math.max(5, Math.min(600, Math.round(seconds)));
      if (lastAppliedPoll.current === clamped) return;
      const probe = guard.capture();
      setBusy(true);
      try {
        const ack = await smpMetaControl(
          transferAddr(probe.host),
          "set_poll",
          clamped,
        );
        if (probe.isStale()) return;
        if (!ack.ok) {
          setError(ack.err || "set_poll_failed");
        }
        const applied = ack.poll_seconds || clamped;
        lastAppliedPoll.current = applied;
        setPollDraft(applied);
        await refresh();
      } catch (e) {
        if (probe.isStale()) return;
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        // Always clear the local busy flag — gating it on !isStale() left the
        // whole SMP card permanently disabled when the host switched mid-RPC
        // (busy is component-local UI state; the stale guard already blocks
        // stale data/error writes above). Matches start()/runNow().
        setBusy(false);
      }
    },
    [canTalk, busy, guard, refresh],
  );

  const running = stats?.running === true;
  const lastRunLabel = stats?.last_run_unix
    ? new Date(stats.last_run_unix * 1000).toLocaleTimeString()
    : tr("smp_meta_never_run", "never");

  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center gap-2 text-xs uppercase tracking-wide text-[var(--color-muted)]">
        <ImageIcon size={14} />
        <span className="font-semibold">
          {tr("smp_meta_title", "SMP appmeta heal")}
        </span>
        {busy && (
          <Spinner size={12} tone="accent" />
        )}
        <span className="ml-auto flex items-center gap-1 text-xs font-medium">
          {running ? (
            <>
              <span className="inline-block h-1.5 w-1.5 rounded-full bg-[var(--color-good)]" />
              {tr("smp_meta_running", "running")}
            </>
          ) : (
            <>
              <span className="inline-block h-1.5 w-1.5 rounded-full bg-[var(--color-muted)]" />
              {tr("smp_meta_stopped", "off")}
            </>
          )}
        </span>
      </header>

      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "smp_meta_blurb",
          "Background worker that copies missing icons + game art from /user/app into /user/appmeta. Fixes blank home-screen tiles caused by ShadowMountPlus. Off by default.",
        )}
      </p>

      {!running ? (
        <Button
          variant="primary"
          size="sm"
          onClick={() => void start()}
          disabled={!canTalk || busy}
        >
          {tr("smp_meta_enable", "Enable")}
        </Button>
      ) : (
        <>
          <dl className="mb-3 grid grid-cols-2 gap-x-3 gap-y-1 text-xs">
            <dt className="text-[var(--color-muted)]">
              {tr("smp_meta_games", "Games")}
            </dt>
            <dd className="font-mono tabular-nums">
              {stats?.games_scanned ?? 0}
            </dd>
            <dt className="text-[var(--color-muted)]">
              {tr("smp_meta_icons", "Icons healed")}
            </dt>
            <dd className="font-mono tabular-nums">
              {stats?.icons_healed ?? 0}
            </dd>
            <dt className="text-[var(--color-muted)]">
              {tr("smp_meta_pics", "Other art")}
            </dt>
            <dd className="font-mono tabular-nums">
              {stats?.pics_healed ?? 0}
            </dd>
            <dt className="text-[var(--color-muted)]">
              {tr("smp_meta_json", "param.json")}
            </dt>
            <dd className="font-mono tabular-nums">
              {stats?.json_healed ?? 0}
            </dd>
            <dt className="text-[var(--color-muted)]">
              {tr("smp_meta_missing", "Unfixable")}
            </dt>
            <dd className="font-mono tabular-nums">
              {stats?.still_missing ?? 0}
              {stats?.last_missing && (
                <span className="ml-1 text-[var(--color-muted)]">
                  ({stats.last_missing})
                </span>
              )}
            </dd>
            <dt className="text-[var(--color-muted)]">
              {tr("smp_meta_last_run", "Last sweep")}
            </dt>
            <dd className="font-mono tabular-nums">{lastRunLabel}</dd>
          </dl>

          <label className="mb-1 flex items-center justify-between text-xs text-[var(--color-muted)]">
            <span>{tr("smp_meta_interval", "Poll interval")}</span>
            <span className="font-mono tabular-nums">{pollDraft}s</span>
          </label>
          <div className="mb-3 flex items-center gap-2">
            <input
              type="range"
              min={5}
              max={600}
              step={5}
              value={pollDraft}
              onChange={(e) => setPollDraft(Number(e.target.value))}
              /* Three commit triggers cover mouse, touch, and
               * keyboard (←/→/Page/Home/End all fire keyup). Without
               * onKeyUp, a keyboard-only user would see the slider
               * thumb move but the PS5 would keep the old interval. */
              onMouseUp={() => void applyPoll(pollDraft)}
              onTouchEnd={() => void applyPoll(pollDraft)}
              onKeyUp={() => void applyPoll(pollDraft)}
              disabled={!canTalk || busy}
              className="flex-1"
            />
          </div>

          <Button
            variant="ghost"
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={() => void runNow()}
            disabled={!canTalk || busy}
          >
            {tr("smp_meta_run_now", "Sweep now")}
          </Button>
        </>
      )}

      {error && (
        <div className="mt-2 flex items-start gap-1 text-xs text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          <span className="font-mono break-all">{error}</span>
        </div>
      )}
    </section>
  );
}

/* ─── PS5 system log (kern.msgbuf) ──────────────────────────────────────
 * Collapsible diagnostic surface. Off by default — fetching ~128 KiB of
 * raw kernel log every render is wasteful AND most users never need it.
 * When expanded: one fetch on mount + a Refresh button. Auto-scrolled
 * to the bottom so the newest output is visible immediately.
 *
 * Powered by sysctl kern.msgbuf on the PS5 (the same source userland
 * `dmesg` reads). Useful for "the payload sent but no port" / "an app
 * crash I can't see in the UI" / "why did Sony reject my register".
 */
type SystemLogSectionProps = {
  host: string | null | undefined;
  payloadStatus: ReturnType<
    typeof useConnectionStore.getState
  >["payloadStatus"];
};

function SystemLogSection({ host, payloadStatus }: SystemLogSectionProps) {
  const tr = useTr();
  const [open, setOpen] = useState(false);
  const [text, setText] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [logErr, setLogErr] = useState<string | null>(null);
  const fetchLog = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    setBusy(true);
    setLogErr(null);
    try {
      const r = await invoke<{ text?: string }>("ps5_syslog_tail", {
        addr: transferAddr(host),
      });
      setText(r.text ?? "");
    } catch (e) {
      setLogErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [host, payloadStatus]);
  useEffect(() => {
    if (!open) return;
    void fetchLog();
  }, [open, fetchLog]);
  const lineCount = text ? text.split("\n").length : 0;
  return (
    <section className="mt-6 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-1)]">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between gap-2 px-3 py-2 text-left text-sm hover:bg-[var(--color-surface-2)]"
        aria-expanded={open}
      >
        <span className="font-medium">
          {tr("hw_syslog_title", undefined, "PS5 system log (kernel)")}
        </span>
        <span className="text-xs text-[var(--color-muted)]">
          {open
            ? tr("hw_syslog_collapse", undefined, "hide")
            : tr(
                "hw_syslog_expand",
                undefined,
                "show — read kern.msgbuf for diagnostics",
              )}
        </span>
      </button>
      {open && (
        <div className="border-t border-[var(--color-border)] p-3">
          <div className="mb-2 flex items-center gap-2 text-xs text-[var(--color-muted)]">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RefreshCw size={12} />}
              onClick={() => void fetchLog()}
              disabled={busy || !host?.trim() || payloadStatus !== "up"}
            >
              {busy
                ? tr("hw_syslog_loading", undefined, "Loading…")
                : tr("refresh", undefined, "Refresh")}
            </Button>
            <span>
              {tr(
                "hw_syslog_meta",
                { lines: lineCount.toLocaleString() },
                "{lines} lines",
              )}
            </span>
          </div>
          {logErr && (
            <div className="mb-2 text-xs text-[var(--color-bad)]">{logErr}</div>
          )}
          {text ? (
            <pre className="max-h-[480px] overflow-auto rounded border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-xs font-mono leading-snug whitespace-pre-wrap break-words">
              {text}
            </pre>
          ) : (
            <div className="text-xs text-[var(--color-muted)]">
              {busy
                ? tr("hw_syslog_loading", undefined, "Loading…")
                : tr(
                    "hw_syslog_empty",
                    undefined,
                    "No data yet — click Refresh.",
                  )}
            </div>
          )}
        </div>
      )}
    </section>
  );
}
