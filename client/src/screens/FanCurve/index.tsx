import { useCallback, useEffect, useState } from "react";
import { Fan, Plus, Trash2, Check, Activity, Info } from "lucide-react";
import {
  PageHeader,
  Button,
  ErrorCard,
  ConnectionGate,
  Spinner,
} from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { useSensors } from "../../state/sensors";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { transferAddr } from "../../lib/addr";
import { fanCurveSet, fanCurveGet, type FanCurvePoint } from "../../api/ps5";
import { humanizePs5Error } from "../../lib/humanizeError";

/**
 * Duty the curve prescribes at `t` degrees.
 *
 * Linear between the two surrounding points and flat outside the ends —
 * matching how the payload applies the curve, so the "you are here" marker
 * cannot disagree with what the console is actually doing. `points` must be
 * sorted by temperature. Exported so that agreement is a test, not a claim.
 */
export function dutyAtTemp(
  points: readonly FanCurvePoint[],
  t: number,
): number {
  if (points.length === 0) return 0;
  if (t <= points[0].temp_c) return points[0].duty_pct;
  const last = points[points.length - 1];
  if (t >= last.temp_c) return last.duty_pct;
  for (let i = 0; i < points.length - 1; i++) {
    const a = points[i];
    const b = points[i + 1];
    if (t >= a.temp_c && t <= b.temp_c) {
      const span = b.temp_c - a.temp_c;
      // Two points at the same temperature: take the later one rather than
      // dividing by zero.
      if (span <= 0) return b.duty_pct;
      const k = (t - a.temp_c) / span;
      return Math.round(a.duty_pct + k * (b.duty_pct - a.duty_pct));
    }
  }
  return last.duty_pct;
}

/** Named starting points. Seeds the editor only — every point stays editable.
 *  The common ask is "quieter" or "cooler", not a specific duty at a specific
 *  degree, which is what the four number fields alone forced you to think in. */
const PRESETS: ReadonlyArray<{
  id: string;
  key: string;
  fallback: string;
  points: FanCurvePoint[];
}> = [
  {
    id: "quiet",
    key: "fanCurve_preset_quiet",
    fallback: "Quiet",
    points: [
      { temp_c: 55, duty_pct: 20 },
      { temp_c: 70, duty_pct: 40 },
      { temp_c: 80, duty_pct: 70 },
      { temp_c: 90, duty_pct: 100 },
    ],
  },
  {
    id: "balanced",
    key: "fanCurve_preset_balanced",
    fallback: "Balanced",
    points: [
      { temp_c: 50, duty_pct: 30 },
      { temp_c: 65, duty_pct: 55 },
      { temp_c: 75, duty_pct: 80 },
      { temp_c: 85, duty_pct: 100 },
    ],
  },
  {
    id: "cool",
    key: "fanCurve_preset_cool",
    fallback: "Cool",
    points: [
      { temp_c: 45, duty_pct: 45 },
      { temp_c: 60, duty_pct: 70 },
      { temp_c: 70, duty_pct: 90 },
      { temp_c: 80, duty_pct: 100 },
    ],
  },
];

const DEFAULT_POINTS: FanCurvePoint[] = [
  { temp_c: 50, duty_pct: 30 },
  { temp_c: 65, duty_pct: 55 },
  { temp_c: 75, duty_pct: 80 },
  { temp_c: 85, duty_pct: 100 },
];

export default function FanCurveScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";
  const { confirm, dialog: confirmDialog } = useConfirm();
  const guard = useStaleHostGuard();
  // Live CPU temperature for the "you are here" marker. The sensors store
  // already polls while a subscriber is mounted, and pauses when the payload is
  // down or a transfer owns the console — so this costs nothing beyond the time
  // this screen is open. Null when unavailable: the marker is then not drawn at
  // all rather than parked at a guessed position.
  const { sample } = useSensors(host ?? "");
  const liveTemp = sample?.temps?.cpu_temp ?? null;

  const [points, setPoints] = useState<FanCurvePoint[]>(DEFAULT_POINTS);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [applied, setApplied] = useState(false);
  const [hasSavedCurve, setHasSavedCurve] = useState(false);

  // Load persisted curve on mount / addr change
  useEffect(() => {
    if (!addr || payloadStatus !== "up") {
      setLoading(false);
      return;
    }
    const probe = guard.capture();
    setLoading(true);
    setError(null);
    fanCurveGet(addr)
      .then((res) => {
        if (probe.isStale()) return;
        const pts = res.points;
        if (pts && pts.length > 0) {
          setPoints(pts);
          setHasSavedCurve(true);
        }
      })
      .catch(() => {
        // Non-fatal — the curve just isn't persisted yet
      })
      .finally(() => {
        if (!probe.isStale()) setLoading(false);
      });
  }, [addr, payloadStatus, guard]);

  const sorted = [...points].sort((a, b) => a.temp_c - b.temp_c);

  const updatePoint = useCallback((idx: number, patch: Partial<FanCurvePoint>) => {
    setPoints((prev) =>
      prev.map((p, i) => (i === idx ? { ...p, ...patch } : p)),
    );
    setApplied(false);
  }, []);

  const removePoint = useCallback((idx: number) => {
    setPoints((prev) => prev.filter((_, i) => i !== idx));
    setApplied(false);
  }, []);

  const addPoint = useCallback(() => {
    setPoints((prev) => [...prev, { temp_c: 70, duty_pct: 60 }]);
    setApplied(false);
  }, []);

  const handleApply = useCallback(async () => {
    if (!addr) return;
    const ok = await confirm({
      title: tr("fanCurve_confirm_title", undefined, "Apply fan curve?"),
      message: tr(
        "fanCurve_confirm_msg",
        undefined,
        "This overrides the PS5's built-in fan control. Incorrect settings may cause overheating. The first point's temperature becomes the persistent fan threshold — it survives payload redeploy and console reboot.",
      ),
      confirmLabel: tr("fanCurve_apply", undefined, "Apply"),
      destructive: true,
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    const probe = guard.capture();
    try {
      await fanCurveSet(sorted, addr);
      if (probe.isStale()) return;
      setApplied(true);
      setHasSavedCurve(true);
    } catch (e) {
      if (probe.isStale()) return;
      setError(humanizePs5Error(String(e)));
    } finally {
      setBusy(false);
    }
  }, [addr, confirm, tr, sorted, guard]);

  // SVG preview
  // Drawn in a fixed viewBox and scaled by CSS, so the graph fills whatever
  // width it is given instead of staying a 320px thumbnail on a 27" monitor.
  // Left/bottom padding is larger than the rest to make room for real axis
  // labels — the old uniform 24px had nowhere to put them.
  const W = 640;
  const H = 260;
  const PAD_L = 44;
  const PAD_R = 16;
  const PAD_T = 18;
  const PAD_B = 34;
  const tempMin = 30;
  const tempMax = 95;
  const xFor = (t: number) =>
    PAD_L + ((t - tempMin) / (tempMax - tempMin)) * (W - PAD_L - PAD_R);
  const yFor = (p: number) => PAD_T + (1 - p / 100) * (H - PAD_T - PAD_B);
  /** Curve plus the two baseline corners, so the area under it can be filled. */
  const areaPath =
    sorted.length > 1
      ? `M ${xFor(sorted[0].temp_c)},${yFor(0)} ` +
        sorted.map((p) => `L ${xFor(p.temp_c)},${yFor(p.duty_pct)}`).join(" ") +
        ` L ${xFor(sorted[sorted.length - 1].temp_c)},${yFor(0)} Z`
      : "";
  const polyPath = sorted
    .map((p) => `${xFor(p.temp_c).toFixed(1)},${yFor(p.duty_pct).toFixed(1)}`)
    .join(" ");

  return (
    <div className="mx-auto max-w-4xl space-y-4 p-4">
      <PageHeader
        icon={Fan}
        title={tr("fanCurve_title", undefined, "Fan Curve")}
        description={tr(
          "fanCurve_subtitle",
          undefined,
          "Define a custom fan duty curve by temperature. Persists across reboots.",
        )}
      />

      <ConnectionGate>
        {error && <ErrorCard title={error} />}

        {applied && (
          <div className="rounded-lg border border-[var(--color-good)] bg-[var(--color-good-soft)] px-4 py-3 text-sm text-[var(--color-good)]">
            <div className="flex items-center gap-2 font-medium">
              <Check size={14} />
              {tr("fanCurve_applied", undefined, "Fan curve applied and saved")}
            </div>
          </div>
        )}

        {/* Persistence info banner */}
        {hasSavedCurve && !applied && (
          <div className="flex items-start gap-2 rounded-lg border border-[var(--color-accent)] bg-[var(--color-surface-2)] px-4 py-3 text-sm text-[var(--color-text)]">
            <Info size={14} className="mt-0.5 shrink-0 text-[var(--color-accent)]" />
            <span>
              {tr(
                "fanCurve_persisted",
                undefined,
                "A fan curve is saved on this PS5. The first point's temperature is the active fan threshold and auto-restores on every payload load — no desktop app needed.",
              )}
            </span>
          </div>
        )}

        {loading ? (
          <div className="flex items-center justify-center py-12">
            <Spinner size={32} />
          </div>
        ) : (
          <>
            {/* Visual preview */}
            <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
              <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
                <h3 className="flex items-center gap-2 text-sm font-medium text-[var(--color-text)]">
                  <Activity size={16} />
                  {tr("fanCurve_preview", undefined, "Curve preview")}
                </h3>
                <div className="flex flex-wrap items-center gap-1.5">
                  <span className="text-xs text-[var(--color-muted)]">
                    {tr("fanCurve_presets", undefined, "Presets")}
                  </span>
                  {PRESETS.map((preset) => (
                    <Button
                      key={preset.id}
                      variant="ghost"
                      size="sm"
                      disabled={busy}
                      onClick={() => {
                        setPoints(preset.points.map((pt) => ({ ...pt })));
                        setApplied(false);
                      }}
                    >
                      {tr(preset.key, undefined, preset.fallback)}
                    </Button>
                  ))}
                </div>
              </div>
              <svg
                viewBox={`0 0 ${W} ${H}`}
                className="block h-auto w-full"
                role="img"
                aria-label={tr(
                  "fanCurve_preview_aria",
                  undefined,
                  "Fan duty against temperature",
                )}
              >
                {/* Duty gridlines every 25%, each labelled. The old chart drew
                    three unlabelled dashes: the shape was visible but you could
                    not read a value off it. */}
                {[0, 25, 50, 75, 100].map((p) => (
                  <g key={p}>
                    <line
                      x1={PAD_L}
                      y1={yFor(p)}
                      x2={W - PAD_R}
                      y2={yFor(p)}
                      stroke="var(--color-border)"
                      strokeDasharray={p === 0 ? undefined : "2 4"}
                    />
                    <text
                      x={PAD_L - 8}
                      y={yFor(p) + 3}
                      fontSize="10"
                      textAnchor="end"
                      fill="var(--color-muted)"
                    >
                      {p}%
                    </text>
                  </g>
                ))}
                {[30, 40, 50, 60, 70, 80, 90].map((t) => (
                  <text
                    key={t}
                    x={xFor(t)}
                    y={H - PAD_B + 16}
                    fontSize="10"
                    textAnchor="middle"
                    fill="var(--color-muted)"
                  >
                    {t}°
                  </text>
                ))}
                {areaPath && (
                  <path d={areaPath} fill="var(--color-accent)" opacity={0.12} />
                )}
                {sorted.length > 1 && (
                  <polyline
                    points={polyPath}
                    fill="none"
                    stroke="var(--color-accent)"
                    strokeWidth={2.5}
                    strokeLinejoin="round"
                    strokeLinecap="round"
                  />
                )}
                {/* Where the console is RIGHT NOW — the difference between a
                    shape and a decision, since it shows which part of the curve
                    is actually in use. Drawn only for a real in-range reading. */}
                {liveTemp !== null &&
                  liveTemp >= tempMin &&
                  liveTemp <= tempMax && (
                    <g>
                      <line
                        x1={xFor(liveTemp)}
                        y1={PAD_T}
                        x2={xFor(liveTemp)}
                        y2={yFor(0)}
                        stroke="var(--color-good)"
                        strokeWidth={1.5}
                        strokeDasharray="3 3"
                      />
                      <circle
                        cx={xFor(liveTemp)}
                        cy={yFor(dutyAtTemp(sorted, liveTemp))}
                        r={5}
                        fill="var(--color-good)"
                      />
                      <text
                        x={xFor(liveTemp)}
                        y={PAD_T - 5}
                        fontSize="10"
                        textAnchor="middle"
                        fill="var(--color-good)"
                      >
                        {liveTemp}° → {dutyAtTemp(sorted, liveTemp)}%
                      </text>
                    </g>
                  )}
                {sorted.map((p, i) => (
                  <circle
                    key={i}
                    cx={xFor(p.temp_c)}
                    cy={yFor(p.duty_pct)}
                    r={4.5}
                    fill="var(--color-surface-2)"
                    stroke="var(--color-accent)"
                    strokeWidth={2.5}
                  />
                ))}
              </svg>
            </div>

            {/* Point editor */}
            <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
              <div className="mb-3 flex items-center justify-between">
                <h3 className="flex items-center gap-2 text-sm font-medium text-[var(--color-text)]">
                  <Fan size={16} />
                  {tr("fanCurve_points", undefined, "Curve points")}
                </h3>
                <Button variant="ghost" size="sm" onClick={addPoint} disabled={busy}>
                  <Plus size={14} />
                  {tr("fanCurve_add", undefined, "Add")}
                </Button>
              </div>

              <div className="space-y-2">
                <div className="grid grid-cols-[1fr_1fr_auto] gap-2 px-1 text-xs text-[var(--color-muted)]">
                  <span>{tr("fanCurve_temp", undefined, "Temp (°C)")}</span>
                  <span>{tr("fanCurve_duty", undefined, "Duty (%)")}</span>
                  <span />
                </div>
                {points.map((p, i) => (
                  <div
                    key={i}
                    className="grid grid-cols-[1fr_1fr_auto] items-center gap-2"
                  >
                    <input
                      type="number"
                      value={p.temp_c}
                      min={0}
                      max={100}
                      inputMode="numeric"
                      onChange={(e) =>
                        updatePoint(i, { temp_c: Number(e.target.value) })
                      }
                      className="w-full rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm text-[var(--color-text)] focus:border-[var(--color-accent)] focus:outline-none"
                    />
                    <input
                      type="number"
                      value={p.duty_pct}
                      min={0}
                      max={100}
                      inputMode="numeric"
                      onChange={(e) =>
                        updatePoint(i, { duty_pct: Number(e.target.value) })
                      }
                      className="w-full rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm text-[var(--color-text)] focus:border-[var(--color-accent)] focus:outline-none"
                    />
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => removePoint(i)}
                      disabled={busy || points.length <= 1}
                      className="text-[var(--color-bad)]"
                    >
                      <Trash2 size={14} />
                    </Button>
                  </div>
                ))}
              </div>

              <div className="mt-4">
                <Button
                  variant="primary"
                  size="md"
                  onClick={handleApply}
                  disabled={
                    busy || points.length === 0 || payloadStatus !== "up" || !addr
                  }
                >
                  {busy ? (
                    <Spinner size={14} tone="inherit" />
                  ) : (
                    <Fan size={14} />
                  )}
                  {tr("fanCurve_apply", undefined, "Apply")}
                </Button>
              </div>
            </div>
          </>
        )}
      </ConnectionGate>
      {confirmDialog}
    </div>
  );
}
