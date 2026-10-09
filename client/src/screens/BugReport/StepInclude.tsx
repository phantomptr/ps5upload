import { useState } from "react";

import { Checkbox, Select } from "../../components";
import { useTr } from "../../state/lang";
import { EVENT_CATS, type EventCat } from "../../lib/eventRecord";
import { SOURCE_IDS, type SourceId } from "../../lib/reportBuilder";
import { isTauriEnv } from "../../lib/tauriEnv";
import { RANGE_KEYS, type RangeKey, type WizardDraft } from "./draft";

/** A datetime-local value for `ms` in this machine's time zone. */
function toLocalInput(ms: number): string {
  const d = new Date(ms - new Date().getTimezoneOffset() * 60_000);
  return d.toISOString().slice(0, 16);
}

/** Step 3: the time range (from when until now), and what goes in. Everything is the default. */
export default function StepInclude({
  draft,
  update,
}: {
  draft: WizardDraft;
  update: (p: Partial<WizardDraft>) => void;
}) {
  const tr = useTr();
  const [latest] = useState(() => toLocalInput(Date.now()));
  const rangeLabel: Record<RangeKey, string> = {
    "15m": tr("br_range_15m", undefined, "Last 15 minutes"),
    "1h": tr("br_range_1h", undefined, "Last hour"),
    "6h": tr("br_range_6h", undefined, "Last 6 hours"),
    "24h": tr("br_range_24h", undefined, "Last 24 hours"),
    "3d": tr("br_range_3d", undefined, "Last 3 days"),
    "7d": tr("br_range_7d", undefined, "Last 7 days"),
    custom: tr("br_range_custom", undefined, "From a time you choose"),
  };
  const catLabel: Record<EventCat, string> = {
    connection: tr("br_cat_connection", undefined, "Connections (connect, lost, reconnect, pairing)"),
    helper: tr("br_cat_helper", undefined, "PS5 helper (started, stopped, crashed, its log)"),
    transfer: tr("br_cat_transfer", undefined, "Uploads and copies"),
    install: tr("br_cat_install", undefined, "Installs"),
    api: tr("br_cat_api", undefined, "Failed engine calls"),
    app: tr("br_cat_app", undefined, "App errors and crashes"),
    system: tr("br_cat_system", undefined, "App and engine start, settings"),
  };
  const sourceLabel: Record<SourceId, string> = {
    app_journal: tr("br_src_app_journal", undefined, "App events"),
    engine_journal: tr("br_src_engine_journal", undefined, "Engine events"),
    engine_log: tr("br_src_engine_log", undefined, "Engine log"),
    app_log: tr("br_src_app_log", undefined, "App log"),
    crash_reports: tr("br_src_crash_reports", undefined, "Crash reports (desktop app)"),
    helper_log: tr("br_src_helper_log", undefined, "PS5 helper log (read now)"),
    helper_ftp: tr("br_src_helper_ftp", undefined, "PS5 helper log over FTP if the helper doesn't answer (port 2121)"),
    console_logs: tr("br_src_console_logs", undefined, "PS5 kernel log and system log"),
    jobs: tr("br_src_jobs", undefined, "Recent tasks and install history"),
    settings: tr("br_src_settings", undefined, "App settings (secrets removed)"),
  };
  const all = draft.cats.length === EVENT_CATS.length && draft.sources.length === SOURCE_IDS.length;
  const setEverything = (on: boolean) =>
    update(on ? { everything: true, cats: [...EVENT_CATS], sources: [...SOURCE_IDS] } : { everything: false, cats: [], sources: [] });
  const toggle = <T extends string>(list: T[], v: T): T[] => (list.includes(v) ? list.filter((x) => x !== v) : [...list, v]);
  const unavailable = (s: SourceId) => s === "crash_reports" && !isTauriEnv();

  return (
    <div className="grid max-w-3xl gap-4">
      <div className="grid gap-3 sm:grid-cols-2">
        <Select
          label={tr("br_range", undefined, "Time range (until now)")}
          value={draft.rangeKey}
          onChange={(e) => update({ rangeKey: e.target.value as RangeKey })}
          options={RANGE_KEYS.map((k) => ({ value: k, label: rangeLabel[k] }))}
        />
        {draft.rangeKey === "custom" && (
          <label className="grid gap-1 text-xs font-medium uppercase tracking-wide text-[var(--color-muted)]">
            {tr("br_range_from", undefined, "From")}
            <input
              type="datetime-local"
              value={draft.customStart ? toLocalInput(draft.customStart) : ""}
              max={latest}
              onChange={(e) => update({ customStart: e.target.value ? new Date(e.target.value).getTime() : null })}
              className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm normal-case text-[var(--color-text)]"
            />
          </label>
        )}
      </div>
      <Checkbox
        checked={all}
        indeterminate={!all && (draft.cats.length > 0 || draft.sources.length > 0)}
        onChange={setEverything}
        label={<span className="font-medium">{tr("br_everything", undefined, "Everything")}</span>}
        hint={tr("br_everything_hint", undefined, "Recommended. Untick below to leave something out.")}
      />
      <div className="grid gap-4 sm:grid-cols-2">
        <section>
          <h3 className="mb-1.5 text-sm font-medium">{tr("br_cats_title", undefined, "Events")}</h3>
          <ul className="grid gap-1">
            {EVENT_CATS.map((c) => (
              <li key={c}>
                <Checkbox checked={draft.cats.includes(c)} onChange={() => update({ cats: toggle(draft.cats, c) })} label={catLabel[c]} />
              </li>
            ))}
          </ul>
        </section>
        <section>
          <h3 className="mb-1.5 text-sm font-medium">{tr("br_sources_title", undefined, "Logs and data")}</h3>
          <ul className="grid gap-1">
            {SOURCE_IDS.map((s) => (
              <li key={s}>
                <Checkbox
                  checked={draft.sources.includes(s)}
                  disabled={unavailable(s)}
                  onChange={() => update({ sources: toggle(draft.sources, s) })}
                  label={sourceLabel[s]}
                />
              </li>
            ))}
          </ul>
        </section>
      </div>
      <Checkbox
        checked={draft.redact}
        onChange={(v) => update({ redact: v })}
        label={tr("br_redact", undefined, "Hide IP addresses, serials and home folders")}
        hint={tr("br_redact_hint", undefined, "Pairing keys and tokens are always removed.")}
      />
      <p className="text-xs text-[var(--color-muted)]" data-testid="br-include-summary">
        {tr(
          "br_include_summary",
          { cats: draft.cats.length, sources: draft.sources.length },
          `${draft.cats.length} kinds of events and ${draft.sources.length} logs selected.`,
        )}
      </p>
    </div>
  );
}
