import { LogOut, MonitorPlay, Square } from "lucide-react";
import { useLocation, useNavigate } from "react-router";

import { Button, Spinner } from "../components";
import { useConfirm } from "../components/ConfirmDialog";
import { friendsPlaying } from "../lib/rpStopConfirm";
import { isLive } from "../lib/goLive";
import { useGoLiveStore } from "../state/goLive";
import { useJoinPresence } from "../state/joinPresence";
import { useTr } from "../state/lang";
import { useRemotePlayStore } from "../state/remotePlay";

/** A stream keeps running on other screens: this bar says so, with the way back and Stop. */
export default function RemotePlayBanner() {
  return (
    <>
      <StreamBanner />
      <JoinBanner />
    </>
  );
}

/** A friend's game keeps playing on other screens, with its sound: this says so. */
function JoinBanner() {
  const tr = useTr();
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const host = useJoinPresence((s) => s.host);
  const leave = useJoinPresence((s) => s.leave);
  if (host === null || pathname === "/play") return null;
  return (
    <div
      className="mx-3 mt-3 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-accent)_35%,transparent)] bg-[var(--color-accent-soft,var(--color-surface-raised))] px-4 py-2 md:mx-5"
      data-testid="join-banner"
    >
      <div className="flex flex-wrap items-center gap-3">
        <MonitorPlay size={18} className="shrink-0 text-[var(--color-accent)]" />
        <p className="min-w-0 flex-1 basis-[12rem] text-sm font-medium">
          {tr("join_banner_playing", { host }, `You're in ${host}'s game`)}
        </p>
        <Button size="sm" variant="primary" onClick={() => navigate("/play")}>
          {tr("rp_banner_return", undefined, "Return to it")}
        </Button>
        <Button size="sm" variant="ghost" leftIcon={<LogOut size={12} />} onClick={() => leave?.()} data-testid="join-banner-leave">
          {tr("join_leave", undefined, "Leave")}
        </Button>
      </div>
    </div>
  );
}

function StreamBanner() {
  const tr = useTr();
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const stream = useRemotePlayStore((s) => s.stream);
  const phase = useRemotePlayStore((s) => s.phase);
  const target = useRemotePlayStore((s) => s.target);
  const stop = useRemotePlayStore((s) => s.stop);
  const live = useGoLiveStore((s) => isLive(s.status?.live ?? null));
  const { confirm, dialog } = useConfirm();
  /** Stopping Play ends Play Together: ask first while friends are in (as Play's own Stop does). */
  const askStop = async () => {
    const n = await friendsPlaying(target?.host);
    if (
      n > 0 &&
      !(await confirm({
        title: tr("pt_confirm_stop", { n }, `Stopping Play ends Play Together for ${n} friend(s). Continue?`),
        confirmLabel: tr("join_confirm_continue", undefined, "Continue"),
        destructive: true,
      }))
    ) {
      return;
    }
    stop();
  };
  if (!stream || !phase || phase === "ended" || pathname === "/play") return null;
  const name = target?.name ?? "";
  return (
    <div
      className="mx-3 mt-3 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-accent)_35%,transparent)] bg-[var(--color-accent-soft,var(--color-surface-raised))] px-4 py-2 md:mx-5"
      data-testid="rp-banner"
    >
      <div className="flex flex-wrap items-center gap-3">
        {phase === "playing" ? (
          <MonitorPlay size={18} className="shrink-0 text-[var(--color-accent)]" />
        ) : (
          <Spinner size={14} />
        )}
        <p className="min-w-0 flex-1 text-sm font-medium">
          {phase === "moved"
            ? tr("rp_banner_moved", { name }, `Remote Play on ${name} is showing in another window`)
            : tr("rp_banner_playing", { name }, `Remote Play is on: ${name}`)}
        </p>
        {live && (
          <span
            className="shrink-0 rounded-full bg-[var(--color-danger,#dc2626)] px-2 py-0.5 text-xs font-semibold text-white"
            data-testid="rp-banner-live"
          >
            {tr("golive_state_live", undefined, "Live")}
          </span>
        )}
        {/* A passcode the PS5 waits for is answered over the app (RemotePlayHost). */}
        <Button size="sm" variant="primary" onClick={() => navigate("/play")}>
          {tr("rp_banner_return", undefined, "Return to it")}
        </Button>
        <Button size="sm" variant="ghost" leftIcon={<Square size={12} />} onClick={() => void askStop()}>
          {tr("rp_stop", undefined, "Stop")}
        </Button>
      </div>
      {dialog}
    </div>
  );
}
