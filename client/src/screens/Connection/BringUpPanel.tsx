import { CheckCircle2, Zap } from "lucide-react";
import { Link } from "react-router";

import { Button, Spinner, ErrorCard } from "../../components";
import { profileForHost } from "../../state/consoleSettings";
import { useRosterStore } from "../../state/roster";
import { AutoBringUpToggle, useBringUpPlaylistId } from "../Settings/ConsoleBringUp";
import { ps5SettingsLink } from "../Settings/settingsTabs";
import { useConnectionStore } from "../../state/connection";
import {
  bringUpStatusFor,
  useBringUpStore,
  type BringUpStatus,
} from "../../state/bringUp";
import { helperSendFor, useHelperSendStore } from "../../state/helperSend";
import { usePayloadPlaylistsStore } from "../../state/payloadPlaylists";
import { useTr } from "../../state/lang";
import { useHelperReplaceGuard } from "./useHelperReplaceGuard";

/**
 * Quick bring-up panel for the Connection screen. One tap runs the configured
 * pre-helper bring-up playlist (kstuff/SMP), sends the helper, and waits for it
 * to come up — after which the auto-loader fires the post-helper chain. The
 * orchestration lives in state/bringUp; this is just the trigger + status.
 */
export function BringUpPanel() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  // This console's status only: another console's bring-up neither shows here nor blocks Run.
  const status = useBringUpStore((s) => bringUpStatusFor(s, host));
  const run = useBringUpStore((s) => s.run);
  const { guard, dialog: replaceDialog } = useHelperReplaceGuard();
  // This console's own bring-up playlist and switch (Settings › This PS5 shows the same ones).
  const consoleId = useRosterStore((s) => profileForHost(host, s.profiles)?.id ?? null);
  const bringUpId = useBringUpPlaylistId(consoleId);
  const playlists = usePayloadPlaylistsStore((s) => s.playlists);
  const bringUpPlaylist = bringUpId
    ? playlists.find((p) => p.id === bringUpId)
    : undefined;

  const running = status.kind === "running";
  const canRun = !!host.trim() && !running;

  return (
    <section className="rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] mt-6 p-5">
      {replaceDialog}
      <header className="mb-3 flex flex-wrap items-start justify-between gap-3">
        <div className="flex min-w-0 items-start gap-2">
          <Zap size={16} className="mt-0.5 shrink-0 text-[var(--color-accent)]" />
          <div className="min-w-0">
            <h2 className="text-sm font-semibold">
              {tr("bringup_title", undefined, "Quick bring-up")}
            </h2>
            <p className="mt-0.5 text-xs text-[var(--color-muted)]">
              {tr(
                "bringup_desc_v2",
                undefined,
                "For every boot, once your playlists are set up in Payloads: runs your bring-up playlist (kstuff, ShadowMount+…), sends the helper the same way step 2 does, then runs your auto-loader playlist.",
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
            running ? <Spinner size={12} tone="inherit" /> : <Zap size={12} />
          }
          onClick={() => void run(host, tr, { guard })}
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

      <AutoBringUpToggle
        className="mb-3"
        consoleId={consoleId}
        hint={tr(
          "bringup_auto_hint_short",
          undefined,
          "Runs this by itself when the PS5's loader is up and the helper isn't, about 90 seconds after the loader appears. Once each time the console starts.",
        )}
      />
      {consoleId && (
        <p className="mb-3 text-xs">
          <Link
            to={ps5SettingsLink(consoleId)}
            className="text-[var(--color-accent)] hover:underline"
          >
            {tr("bringup_ps5_settings_link", undefined, "Bring-up playlist and other settings for this PS5")}
          </Link>
        </p>
      )}

      {status.kind !== "idle" && <BringUpStatusLine status={status} host={host} />}
    </section>
  );
}

function BringUpStatusLine({ status, host }: { status: BringUpStatus; host: string }) {
  const tr = useTr();
  // The helper phase is the shared send: show its own step (checking elfldr, sending, waiting).
  const sendMsg = useHelperSendStore((s) => helperSendFor(s, host.trim())?.msg);

  if (status.kind === "idle") return null;
  if (status.kind === "running") {
    const phaseText =
      status.phase === "prehelper"
        ? tr(
            "bringup_phase_prehelper",
            { name: status.detail },
            "Loading bring-up payloads ({name})…",
          )
        : sendMsg || tr("bringup_phase_helper", undefined, "Sending the helper…");
    return (
      <div className="flex items-center gap-2 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-accent)_40%,transparent)] bg-[var(--color-surface)] p-2 text-xs">
        <Spinner size={14} tone="accent" />
        <span>{phaseText}</span>
      </div>
    );
  }
  if (status.kind === "done") {
    return (
      <div className="flex items-center gap-2 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-good)_40%,transparent)] bg-[var(--color-surface)] p-2 text-xs">
        <CheckCircle2 size={14} className="text-[var(--color-good)]" />
        <span>{tr("bringup_done", undefined, "PS5 is ready.")}</span>
      </div>
    );
  }
  // failed
  return (
    <ErrorCard
      title={tr("bringup_failed_title", "Bring-up failed")}
      detail={tr(
        "bringup_failed",
        { phase: status.phase, error: status.error },
        "Bring-up failed at {phase}: {error}",
      )}
    />
  );
}
