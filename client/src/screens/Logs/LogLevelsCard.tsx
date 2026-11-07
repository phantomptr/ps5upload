import { useEffect, useState } from "react";
import { ChevronRight } from "lucide-react";

import { Button, Callout } from "../../components";
import { useTr } from "../../state/lang";
import {
  LOG_CATEGORIES,
  LOG_LEVELS,
  isRaise,
  raiseActive,
  useDiagSettingsStore,
  type LevelsChange,
  type LogCategory,
} from "../../state/diagSettings";
import type { LogLevel } from "../../state/logs";
import { changeLevels, refreshLevels } from "../../lib/logLevels";
import { formatDate } from "../../lib/formatDate";

/** How long a raise lasts, in minutes. */
const DURATIONS = [30, 60, 240, 1440] as const;

/**
 * Log levels (state/diagSettings, the engine's `/api/logs/level`): one level for everything,
 * per-category overrides, and how long a level above Info lasts before it goes back by itself.
 * The same setting reaches the engine and the console's helper, and the app's own disk log.
 */
export default function LogLevelsCard() {
  const tr = useTr();
  const engine = useDiagSettingsStore((s) => s.engine);
  const localLevel = useDiagSettingsStore((s) => s.logLevel);
  const setLocalLevel = useDiagSettingsStore((s) => s.setLogLevel);
  const [minutes, setMinutes] = useState<number>(60);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    void refreshLevels();
  }, []);

  const levelLabel: Record<LogLevel, string> = {
    trace: tr("log_level_trace", undefined, "Trace"),
    debug: tr("log_level_debug", undefined, "Debug"),
    info: tr("log_level_info", undefined, "Info"),
    warn: tr("log_level_warn", undefined, "Warn"),
    error: tr("log_level_error", undefined, "Error"),
  };
  const catLabel: Record<LogCategory, string> = {
    general: tr("logs_cat_general", undefined, "Everything else"),
    api: tr("logs_cat_api", undefined, "Engine requests"),
    connection: tr("logs_cat_connection", undefined, "Connections"),
    helper: tr("logs_cat_helper", undefined, "PS5 helper"),
    rp: tr("logs_cat_rp", undefined, "Remote Play"),
    pairing: tr("logs_cat_pairing", undefined, "Pairing"),
    wake: tr("logs_cat_wake", undefined, "Waking the console"),
    transfer: tr("logs_cat_transfer", undefined, "Transfers"),
    install: tr("logs_cat_install", undefined, "Installs"),
    cleanup: tr("logs_cat_cleanup", undefined, "Clean-up"),
    files: tr("logs_cat_files", undefined, "Files"),
    convert: tr("logs_cat_convert", undefined, "Conversions"),
    together: tr("logs_cat_together", undefined, "Play Together"),
    golive: tr("logs_cat_golive", undefined, "Go live"),
    mesh: tr("logs_cat_mesh", undefined, "Mesh and Drive"),
    ui: tr("logs_cat_ui", undefined, "App screens"),
  };
  const durationLabel: Record<(typeof DURATIONS)[number], string> = {
    30: tr("logs_levels_30m", undefined, "30 minutes"),
    60: tr("logs_levels_1h", undefined, "1 hour"),
    240: tr("logs_levels_4h", undefined, "4 hours"),
    1440: tr("logs_levels_24h", undefined, "24 hours"),
  };

  const apply = async (change: LevelsChange) => {
    setBusy(true);
    setError(null);
    try {
      await changeLevels(change);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const setGlobal = (level: LogLevel) => {
    if (!engine) {
      setLocalLevel(level);
      return;
    }
    void apply(isRaise(level) ? { level, minutes } : { level });
  };
  const setCategory = (cat: LogCategory, value: string) => {
    const level = value === "" ? null : (value as LogLevel);
    void apply({ categories: { [cat]: level }, merge: true, ...(level && isRaise(level) ? { minutes } : {}) });
  };

  const raised = raiseActive(engine);
  const global = engine?.level ?? localLevel;
  const overrides = Object.keys(engine?.categories ?? {}).length;

  return (
    <div className="mb-3 grid gap-2.5 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-4 py-3 text-xs shadow-[var(--edge-highlight)]">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className="font-medium">{tr("logs_levels_title", undefined, "Log level")}</span>
        <select
          aria-label={tr("logs_levels_title", undefined, "Log level")}
          value={global}
          disabled={busy}
          onChange={(e) => setGlobal(e.target.value as LogLevel)}
          className="select !w-auto !py-1 text-xs max-md:min-h-11"
          data-testid="log-level-global"
        >
          {(engine ? LOG_LEVELS : LOG_LEVELS.filter((l) => !isRaise(l))).map((l) => (
            <option key={l} value={l}>
              {levelLabel[l]}
            </option>
          ))}
        </select>
        {engine && (
          <label className="flex items-center gap-2 text-[var(--color-muted)]">
            {tr("logs_levels_for", undefined, "Raised levels last")}
            <select
              value={minutes}
              disabled={busy}
              onChange={(e) => setMinutes(Number(e.target.value))}
              className="select !w-auto !py-1 text-xs max-md:min-h-11"
            >
              {DURATIONS.map((m) => (
                <option key={m} value={m}>
                  {durationLabel[m]}
                </option>
              ))}
            </select>
          </label>
        )}
        {engine && (raised || overrides > 0 || global !== "info") && (
          <Button variant="ghost" size="sm" className="ms-auto" disabled={busy} onClick={() => void apply({ reset: true })}>
            {tr("logs_levels_reset", undefined, "Back to Info")}
          </Button>
        )}
      </div>
      <p className="leading-relaxed text-[var(--color-muted)]">
        {!engine
          ? tr("logs_levels_offline", undefined, "The engine can't be reached: this sets only the app's own log.")
          : raised && engine.until_ms !== null
            ? tr(
                "logs_levels_raised_until",
                { time: formatDate(engine.until_ms) },
                "Raised until {time}, then back to Info by itself.",
              )
            : tr(
                "logs_levels_hint",
                undefined,
                "Debug and Trace record more detail to find a problem, in the app, the engine and the PS5 helper, then go back to Info by themselves. Never keys, PINs or passwords.",
              )}
      </p>
      {engine && (
        <div>
          <button
            type="button"
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
            className="flex items-center gap-1 text-[var(--color-muted)] hover:text-[var(--color-text)] max-md:min-h-11"
          >
            <ChevronRight size={12} className={`transition-transform ${open ? "rotate-90" : "rtl:rotate-180"}`} />
            {tr("logs_levels_by_category", undefined, "By category")}
            {overrides > 0 && <span className="tabular-nums">({overrides})</span>}
          </button>
          {open && (
            <ul className="mt-2 grid gap-1.5 sm:grid-cols-2" data-testid="log-level-categories">
              {LOG_CATEGORIES.map((c) => (
                <li key={c} className="flex items-center justify-between gap-2">
                  <span className="min-w-0 truncate">{catLabel[c]}</span>
                  <select
                    aria-label={catLabel[c]}
                    value={engine.categories[c] ?? ""}
                    disabled={busy}
                    onChange={(e) => setCategory(c, e.target.value)}
                    className="select !w-auto !py-1 text-xs max-md:min-h-11"
                  >
                    <option value="">{tr("logs_levels_same", undefined, "Same as above")}</option>
                    {LOG_LEVELS.map((l) => (
                      <option key={l} value={l}>
                        {levelLabel[l]}
                      </option>
                    ))}
                  </select>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {error && (
        <Callout
          tone="error"
          title={tr("logs_levels_failed", undefined, "Couldn't change the log level")}
          onDismiss={() => setError(null)}
        >
          {error}
        </Callout>
      )}
    </div>
  );
}
