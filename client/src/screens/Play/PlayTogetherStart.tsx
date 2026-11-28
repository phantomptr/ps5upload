import { useEffect, useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { Play, Settings2, Users } from "lucide-react";

import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { InternetHint } from "./NetworkChoice";
import { joinApi, setPendingRoom } from "./playTogetherState";
import { CreateRoomFields, createBody } from "./PlayRoom/CreateRoom";
import { PlayAppTip } from "./PlayRoom/PlayAppTip";
import { defaultRoomName } from "../../lib/room/nameText";
import { useRoomDraft } from "../../state/roomDraft";
import { useMeshStore } from "../../state/mesh";
import { PlayAppInstall } from "./PlayAppInstall";
import { PlayAppPairing } from "./PlayAppPairing";

/**
 * Play › Play Room before a stream: "Create a Play Room" (design 7.1): the console, the room's
 * name, size, who can see it and a password, and one button that starts the stream and opens the
 * room. The stream starts as Play my PS5's Start does (StreamPanel start); the room panel then
 * shows in its place, opens the room with these choices and makes the one link.
 */
export function PlayTogetherStart({ host, ...rest }: Omit<PlayTogetherStartViewProps, "invites"> & { host: string | null }) {
  // Where invites work (Settings › Play Together): read once per console. null: an engine from
  // before internet invites; undefined: not known yet.
  const [invites, setInvites] = useState<"anywhere" | "wifi" | null | undefined>(undefined);
  // Room size as saved (max_players + 1: a 12.6 file with 3 friends is a 4-player room).
  const [savedSize, setSavedSize] = useState(4);
  useEffect(() => {
    if (!host) return;
    let alive = true;
    void joinApi.read(host).then((s) => {
      if (!alive || !s) return;
      setInvites(s.settings.invites);
      setSavedSize(s.room.known ? s.room.size : Math.max(2, Math.min(4, s.settings.maxPlayers + 1)));
    });
    return () => {
      alive = false;
    };
  }, [host]);
  return (
    <div className="space-y-3">
      <PlayTogetherStartView {...rest} invites={host ? invites : undefined} savedSize={savedSize} />
      {/* A friend with a PS5 joins with PS5Upload Play (once per device, dismiss for good). */}
      <PlayAppTip />
      {/* PS5Upload Play on this PS5: install, update, open or remove it from here. */}
      <PlayAppInstall host={host} />
      {/* PS5Upload Play on the PS5: pair it here with the code it shows on the TV. */}
      <PlayAppPairing />
    </div>
  );
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
  /** Invites work: "anywhere", "wifi" (the settings say only on the Wi-Fi), null (the engine has
   *  no internet invites), undefined (not known yet). */
  invites: "anywhere" | "wifi" | null | undefined;
  /** Room size as saved (2–4). */
  savedSize?: number;
}

export function PlayTogetherStartView({
  consoleName,
  canStart,
  starting,
  startLabel,
  onStart,
  playAs,
  invites,
  savedSize = 4,
}: PlayTogetherStartViewProps) {
  const tr = useTr();
  const navigate = useNavigate();
  const me = useMeshStore((s) => s.me?.name) || "PS5Upload";
  const draftBad = useRoomDraft((s) => {
    const pw = s.draft.password ?? "";
    return pw.length > 0 && pw.length < 4;
  });
  return (
    <section
      aria-labelledby="pt-start-title"
      className="space-y-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
      data-testid="pt-start"
    >
      <div className="space-y-1">
        <h2 id="pt-start-title" className="flex items-center gap-2 text-sm font-semibold">
          <Users size={16} aria-hidden />
          {tr("room_title", undefined, "Play Room")}
        </h2>
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "room_intro",
            undefined,
            "Play your PS5 games with friends. They join from a phone, a computer or their own PS5, with their own controller.",
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

      <CreateRoomFields invites={invites} savedSize={savedSize} defaultName={defaultRoomName(tr, me)} />

      {/* One link for friends anywhere (the same Wi-Fi included): nothing to choose here. */}
      {invites === "anywhere" && (
        <div className="space-y-1" data-testid="pt-start-net">
          <InternetHint testId="pt-start-net" />
        </div>
      )}
      {invites === "wifi" && (
        <div className="flex flex-wrap items-center gap-2" data-testid="pt-start-net-off">
          <p className="min-w-0 flex-1 basis-[12rem] text-xs text-[var(--color-muted)]">
            {tr(
              "pt_wifi_only_setting",
              undefined,
              "Invites work only on your Wi-Fi. For friends somewhere else, set Invites work to Anywhere in Settings › App › Play Room.",
            )}
          </p>
          <Button size="sm" variant="ghost" leftIcon={<Settings2 size={14} />} onClick={() => navigate("/settings")}>
            {tr("picker_open_settings", undefined, "Open settings")}
          </Button>
        </div>
      )}

      <Button
        variant="primary"
        size="md"
        leftIcon={<Play size={14} />}
        onClick={() => {
          // The room opens with these choices once the stream is up (joinApi.open).
          setPendingRoom(createBody(useRoomDraft.getState().draft));
          onStart();
        }}
        disabled={!canStart || draftBad}
        loading={starting}
        className="max-sm:w-full"
        data-testid="pt-start-invite"
      >
        {startLabel}
      </Button>
      {consoleName && (
        <p className="text-xs text-[var(--color-muted)]" data-testid="room-create-note">
          {tr("room_create_note", { console: consoleName }, `Starts Remote Play on ${consoleName} and opens your room.`)}
        </p>
      )}
    </section>
  );
}
