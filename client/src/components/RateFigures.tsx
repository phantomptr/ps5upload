import { formatBytes } from "../lib/format";
import { formatEtaSeconds } from "../lib/uploadEta";
import { useRateEta } from "../lib/useRateEta";
import { useTr } from "../state/lang";

/**
 * Speed and time left of something that counts bytes (#981), for a screen's own progress line:
 * "12.3 MiB/s · about 3 min left", or "Stalled" when nothing has moved for a while. The same
 * computation as the Activity panel (lib/transferRate). Renders nothing until there is a speed.
 * `id` names the transfer: a new id starts the speed again.
 */
export function RateFigures({
  id,
  done,
  total,
  className = "",
  separator = " · ",
  prefix = "",
}: {
  id: string;
  done: number;
  total: number;
  className?: string;
  separator?: string;
  /** Put in front (" · "), only when there is something to show. */
  prefix?: string;
}) {
  const tr = useTr();
  const { rate, etaSeconds, stalled } = useRateEta(id, done, total);
  const parts: string[] = [];
  if (stalled) parts.push(tr("task_stalled", undefined, "Stalled"));
  else if (rate > 0) parts.push(`${formatBytes(rate)}/s`);
  if (!stalled && etaSeconds !== null) {
    const time = formatEtaSeconds(etaSeconds);
    parts.push(tr("fs_progress_eta", { time }, `about ${time} left`));
  }
  if (!parts.length) return null;
  return (
    <span className={className} data-testid="rate-figures">
      {prefix}
      {parts.join(separator)}
    </span>
  );
}
