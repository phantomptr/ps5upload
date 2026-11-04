import { Clock } from "lucide-react";

import { formatDuration } from "../../lib/formatDate";
import { formatDate } from "../../lib/formatDate";
import { useTr } from "../../state/lang";

/**
 * The game's play on the selected console in one line: total time and when it was last played
 * (what the Game Activity screen used to list). `seconds`/`lastSeenMs` are the helper's tracked
 * numbers (or the app's own count); `consoleSeconds` is the console's own Play Time record,
 * which stands in when nothing was tracked and is added when it tells a different story.
 * Nothing at all is rendered for a game with no play on record.
 */
export function PlayedLine({
  seconds,
  lastSeenMs,
  consoleSeconds,
}: {
  seconds: number | undefined;
  lastSeenMs: number | undefined;
  consoleSeconds: number | undefined;
}) {
  const tr = useTr();
  const tracked = seconds !== undefined && seconds > 0 ? seconds : 0;
  const total = tracked || consoleSeconds || 0;
  const parts: string[] = [];
  if (total > 0) parts.push(tr("game_hub_played_time", { time: formatDuration(total) }, "Played {time}"));
  if (lastSeenMs) {
    parts.push(tr("game_hub_last_played_on", { date: formatDate(new Date(lastSeenMs), "date") }, "Last played {date}"));
  }
  // Both counts, and more than a minute apart: the console also counts play the helper missed.
  if (tracked > 0 && consoleSeconds && Math.abs(consoleSeconds - tracked) >= 60) {
    parts.push(
      tr("game_hub_played_console", { time: formatDuration(consoleSeconds) }, "{time} by the console's own count"),
    );
  }
  if (parts.length === 0) return null;
  return (
    <p className="mt-3 flex items-center gap-1.5 text-sm text-[var(--color-muted)]" data-testid="game-played">
      <Clock size={14} className="shrink-0" aria-hidden />
      <span className="min-w-0">{parts.join(" · ")}</span>
    </p>
  );
}
