import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { CircleStop, Download, Gauge, Play } from "lucide-react";
import {
  fetchFpsCapture,
  fetchFpsCaptureCsv,
  fetchFpsLive,
  startFpsCapture,
  stopFpsCapture,
  type FpsCapture,
  type FpsLive,
  type FpsSample,
  type FpsState,
} from "../../api/gameFps";
import { Button, Toggle } from "../../components";
import { useTr, type Translator } from "../../state/lang";
import { usePoll } from "../../lib/usePoll";
import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { isTauriEnv } from "../../lib/tauriEnv";
import { pushNotification } from "../../state/notifications";
import { humanizeCaught } from "../../lib/humanizeError";
import { formatNumber } from "../../lib/formatDate";
import {
  fpsAxisMax,
  fpsChartSegments,
  fpsCsvFileName,
  fpsGridLines,
  formatCaptureLength,
  formatFps,
  mergeSamples,
} from "./gameFps";

const LIVE_KEY = "ps5upload.hw.fps_live";
const CHART_W = 600;
const CHART_H = 140;

function stateText(tr: Translator, state: FpsState): string {
  switch (state) {
    case "no_game":
      return tr("game_fps_state_no_game", undefined, "No game picture to count. Start a game to see its frame rate.");
    case "unsupported":
      return tr(
        "game_fps_state_unsupported",
        undefined,
        "The helper on this console is too old for this. Send the helper again from Connection.",
      );
    case "offline":
      return tr("game_fps_state_offline", undefined, "The console did not answer.");
    case "no_device":
    case "failed":
      return tr("game_fps_state_failed", undefined, "This console did not give a frame count.");
    default:
      return tr("game_fps_state_starting", undefined, "Measuring…");
  }
}

/**
 * The running game's frame rate (#774): a live readout the user turns on, and an opt-in capture
 * with a graph, min / average / 1% low / max and a CSV export. The engine samples the console's
 * display frame count once a second, so every figure here is a sampled one, and says so.
 */
export default function GameFpsPanel({ host }: { host: string }) {
  const tr = useTr();
  const [liveOn, setLiveOn] = useState(() => safeGetItem(LIVE_KEY) === "1");
  const [live, setLive] = useState<FpsLive | null>(null);
  const [capture, setCapture] = useState<FpsCapture | null>(null);
  const [rows, setRows] = useState<FpsSample[]>([]);
  const [busy, setBusy] = useState(false);
  const rowsRef = useRef<FpsSample[]>([]);
  const startedRef = useRef<number | undefined>(undefined);
  const hostRef = useRef(host);

  // The Hardware screen keys this panel on the console, so a switch starts it from nothing; a
  // reply that lands after a switch is dropped by comparing with the current host.
  useEffect(() => {
    hostRef.current = host;
  }, [host]);

  const capturing = capture?.active === true || live?.capture.active === true;

  const pollLive = useCallback(async () => {
    const h = host;
    try {
      const l = await fetchFpsLive(h);
      if (hostRef.current === h) setLive(l);
    } catch {
      if (hostRef.current === h) setLive((p) => (p ? { ...p, state: "offline", fps: null } : p));
    }
  }, [host]);

  // Only the rows not seen yet; a capture started elsewhere in between is read whole.
  const pollCapture = useCallback(async () => {
    const h = host;
    try {
      const since = startedRef.current !== undefined ? rowsRef.current.length : 0;
      let c = await fetchFpsCapture(h, since);
      if (c && since > 0 && c.started_ms !== startedRef.current) c = await fetchFpsCapture(h, 0);
      if (hostRef.current !== h) return;
      if (!c) {
        startedRef.current = undefined;
        rowsRef.current = [];
        setRows([]);
        setCapture(null);
        return;
      }
      const merged = c.since === 0 ? c.samples : mergeSamples(rowsRef.current, c.since, c.samples);
      startedRef.current = c.started_ms;
      rowsRef.current = merged;
      setRows(merged);
      setCapture(c);
    } catch {
      /* the next poll tries again; the capture lives in the engine */
    }
  }, [host]);

  usePoll(pollLive, 1_000, { host, enabled: !!host && (liveOn || capturing) });
  usePoll(pollCapture, capturing ? 2_000 : null, { host, enabled: !!host, duringTransfers: true });

  function setLiveReadout(on: boolean) {
    setLiveOn(on);
    safeSetItem(LIVE_KEY, on ? "1" : "0");
    if (!on && !capturing) setLive(null);
  }

  async function toggleCapture() {
    if (busy || !host) return;
    setBusy(true);
    try {
      const l = capturing ? await stopFpsCapture(host) : await startFpsCapture(host);
      if (!capturing) {
        rowsRef.current = [];
        setRows([]);
        startedRef.current = undefined;
      }
      setLive(l);
      // read the rows (and the final ones after a stop) right away
      const c = await fetchFpsCapture(host, 0);
      if (c) {
        startedRef.current = c.started_ms;
        rowsRef.current = c.samples;
        setRows(c.samples);
      }
      setCapture(c);
    } catch (e) {
      pushNotification("error", tr("game_fps_capture_error", undefined, "Couldn't change the capture"), {
        body: humanizeCaught(e),
      });
    } finally {
      setBusy(false);
    }
  }

  async function exportCsv() {
    if (!host || !capture) return;
    const fileName = fpsCsvFileName(capture.started_ms);
    try {
      const csv = await fetchFpsCaptureCsv(host);
      if (!isTauriEnv()) {
        const { browserDownloadText } = await import("../../lib/browserDownload");
        browserDownloadText(fileName, csv, "text/csv");
      } else {
        const { pickSaveDest } = await import("../../lib/pickSaveDest");
        const { writeTextFileToPath } = await import("../../lib/saveTextFile");
        const dest = await pickSaveDest({
          defaultPath: fileName,
          filters: [{ name: "CSV", extensions: ["csv"] }],
        });
        if (!dest || typeof dest !== "string") return;
        await writeTextFileToPath(dest, csv, fileName);
      }
      pushNotification("success", tr("game_fps_exported", undefined, "Capture exported"));
    } catch (e) {
      pushNotification("error", tr("game_fps_export_failed", undefined, "Couldn't export the capture"), {
        body: humanizeCaught(e),
      });
    }
  }

  const stats = capture?.stats;
  const top = useMemo(() => fpsAxisMax(Math.max(60, ...rows.map((r) => r.fps))), [rows]);
  const segments = useMemo(() => fpsChartSegments(rows, CHART_W, CHART_H, top), [rows, top]);
  const showLive = liveOn || capturing;
  const fps = live?.fps ?? null;

  return (
    <section className="surface-panel min-w-0 p-5 sm:p-6">
      <header className="mb-4 flex items-center gap-2.5">
        <span className="icon-disc" aria-hidden>
          <Gauge size={14} />
        </span>
        <h3 className="flex-1 text-sm font-semibold">{tr("game_fps_title", undefined, "Game frame rate")}</h3>
      </header>

      <Toggle
        checked={liveOn}
        onChange={setLiveReadout}
        label={tr("game_fps_live", undefined, "Live readout")}
        hint={tr("game_fps_live_hint", undefined, "Shows the frame rate here while you play.")}
      />

      {showLive && (
        <div className="mt-4 flex items-baseline gap-2" aria-live="polite">
          {fps !== null ? (
            <>
              <span className="text-4xl font-semibold tabular-nums">{formatFps(fps)}</span>
              <span className="text-xs text-[var(--color-muted)]">{tr("game_fps_unit", undefined, "FPS (sampled)")}</span>
            </>
          ) : (
            <span className="text-xs text-[var(--color-muted)]">{stateText(tr, live?.state ?? "starting")}</span>
          )}
        </div>
      )}

      <div className="mt-4 flex flex-wrap items-center gap-2">
        <Button
          variant={capturing ? "danger" : "primary"}
          size="sm"
          leftIcon={capturing ? <CircleStop size={12} /> : <Play size={12} />}
          onClick={() => void toggleCapture()}
          disabled={busy || !host}
          data-testid="game-fps-capture-toggle"
        >
          {capturing
            ? tr("game_fps_capture_stop", undefined, "Stop capture")
            : tr("game_fps_capture_start", undefined, "Start capture")}
        </Button>
        {/* While a capture runs: a word as well as the red button, so the state never rests on colour (#774). */}
        {capturing && (
          <span
            role="status"
            className="inline-flex items-center gap-1.5 text-xs font-semibold text-[var(--color-bad)]"
            data-testid="game-fps-recording"
          >
            <span aria-hidden className="h-2 w-2 animate-pulse rounded-full bg-[var(--color-bad)] motion-reduce:animate-none" />
            {tr("game_fps_recording", undefined, "Recording")}
          </span>
        )}
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<Download size={12} />}
          onClick={() => void exportCsv()}
          disabled={!capture || rows.length === 0}
        >
          {tr("game_fps_export_csv", undefined, "Export CSV")}
        </Button>
        {capture && (
          <span className="text-xs text-[var(--color-muted)]">
            {capture.active
              ? tr("game_fps_capturing", { count: formatNumber(capture.total) }, "Capturing: {count} samples")
              : tr("game_fps_capture_done", { count: formatNumber(capture.total) }, "Last capture: {count} samples")}
          </span>
        )}
      </div>

      {capture && (
        <>
          <svg
            className="mt-3 w-full rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)]"
            viewBox={`-28 -8 ${CHART_W + 36} ${CHART_H + 16}`}
            role="img"
            aria-label={tr("game_fps_graph_label", undefined, "Frame rate over the capture")}
          >
            {fpsGridLines(top).map((v) => {
              const y = CHART_H - (v / top) * CHART_H;
              return (
                <g key={v}>
                  <line x1={0} x2={CHART_W} y1={y} y2={y} stroke="var(--color-border)" strokeWidth={1} />
                  <text x={-6} y={y + 4} textAnchor="end" fontSize={11} fill="var(--color-muted)">
                    {v}
                  </text>
                </g>
              );
            })}
            <line x1={0} x2={CHART_W} y1={CHART_H} y2={CHART_H} stroke="var(--color-border-strong)" strokeWidth={1} />
            {segments.map((pts, i) => (
              <polyline
                key={i}
                points={pts}
                fill="none"
                stroke="var(--color-accent)"
                strokeWidth={1.5}
                strokeLinejoin="round"
                vectorEffect="non-scaling-stroke"
              />
            ))}
          </svg>
          {rows.length === 0 && (
            <p className="mt-2 text-xs text-[var(--color-muted)]">{stateText(tr, live?.state ?? "starting")}</p>
          )}
          {stats && stats.samples > 0 && (
            <dl className="mt-3 grid grid-cols-3 gap-x-4 gap-y-1 text-xs sm:grid-cols-5">
              <Stat label={tr("game_fps_stat_min", undefined, "Min")} value={formatFps(stats.min_fps)} />
              <Stat label={tr("game_fps_stat_avg", undefined, "Average")} value={formatFps(stats.avg_fps, 1)} />
              <Stat label={tr("game_fps_stat_low", undefined, "1% low")} value={formatFps(stats.low_1pct_fps, 1)} />
              <Stat label={tr("game_fps_stat_max", undefined, "Max")} value={formatFps(stats.max_fps)} />
              <Stat label={tr("game_fps_stat_time", undefined, "Length")} value={formatCaptureLength(stats.seconds)} />
            </dl>
          )}
          {capture.stop_reason === "limit" && (
            <p className="mt-2 text-xs text-[var(--color-warn)]">
              {tr("game_fps_capture_limit", undefined, "The capture stopped at its six-hour limit.")}
            </p>
          )}
        </>
      )}

      <p className="mt-3 text-xs text-[var(--color-muted)]">
        {tr(
          "game_fps_explainer",
          undefined,
          "Sampled once a second from the console's display: the frames it showed in each second, not the time of single frames. The 1% low is the average of the slowest 1% of seconds. A capture also notes the console's temperatures every 5 seconds.",
        )}
      </p>
      <p className="mt-2 text-xs text-[var(--color-muted)]">
        {tr(
          "game_fps_tv_note",
          undefined,
          "Shown here, not on the TV: drawing over a game means changing the PS5's system screen, which can crash the console.",
        )}
      </p>
    </section>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0">
      <dt className="text-[var(--color-muted)]">{label}</dt>
      <dd className="font-semibold tabular-nums">{value}</dd>
    </div>
  );
}
