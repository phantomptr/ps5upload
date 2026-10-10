import { AlarmClockOff, Play, X } from "lucide-react";

import { Button } from "../../components";
import { formatDate } from "../../lib/formatDate";
import { useTr } from "../../state/lang";
import { runScheduleAction, useScheduleStore } from "../../state/schedules";

/**
 * Scheduled reminders that came due while the app couldn't fire them (closed,
 * or the computer asleep), each with Run now and Dismiss.
 */
export function MissedSchedules() {
  const tr = useTr();
  const schedules = useScheduleStore((s) => s.schedules);
  const clearMissed = useScheduleStore((s) => s.clearMissed);
  const missed = schedules.filter((s) => s.missedAtMs !== undefined);
  if (missed.length === 0) return null;
  return (
    <section
      className="mb-6 rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-5 shadow-[var(--edge-highlight),var(--shadow-1)]"
      aria-labelledby="missed-schedules-title"
    >
      <header className="mb-3 flex items-center gap-2">
        <span className="grid h-8 w-8 place-items-center rounded-full bg-[var(--color-warn-soft)] text-[var(--color-warn)]">
          <AlarmClockOff size={15} aria-hidden />
        </span>
        <h2 id="missed-schedules-title" className="text-sm font-semibold">
          {tr("schedules_missed_title", undefined, "Missed scheduled reminders")}
        </h2>
      </header>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "schedules_missed_hint",
          undefined,
          "These came due while ps5upload was closed or the computer was asleep, so they didn't fire.",
        )}
      </p>
      <ul className="space-y-2">
        {missed.map((s) => (
          <li
            key={s.id}
            className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] px-4 py-2.5"
          >
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-medium">{s.label}</div>
              <div className="text-xs text-[var(--color-muted)]">
                {tr(
                  "schedules_missed_at",
                  { time: formatDate(s.missedAtMs!) },
                  `Due ${formatDate(s.missedAtMs!)}`,
                )}
              </div>
            </div>
            <Button
              size="sm"
              leftIcon={<Play size={12} />}
              onClick={() => {
                runScheduleAction(s);
                clearMissed(s.id);
              }}
            >
              {tr("schedules_run_now", undefined, "Run now")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<X size={12} />}
              onClick={() => clearMissed(s.id)}
            >
              {tr("dismiss", undefined, "Dismiss")}
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}
