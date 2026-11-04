import { useTr } from "../state/lang";
import type { TempsView } from "../state/sensors";

/** Why a console's temperatures are not current, or nothing when they are. */
export function TempsNote({ view, className = "" }: { view: TempsView; className?: string }) {
  const tr = useTr();
  const lines: string[] = [];
  if (view.note === "failed") {
    lines.push(
      view.error
        ? tr("sensors_read_failed", { error: view.error }, "Could not read the temperatures: {error}")
        : tr("sensors_read_failed_plain", undefined, "Could not read the temperatures."),
    );
  } else if (view.note === "transfer") {
    lines.push(tr("sensors_paused_transfer", undefined, "Paused during a transfer."));
  } else if (view.note === "helper") {
    lines.push(tr("sensors_helper_down", undefined, "The helper is not running."));
  }
  if (view.readAt && view.temps) {
    const time = new Date(view.readAt).toLocaleTimeString();
    lines.push(tr("sensors_last_read", { time }, "Last read at {time}."));
  }
  if (lines.length === 0) return null;
  return (
    <div className={`text-xs opacity-70 ${className}`} data-testid="temps-note">
      {lines.join(" ")}
    </div>
  );
}
