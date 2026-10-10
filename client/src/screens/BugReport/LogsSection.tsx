import { useState } from "react";

import { useTr } from "../../state/lang";
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
    </div>
  );
}
