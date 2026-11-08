import { useEffect, useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { Play, Settings2, Users } from "lucide-react";

import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { InternetHint, saveNetwork, savedNetwork, type NetworkKind } from "./NetworkChoice";
import { joinApi } from "./playTogetherState";

/**
 * Play › Play together before a stream: what it is, the console, where the friends are (this
 * network or the internet), and one button that starts the stream and makes the invite. The
 * stream starts as Play my PS5's Start does (StreamPanel start); the Play Together panel then
 * shows in its place, opens Play Together and makes the invite for the network picked here
 * (it reads the same remembered choice, NetworkChoice savedNetwork).
 */
export function PlayTogetherStart({ host, ...rest }: Omit<PlayTogetherStartViewProps, "internetInvites"> & { host: string | null }) {
  // Whether the engine makes internet invites, and whether they are on (Settings › Play
  // Together): read once per console. null: an engine from before them; undefined: not known yet.
  const [internetInvites, setInternetInvites] = useState<boolean | null | undefined>(undefined);
  useEffect(() => {
    if (!host) return;
    let alive = true;
    void joinApi.read(host).then((s) => {
      if (alive && s) setInternetInvites(s.settings.internetInvites);
    });
    return () => {
      alive = false;
    };
  }, [host]);
  return <PlayTogetherStartView {...rest} internetInvites={host ? internetInvites : undefined} />;
}

/** What Start and invite does: start this console's stream, first stop the stream of another
 *  console (one at a time), or show here the stream this console already has from elsewhere (a
 *  reload, another window, the API or MCP), without starting a new one. */
export function togetherStartKind(s: {
  /** Another console streams in this app. */
  otherStream: boolean;
  /** A stream runs in this app. */
  streaming: boolean;
  /** The console of a stream running in the engine that this app does not show. */
  elsewhere: string | null;
  /** The console selected. */
  host: string | null;
}): "switch" | "attach" | "start" {
  if (s.otherStream) return "switch";
  if (!s.streaming && s.elsewhere && s.host && s.elsewhere === s.host) return "attach";
  return "start";
}

export interface PlayTogetherStartViewProps {
  consoleName: string | null;
  /** A console paired for Play and nothing starting yet. */
  canStart: boolean;
  /** A start is under way (looking for blockers, waiting for the PS5's slot). */
  starting: boolean;
  /** Start and invite, or what it does when another stream is in the way. */
  startLabel: string;
  onStart: () => void;
  /** Play as (a console paired as more than one PS5 user), or null. */
  playAs: ReactNode;
  /** Internet invites: on, off in the settings, null (the engine has none), undefined (not known). */
  internetInvites: boolean | null | undefined;
}

export function PlayTogetherStartView({
  consoleName,
  canStart,
  starting,
  startLabel,
  onStart,
  playAs,
  internetInvites,
}: PlayTogetherStartViewProps) {
  const tr = useTr();
  const navigate = useNavigate();
  const [network, setNetwork] = useState<NetworkKind>(savedNetwork);
  const internetOffered = internetInvites !== null;
  const onInternet = internetOffered && network === "internet";
  const pick = (kind: NetworkKind) => {
    setNetwork(kind);
    saveNetwork(kind);
  };
  return (
    <section
      aria-labelledby="pt-start-title"
      className="space-y-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
      data-testid="pt-start"
    >
      <div className="space-y-1">
        <h2 id="pt-start-title" className="flex items-center gap-2 text-sm font-semibold">
          <Users size={16} aria-hidden />
          {tr("pt_title", undefined, "Play together")}
        </h2>
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "pt_tab_intro",
            undefined,
            "Friends play along as Player 2, 3 and 4 from their phone, tablet or computer, in the browser. No app needed.",
          )}
        </p>
      </div>

      <div className="flex flex-wrap items-end gap-3">
        {consoleName && (
          <p className="min-w-0 text-sm" data-testid="pt-start-console">
            <span className="text-[var(--color-muted)]">{tr("pt_tab_console", undefined, "Your PS5")}: </span>
            <span className="font-medium">{consoleName}</span>
          </p>
        )}
        {playAs}
      </div>

      <div className="space-y-2">
        <p id="pt-start-where" className="text-xs font-medium text-[var(--color-muted)]">
          {tr("pt_tab_where", undefined, "Where are your friends?")}
        </p>
        <div className="flex flex-wrap gap-1.5" role="group" aria-labelledby="pt-start-where" data-testid="pt-start-net">
          <button
            type="button"
            aria-pressed={!onInternet}
            onClick={() => pick("home")}
            className="chip min-h-8 px-3 text-xs max-md:min-h-11"
            data-testid="pt-start-net-home"
          >
            {tr("net_home", undefined, "This network")}
          </button>
          {internetOffered && (
            <button
              type="button"
              aria-pressed={onInternet}
              onClick={() => pick("internet")}
              className="chip min-h-8 px-3 text-xs max-md:min-h-11"
              data-testid="pt-start-net-internet"
            >
              {tr("net_internet", undefined, "Internet")}
            </button>
          )}
        </div>
        {onInternet && <InternetHint testId="pt-start-net" />}
        {onInternet && internetInvites === false && (
          <div className="flex flex-wrap items-center gap-2" data-testid="pt-start-net-off">
            <p className="min-w-0 flex-1 text-xs text-[var(--color-warn)]">
              {tr("pt_net_off", undefined, "Internet invites are off. Turn on Allow internet invites in Settings › Play Together.")}
            </p>
            <Button size="sm" variant="ghost" leftIcon={<Settings2 size={14} />} onClick={() => navigate("/settings")}>
              {tr("picker_open_settings", undefined, "Open settings")}
            </Button>
          </div>
        )}
      </div>

      <Button
        variant="primary"
        size="md"
        leftIcon={<Play size={14} />}
        onClick={onStart}
        disabled={!canStart}
        loading={starting}
        className="max-sm:w-full"
        data-testid="pt-start-invite"
      >
        {startLabel}
      </Button>
    </section>
  );
}
