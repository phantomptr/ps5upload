// A save kept in step between two PS5s by the engine itself (save_sync_auto.rs): the switch to
// turn it on, where it stands (In step / Waiting for <console> / Conflict, choose / Can't sync),
// Sync now, and the pick in a conflict. Presentational: the card does the calls.
import { RefreshCw } from "lucide-react";

import { Badge, Button, Checkbox } from "../../components";
import type { AutoView } from "../../api/saveSync";
import { formatAgo } from "../../lib/formatDate";
import { useTr } from "../../state/lang";
import { autoTone } from "./saveSync";

type Tr = ReturnType<typeof useTr>;

function stateLabel(tr: Tr, v: AutoView, where: string | null): string {
  switch (v.state) {
    case "in_step":
      return tr("saves_sync_in_step", undefined, "In step");
    case "conflict":
      return tr("saves_sync_auto_conflict", undefined, "Conflict: choose");
    case "cant_sync":
      return tr("saves_sync_auto_cant", undefined, "Can't sync");
    default:
      return where
        ? tr("saves_sync_auto_waiting_for", { console: where }, "Waiting for {console}")
        : tr("saves_sync_auto_waiting", undefined, "Waiting");
  }
}

function why(tr: Tr, v: AutoView, where: string): string | null {
  if (v.state === "conflict") {
    return v.first
      ? tr(
          "saves_sync_auto_first_body",
          undefined,
          "Not synced yet, so it can't tell which is newer. Pick the save to keep; the other one is backed up, then replaced.",
        )
      : tr(
          "saves_sync_auto_conflict_body",
          undefined,
          "The save changed on both PS5s since the last sync. Both are backed up. Pick the one to keep; the other is replaced.",
        );
  }
  const detail = v.detail ?? "";
  switch (v.reason) {
    case "offline":
      return tr(
        "saves_sync_auto_why_offline",
        { console: where },
        "{console} is off or resting. The save goes across when it's back.",
      );
    case "settling":
      return tr(
        "saves_sync_auto_why_settling",
        { console: where },
        "{console} has just woken up or is busy. Trying again in a minute.",
      );
    case "running":
      return tr(
        "saves_sync_auto_why_running",
        { console: where },
        "The game is running on {console}. The save goes across after it's closed.",
      );
    case "queued":
      return tr("saves_sync_auto_why_queued", undefined, "Goes across at the next check, within a minute.");
    case "error":
      return tr("saves_sync_auto_why_error", { detail }, "The last try failed ({detail}). Trying again.");
    case "ps4":
      return tr("saves_sync_reason_ps4", undefined, "PS4 saves can't be copied to another console.");
    case "no_save":
      return tr(
        "saves_sync_auto_why_no_save",
        { console: where },
        "There's no save of this game for the user on {console} yet. Start the game there once and save.",
      );
    case "refused":
      return tr("saves_sync_auto_why_refused", { detail }, "This save can't be moved safely: {detail}");
    case "failed":
      return tr(
        "saves_sync_auto_why_failed",
        { count: v.fails, detail },
        "Copying failed {count} times ({detail}). Fix that, then press Sync now.",
      );
    default:
      return null;
  }
}

export function AutoSyncPanel({
  view,
  canTurnOn,
  onLabel,
  a,
  b,
  busy,
  error,
  onToggle,
  onSyncNow,
  onKeep,
}: {
  /** Null: not kept in step. */
  view: AutoView | null;
  /** The switch is offered (a save that can go both ways). */
  canTurnOn: boolean;
  /** "Keep in step on <console> (<user>)". */
  onLabel: string;
  /** The pair's consoles by name. */
  a: string;
  b: string;
  busy: boolean;
  error: string | null;
  onToggle: (on: boolean) => void;
  onSyncNow: () => void;
  onKeep: (side: "a" | "b") => void;
}) {
  const tr = useTr();
  if (!view && !canTurnOn) return null;
  const where = view?.side === "b" ? b : view?.side === "a" ? a : null;
  const reason = view ? why(tr, view, where ?? "") : null;
  const last =
    view?.copied_ms && view.copied_direction
      ? tr(
          "saves_sync_auto_last",
          { ago: formatAgo(view.copied_ms), console: view.copied_direction === "a_to_b" ? b : a },
          "Last sent {ago} to {console}",
        )
      : null;
  return (
    <div className="flex flex-col gap-1" data-testid="save-sync-auto">
      <div className="flex flex-wrap items-center gap-2">
        <Checkbox checked={!!view} disabled={busy} onChange={onToggle} label={onLabel} />
        {view && (
          <Badge tone={autoTone(view.state)} size="sm">
            {stateLabel(tr, view, where)}
          </Badge>
        )}
      </div>
      {view && reason && (
        <p
          className={`text-xs ${view.state === "cant_sync" ? "text-[var(--color-bad)]" : "text-[var(--color-muted)]"}`}
        >
          {reason}
        </p>
      )}
      {view?.detail && view.reason !== "error" && view.reason !== "refused" && view.reason !== "failed" && (
        <p className="text-xs text-[var(--color-muted)]">{view.detail}</p>
      )}
      {last && <p className="text-xs text-[var(--color-muted)]">{last}</p>}
      {error && <p className="text-xs text-[var(--color-bad)]">{error}</p>}
      {view && (
        <div className="flex flex-wrap items-center gap-2">
          {view.state === "conflict" ? (
            <>
              <Button variant="secondary" size="sm" loading={busy} onClick={() => onKeep("a")}>
                {tr("saves_sync_keep", { console: a }, "Keep {console}'s save")}
              </Button>
              <Button variant="secondary" size="sm" loading={busy} onClick={() => onKeep("b")}>
                {tr("saves_sync_keep", { console: b }, "Keep {console}'s save")}
              </Button>
            </>
          ) : (
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<RefreshCw size={12} />}
              loading={busy}
              onClick={onSyncNow}
            >
              {tr("saves_sync_auto_sync_now", undefined, "Sync now")}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}
