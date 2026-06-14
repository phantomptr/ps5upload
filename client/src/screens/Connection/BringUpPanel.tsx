import { CheckCircle2, Loader2, XCircle, Zap } from "lucide-react";

import { Button } from "../../components";
import { useConnectionStore } from "../../state/connection";
import { useBringUpStore, type BringUpStatus } from "../../state/bringUp";
import { usePayloadPlaylistsStore } from "../../state/payloadPlaylists";
import { useTr } from "../../state/lang";

/**
 * Quick bring-up panel for the Connection screen. One tap runs the configured
 * pre-helper bring-up playlist (kstuff/SMP), sends the helper, and waits for it
 * to come up — after which the auto-loader fires the post-helper chain. The
 * orchestration lives in state/bringUp; this is just the trigger + status.
 */
export function BringUpPanel() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const status = useBringUpStore((s) => s.status);
  const run = useBringUpStore((s) => s.run);
  const bringUpId = usePayloadPlaylistsStore(
    (s) => s.autoLoader.bringUpPlaylistId,
  );
  const playlists = usePayloadPlaylistsStore((s) => s.playlists);
  const bringUpPlaylist = bringUpId
    ? playlists.find((p) => p.id === bringUpId)
    : undefined;

  const running = status.kind === "running";
  const canRun = !!host.trim() && !running;

  return (
    <section className="mt-6 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <header className="mb-3 flex flex-wrap items-start justify-between gap-3">
        <div className="flex min-w-0 items-start gap-2">
          <Zap size={16} className="mt-0.5 shrink-0 text-[var(--color-accent)]" />
          <div className="min-w-0">
            <h2 className="text-sm font-semibold">
              {tr("bringup_title", undefined, "Quick bring-up")}
            </h2>
            <p className="mt-0.5 text-xs text-[var(--color-muted)]">
              {tr(
                "bringup_desc",
                undefined,
                "One tap: load your bring-up payloads, send the helper, then wait until the PS5 is ready.",
              )}
            </p>
            <p className="mt-1 text-xs text-[var(--color-muted)]">
              {bringUpPlaylist
                ? tr(
                    "bringup_chain_with",
                    { name: bringUpPlaylist.name },
                    "{name} → helper → auto-loader",
                  )
                : tr(
                    "bringup_chain_helper_only",
                    undefined,
                    "helper → auto-loader (no bring-up playlist set)",
                  )}
            </p>
          </div>
        </div>
        <Button
          variant="primary"
          size="sm"
          leftIcon={
            running ? <Loader2 size={12} className="animate-spin" /> : <Zap size={12} />
          }
          onClick={() => void run(host)}
          disabled={!canRun}
          title={
            !host.trim()
              ? tr("bringup_need_host", undefined, "Enter a PS5 IP first")
              : tr("bringup_run", undefined, "Bring up PS5")
          }
        >
          {tr("bringup_run", undefined, "Bring up PS5")}
        </Button>
      </header>

      {status.kind !== "idle" && <BringUpStatusLine status={status} />}
    </section>
  );
}

function BringUpStatusLine({ status }: { status: BringUpStatus }) {
  const tr = useTr();

  if (status.kind === "idle") return null;
  if (status.kind === "running") {
    const phaseText =
      status.phase === "prehelper"
        ? tr(
            "bringup_phase_prehelper",
            { name: status.detail },
            "Loading bring-up payloads ({name})…",
          )
        : status.phase === "helper"
          ? tr("bringup_phase_helper", undefined, "Sending the helper…")
          : tr(
              "bringup_phase_waiting",
              undefined,
              "Waiting for the helper to come up…",
            );
    return (
      <div className="flex items-center gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface)] p-2 text-xs">
        <Loader2 size={14} className="animate-spin text-[var(--color-accent)]" />
        <span>{phaseText}</span>
      </div>
    );
  }
  if (status.kind === "done") {
    return (
      <div className="flex items-center gap-2 rounded-md border border-[var(--color-good)] bg-[var(--color-surface)] p-2 text-xs">
        <CheckCircle2 size={14} className="text-[var(--color-good)]" />
        <span>{tr("bringup_done", undefined, "PS5 is ready.")}</span>
      </div>
    );
  }
  // failed
  return (
    <div className="flex items-start gap-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-bad)]">
      <XCircle size={14} className="mt-0.5 shrink-0" />
      <span>
        {tr(
          "bringup_failed",
          { phase: status.phase, error: status.error },
          "Bring-up failed at {phase}: {error}",
        )}
      </span>
    </div>
  );
}
