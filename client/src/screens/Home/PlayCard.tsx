import { useNavigate } from "react-router";
import { MonitorPlay, Play as PlayIcon, X } from "lucide-react";
import { useShallow } from "zustand/react/shallow";

import { Button } from "../../components";
import { hostOf } from "../../lib/addr";
import { collectionGames, useCollectionStore } from "../../state/collection";
import { useTr } from "../../state/lang";
import { usePlaySetupMarks } from "../../state/playSetup";
import { usePlayTimeStore } from "../../state/playTime";
import { useRemotePlayStore } from "../../state/remotePlay";
import { useRosterStore } from "../../state/roster";
import { CollectionCover } from "../Collection/CollectionCover";
import { useStreamLaunch } from "../Play/game/streamLaunch";
import { usePlaySetup } from "../Play/setup/usePlaySetup";

/** The title last seen playing on `host` (play-time tracking), or null. */
export function lastPlayedOn(lastSeenByHost: Record<string, Record<string, number>>, host: string): string | null {
  const seen = lastSeenByHost[hostOf(host)] ?? {};
  let best: string | null = null;
  for (const [id, t] of Object.entries(seen)) if (!best || t > seen[best]) best = id;
  return best;
}

/**
 * Home's Play card (10.0.0), the first thing on Home: play this console here in one press, carry
 * on with the last game, or get set up. Hidden from Home on request; nothing else on Home needs
 * it.
 */
export function PlayCard() {
  const tr = useTr();
  const navigate = useNavigate();
  const hidden = usePlaySetupMarks((s) => s.homeHidden);
  const setHidden = usePlaySetupMarks((s) => s.setHomeHidden);
  const setup = usePlaySetup({ poll: !hidden });
  const profile = useRosterStore((s) => s.profiles.find((p) => p.host === setup.host) ?? null);
  const { phase, target } = useRemotePlayStore(useShallow((s) => ({ phase: s.phase, target: s.target })));
  const lastSeen = usePlayTimeStore((s) => s.lastSeenByHost);
  const library = useCollectionStore((s) => s.library);
  if (hidden) return null;

  const streaming = phase !== null && phase !== "ended" && !!target;
  const lastId = setup.host ? lastPlayedOn(lastSeen, setup.host) : null;
  const lastGame = lastId ? (collectionGames(library).find((g) => g.game_id === lastId) ?? null) : null;
  const name = profile?.name || setup.host;

  const play = () => {
    if (profile?.wake_regist_key && profile.wake_rp_key && !streaming) {
      void useRemotePlayStore
        .getState()
        .start({ host: profile.host, name: profile.name, registKey: profile.wake_regist_key, rpKey: profile.wake_rp_key });
    }
    navigate("/play");
  };
  const playGame = () => {
    if (!lastId || !setup.host) return;
    useStreamLaunch.getState().request(setup.host, { titleId: lastId, titleName: lastGame?.title ?? lastId });
    navigate("/play");
  };

  const { done, total } = setup.progress;
  return (
    <section
      className="surface-panel relative overflow-hidden p-5"
      aria-label={tr("play_title", undefined, "Play")}
      data-testid="home-play"
      data-state={streaming ? "playing" : setup.ready ? "ready" : "setup"}
    >
      <button
        type="button"
        className="absolute end-3 top-3 rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
        aria-label={tr("play_home_hide", undefined, "Hide from Home")}
        title={tr("play_home_hide", undefined, "Hide from Home")}
        onClick={() => setHidden(true)}
        data-testid="home-play-hide"
      >
        <X size={14} />
      </button>
      <div className="flex flex-wrap items-center gap-4 pe-8">
        <span className="grid h-12 w-12 shrink-0 place-items-center rounded-2xl bg-[color-mix(in_srgb,var(--color-accent)_16%,transparent)] text-[var(--color-accent)]">
          <MonitorPlay size={24} aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="text-lg font-semibold">{tr("play_title", undefined, "Play")}</h2>
          <p className="text-sm text-[var(--color-muted)]">
            {streaming
              ? tr("play_home_playing", { name: target?.name || name }, `Playing ${target?.name || name}`)
              : setup.ready
                ? tr("play_home_ready", { name }, `${name}, here on this device. It wakes the console if it's resting.`)
                : tr(
                    "play_home_pitch",
                    undefined,
                    "Play your PS5 right here: its screen streamed to this device, played with a controller, the keyboard or touch.",
                  )}
          </p>
        </div>
      </div>

      {streaming ? (
        <div className="mt-4 flex flex-wrap gap-2">
          <Button variant="primary" leftIcon={<PlayIcon size={15} />} onClick={() => navigate("/play")} data-testid="home-play-back">
            {tr("play_home_back", undefined, "Back to Play")}
          </Button>
        </div>
      ) : setup.ready ? (
        <div className="mt-4 flex flex-wrap items-center gap-3">
          <Button variant="primary" size="lg" leftIcon={<PlayIcon size={16} />} onClick={play} data-testid="home-play-start">
            {tr("play_title", undefined, "Play")}
          </Button>
          {lastId && (
            <button
              type="button"
              onClick={playGame}
              className="flex min-w-0 items-center gap-2.5 rounded-xl border border-[var(--color-border)] py-1.5 ps-1.5 pe-3 text-start hover:bg-[var(--color-surface-3)]"
              data-testid="home-play-continue"
            >
              {lastGame && (
                <span className="h-10 w-10 shrink-0 overflow-hidden rounded-lg">
                  <CollectionCover game={lastGame} className="h-full" />
                </span>
              )}
              <span className="min-w-0">
                <span className="block text-xs text-[var(--color-muted)]">{tr("play_home_continue", undefined, "Continue")}</span>
                <span className="block truncate text-sm font-medium">{lastGame?.title ?? lastId}</span>
              </span>
            </button>
          )}
        </div>
      ) : (
        <div className="mt-4 space-y-2">
          <div className="flex items-center gap-3">
            <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-[var(--color-surface-3)]" aria-hidden>
              <div className="h-full rounded-full bg-[var(--color-accent)]" style={{ width: `${(done / total) * 100}%` }} />
            </div>
            <span className="shrink-0 text-xs text-[var(--color-muted)]">
              {tr("play_setup_progress", { done, total }, `${done} of ${total} done`)}
            </span>
          </div>
          <Button variant="primary" onClick={() => navigate("/play/setup")} data-testid="home-play-setup">
            {tr("play_setup_title", undefined, "Set up Play")}
          </Button>
        </div>
      )}
    </section>
  );
}
