import { useState } from "react";

import { useTr } from "../../state/lang";
import { raiseActive, useDiagSettingsStore } from "../../state/diagSettings";
import { raiseAllForReproducing, stopRaising } from "../../lib/logLevels";
import { formatDate } from "../../lib/formatDate";
import { Button } from "../../components";
import { RANGE_KEYS, type RangeKey, type ReportDraft } from "./draft";
import Chips from "./Chips";

/** A datetime-local value for `ms` in this machine's time zone. */
function toLocalInput(ms: number): string {
  const d = new Date(ms - new Date().getTimezoneOffset() * 60_000);
  return d.toISOString().slice(0, 16);
}

/** How far back the logs go. Everything recorded in that window goes in, from every source. */
export default function LogsSection({
  draft,
  update,
}: {
  draft: ReportDraft;
  update: (p: Partial<ReportDraft>) => void;
}) {
  const tr = useTr();
  const [latest] = useState(() => toLocalInput(Date.now()));
  const label: Record<RangeKey, string> = {
    "15m": tr("br_range_15m", undefined, "Last 15 minutes"),
    "1h": tr("br_range_1h", undefined, "Last hour"),
    "6h": tr("br_range_6h", undefined, "Last 6 hours"),
    "24h": tr("br_range_24h", undefined, "Last 24 hours"),
    "3d": tr("br_range_3d", undefined, "Last 3 days"),
    "7d": tr("br_range_7d", undefined, "Last 7 days"),
    custom: tr("br_range_custom", undefined, "From a time you choose"),
  };

  return (
    <div className="grid gap-3">
      <Chips
        label={tr("br_range_title", undefined, "When did it happen? The logs cover from then until now.")}
        value={draft.rangeKey}
        options={RANGE_KEYS.map((k) => ({ value: k, label: label[k] }))}
        onChange={(rangeKey) => update({ rangeKey })}
      />
      {draft.rangeKey === "custom" && (
        <label className="grid max-w-xs gap-1 text-xs font-medium">
          {tr("br_range_from", undefined, "From")}
          <input
            type="datetime-local"
            value={draft.customStart ? toLocalInput(draft.customStart) : ""}
            max={latest}
            onChange={(e) => update({ customStart: e.target.value ? new Date(e.target.value).getTime() : null })}
            className="rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3 py-2 text-sm font-normal text-[var(--color-text)]"
          />
        </label>
      )}
      <p className="text-xs leading-relaxed text-[var(--color-muted)]">
        {tr(
          "br_logs_hint",
          undefined,
          "Everything the app, the engine and the PS5 recorded in that time goes in: connections, the helper's log, installs, transfers, errors and crashes. IP addresses, serials, home folders and keys are removed.",
        )}
      </p>
      <ReproduceHelper />
    </div>
  );
}

/** "Record more while you reproduce it": everything at Debug for 30 minutes, then make the
 *  problem happen again and create the report. The report says which levels were in force. */
function ReproduceHelper() {
  const tr = useTr();
  const engine = useDiagSettingsStore((s) => s.engine);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const raised = raiseActive(engine);

  const run = async (work: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="grid gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-3 text-xs"
      data-testid="br-reproduce"
    >
      {raised && engine?.until_ms ? (
        <>
          <p className="leading-relaxed">
            {tr(
              "br_repro_on",
              { time: formatDate(engine.until_ms) },
              "Recording more detail until {time}. Make the problem happen again now, then come back here and create the report.",
            )}
          </p>
          <div>
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(() => stopRaising())}>
              {tr("br_repro_stop", undefined, "Stop recording more")}
            </Button>
          </div>
        </>
      ) : (
        <>
          <p className="leading-relaxed text-[var(--color-muted)]">
            {tr(
              "br_repro_hint",
              undefined,
              "Hard to catch? Record more detail in the app, the engine and the PS5 helper for 30 minutes, make the problem happen again, then create the report.",
            )}
          </p>
          <div>
            <Button size="sm" variant="secondary" loading={busy} disabled={busy} onClick={() => void run(() => raiseAllForReproducing(30))}>
              {tr("br_repro_button", undefined, "Record more for 30 minutes")}
            </Button>
          </div>
        </>
      )}
      {error && (
        <p className="text-[var(--color-bad)]">
          {tr("br_repro_failed", { error }, "Couldn't change the log level: {error}")}
        </p>
      )}
    </div>
  );
}
