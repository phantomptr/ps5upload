import { useCallback, useEffect, useState } from "react";
import { Fan } from "lucide-react";

import { Spinner, Toggle } from "../../../components";
import { fetchHwTemps } from "../../../api/ps5";
import { transferAddr } from "../../../lib/addr";
import { getEngineUrl } from "../../../state/engine";
import { useTr } from "../../../state/lang";
import { useSensors, useTempsView } from "../../../state/sensors";
import { TempsNote } from "../../../components/TempsNote";
import { formatDate } from "../../../lib/formatDate";
import { humanizeCaught } from "../../../lib/humanizeError";

/** The engine's fan boost for a console (fan_boost.rs). */
export interface FanBoost {
  on: boolean;
  until_ms?: number;
  boost_c: number;
}

export function readFanBoost(v: unknown): FanBoost {
  const o = (v ?? {}) as Record<string, unknown>;
  return {
    on: o.on === true,
    until_ms: typeof o.until_ms === "number" ? o.until_ms : undefined,
    boost_c: typeof o.boost_c === "number" ? o.boost_c : 55,
  };
}

async function boostCall(addr: string, on?: boolean): Promise<FanBoost> {
  const r = await fetch(
    on === undefined
      ? `${getEngineUrl()}/api/ps5/fan-boost?addr=${encodeURIComponent(addr)}`
      : `${getEngineUrl()}/api/ps5/fan-boost`,
    on === undefined
      ? undefined
      : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ addr, on }) },
  );
  const body = (await r.json().catch(() => ({}))) as { error?: string };
  if (!r.ok) throw new Error(body.error || `HTTP ${r.status}`);
  return readFanBoost(body);
}

const clock = (ms: number) => formatDate(ms, "time-short");

/** The Console tab: temperatures (the shared 5 s reader, only while open) and fan boost. The
 *  fan's speed needs an extended read, so it is read only when asked. */
export function ConsoleTab({ host }: { host: string | null }) {
  const tr = useTr();
  useSensors(host);
  // The last reading that worked: one failed read keeps it on screen.
  const tempsNow = useTempsView(host);
  const t = tempsNow.temps;
  const addr = host ? transferAddr(host) : "";
  const [boost, setBoost] = useState<FanBoost | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [duty, setDuty] = useState<number | null>(null);
  const [dutyBusy, setDutyBusy] = useState(false);

  useEffect(() => {
    if (!addr) return;
    let live = true;
    boostCall(addr)
      .then((b) => live && setBoost(b))
      .catch(() => live && setBoost({ on: false, boost_c: 55 }));
    return () => {
      live = false;
    };
  }, [addr]);

  const setOn = useCallback(
    async (on: boolean) => {
      setBusy(true);
      setError(null);
      try {
        setBoost(await boostCall(addr, on));
      } catch (e) {
        setError(humanizeCaught(e));
      } finally {
        setBusy(false);
      }
    },
    [addr],
  );

  const readDuty = async () => {
    setDutyBusy(true);
    try {
      const x = await fetchHwTemps(addr, true);
      setDuty(x.fan_duty_pct >= 0 ? x.fan_duty_pct : null);
    } catch {
      setDuty(null);
    } finally {
      setDutyBusy(false);
    }
  };

  const row = (label: string, v: number | null | undefined, testId: string) => (
    <div className="flex items-center justify-between py-1" data-testid={testId}>
      <span className="text-white/70">{label}</span>
      <span className="font-mono tabular-nums">{v && v > 0 ? `${v} °C` : "—"}</span>
    </div>
  );

  return (
    <div className="space-y-4 py-1" data-testid="rp-game-console">
      <section>
        {!t ? (
          tempsNow.note === "reading" ? (
            <div className="flex justify-center py-3">
              <Spinner size={16} />
            </div>
          ) : (
            <TempsNote view={tempsNow} />
          )
        ) : (
          <>
            {row(tr("rp_console_cpu", undefined, "CPU"), t.cpu_temp, "rp-console-cpu")}
            {row(tr("rp_console_soc", undefined, "System chip"), t.soc_temp, "rp-console-soc")}
            {t.m2_temp >= 20 && row(tr("rp_console_ssd", undefined, "Expansion SSD"), t.m2_temp, "rp-console-ssd")}
            <div className="flex items-center justify-between py-1">
              <span className="text-white/70">{tr("rp_console_fan_target", undefined, "Fan works to")}</span>
              <span className={t.fan_pinned_c > 0 ? "font-mono tabular-nums" : ""} data-testid="rp-console-fan-target">
                {t.fan_pinned_c > 0
                  ? `${t.fan_pinned_c} °C`
                  : tr("rp_console_fan_own", undefined, "the console's own setting")}
              </span>
            </div>
            <div className="flex items-center justify-between py-1">
              <span className="text-white/70">{tr("rp_console_fan_speed", undefined, "Fan speed")}</span>
              {duty !== null ? (
                <span className="font-mono tabular-nums">{duty}%</span>
              ) : (
                <button
                  type="button"
                  disabled={dutyBusy}
                  onClick={() => void readDuty()}
                  className="h-7 rounded-full bg-white/10 px-3 text-xs hover:bg-white/20 disabled:opacity-50"
                  data-testid="rp-console-fan-read"
                >
                  {tr("rp_console_fan_read", undefined, "Read")}
                </button>
              )}
            </div>
            <TempsNote view={tempsNow} />
          </>
        )}
      </section>
      <section className="space-y-1">
        <div className="flex items-center gap-2">
          <Fan size={16} className="shrink-0 text-white/70" aria-hidden />
          <Toggle
            className="min-w-0 flex-1"
            checked={!!boost?.on}
            disabled={busy || !boost || !addr}
            onChange={(v) => void setOn(v)}
            label={tr("rp_console_boost", undefined, "Fan boost")}
            hint={
              boost?.on && boost.until_ms
                ? tr(
                    "rp_console_boost_until",
                    { c: boost.boost_c, time: clock(boost.until_ms) },
                    `Keeping it at ${boost.boost_c} °C until ${clock(boost.until_ms)}, or until the stream ends.`,
                  )
                : tr(
                    "rp_console_boost_hint",
                    { c: boost?.boost_c ?? 55 },
                    `Louder and cooler for 30 minutes: the fan works to keep the console at ${boost?.boost_c ?? 55} °C. It goes back by itself when the stream ends.`,
                  )
            }
          />
        </div>
        {error && (
          <p className="text-xs text-amber-300" role="alert" data-testid="rp-console-boost-error">
            {error}
          </p>
        )}
      </section>
    </div>
  );
}
