import { useEffect, useMemo, useState } from "react";

import { GameIcon } from "../../../components";
import { useConfirm } from "../../../components/ConfirmDialog";
import { PingBars } from "../../../components/PingBars";
import { RoomsBrowserCore, type KnockInput } from "../../../components/RoomsBrowserCore";
import { lastName, rememberName } from "../../../join/guestName";
import { fetchStatus, hubGet, type RoomsSource, type StatusAnswer } from "../../../lib/room/hubRooms";
import { playedHosts } from "../../../lib/room/played";
import { ownRegion } from "../../../lib/room/regions";
import { APP_VERSION } from "../../../lib/room/version";
import { linkFromSocket, type JoinLink } from "../../../lib/joinProtocol";
import type { RoomCard } from "../../../lib/room/rooms";
import { getEngineUrl } from "../../../state/engine";
import { useConnectionStore } from "../../../state/connection";
import { useJoinStore } from "../../../state/joinSession";
import { deviceName, meshFor, nearbyJoins, useMeshStore } from "../../../state/mesh";
import { useTr } from "../../../state/lang";
import { okToJoin } from "../JoinAFriend";
import { DEFAULT_HUB_HOST, ownJoinWs } from "../playTogetherState";
import { useRoomNotice } from "./RoomNotice";

/** Where the Rooms list comes from in the apps: the engine's proxy (GET /api/join/rooms, one
 *  place for a custom hub), else, from an engine before it, the hub itself. */
export function useRoomsSource(): { source: RoomsSource | null; hub: string } {
  const [src, setSrc] = useState<{ source: RoomsSource | null; hub: string }>({ source: null, hub: DEFAULT_HUB_HOST });
  useEffect(() => {
    let alive = true;
    const engine = getEngineUrl();
    void hubGet(`${engine}/api/join/rooms?free=1`, { tries: 1 }).then((r) => {
      if (!alive) return;
      const hub = typeof (r?.json as { hub?: unknown } | null)?.hub === "string" ? ((r!.json as { hub: string }).hub as string) : DEFAULT_HUB_HOST;
      const viaEngine = !!r && r.status !== 404 && r.status !== 405;
      setSrc({ source: viaEngine ? { base: engine, viaEngine: true } : { base: `https://${DEFAULT_HUB_HOST}`, viaEngine: false }, hub });
    });
    return () => {
      alive = false;
    };
  }, []);
  return src;
}

/**
 * Play › Play Room › Rooms (design 7.0, 7.1): your friends' rooms first, then nearby games and
 * the public list. Joining is the same flow as an invite (Play › Join a friend's player shows it).
 */
export function RoomsTab({ onJoined, onCode, onCreate }: { onJoined: () => void; onCode: () => void; onCreate: () => void }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const notice = useRoomNotice();
  const { source, hub } = useRoomsSource();
  const console_ = useConnectionStore((s) => s.host) ?? "";
  const me = useMemo(() => ownRegion(), []);

  const join = async (link: JoinLink, name: string) => {
    const clean = rememberName(name);
    if (!clean) return;
    if (!(await okToJoin(tr, confirm, link))) return;
    useJoinStore.getState().join(link, clean);
    onJoined();
  };

  const onJoin = async (card: RoomCard, k: KnockInput) => {
    let pk = card.pk;
    // The list leaves the room key out (small pages): the friends lookup has it.
    if (!pk && card.pw && source) {
      const st = await fetchStatus(source, [card.room]);
      if (typeof st === "object") pk = st.rooms[card.room]?.pk ?? null;
    }
    await join(
      { origin: "", invite: null, name: card.host, room: { lobby: card.room, hub, via: "rooms", pk, password: k.password, tag: card.tag } },
      k.name,
    );
  };

  return (
    <div className="space-y-3" data-testid="rooms-tab">
      <RoomsBrowserCore
        source={source}
        me={me}
        version={APP_VERSION}
        defaultName={lastName()}
        ensureNotice={() => notice.ask("rooms")}
        onJoin={(c, k) => void onJoin(c, k)}
        onCode={onCode}
        onCreate={onCreate}
        cover={(c) => (c.game?.id && console_ ? <GameIcon host={console_} titleId={c.game.id} size={56} label={c.game.title} /> : null)}
        groups={
          <>
            <FriendsRooms source={source} hub={hub} onJoin={(link) => void join(link, lastName())} />
            <NearbyGames onJoin={(link) => void join(link, lastName())} />
          </>
        }
      />
      {notice.dialog}
      {dialog}
    </div>
  );
}

/** "Your friends' rooms": hosts this device played with whose room is open (the hub's status
 *  lookup by the lobby ids they handed over), asked again as often as the hub says. */
function FriendsRooms({ source, hub, onJoin }: { source: RoomsSource | null; hub: string; onJoin: (l: JoinLink) => void }) {
  const tr = useTr();
  const friends = useMemo(() => playedHosts().filter((p) => p.lobby), []);
  const [st, setSt] = useState<StatusAnswer | null>(null);
  useEffect(() => {
    if (!source || !friends.length) return;
    let alive = true;
    let timer = 0;
    const read = async () => {
      const r = await fetchStatus(
        source,
        friends.map((f) => f.lobby!),
      );
      if (!alive) return;
      const next = typeof r === "object" ? r : null;
      if (next) setSt(next);
      timer = window.setTimeout(() => void read(), (next?.everyS ?? 60) * 1000);
    };
    void read();
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [source, friends]);
  if (!friends.length) return null;
  const open = friends.filter((f) => st?.rooms[f.lobby!]);
  return (
    <section className="space-y-2" data-testid="rooms-friends">
      <h3 className="text-sm font-semibold">{tr("rooms_friends", undefined, "Your friends' rooms")}</h3>
      {open.length === 0 ? (
        <p className="text-xs text-[var(--color-muted)]">{tr("rooms_friends_none", undefined, "None of the people you've played with has a room open.")}</p>
      ) : (
        <ul className="space-y-1.5">
          {open.map((f) => {
            const s = st!.rooms[f.lobby!];
            const full = s.inRoom >= s.size;
            return (
              <li key={f.lobby} className="flex flex-wrap items-center gap-2 rounded-lg border border-[var(--color-border)] p-2.5 text-sm" data-testid="rooms-friend">
                <span className="min-w-0 flex-1 truncate">
                  {tr("rooms_friend_open", { name: f.host }, `${f.host}'s room is open`)}
                  <span className="ms-1 text-xs text-[var(--color-muted)]">
                    {s.game?.title ? `${s.game.title} · ` : ""}
                    {s.inRoom}/{s.size}
                    {s.locked ? ` · ${tr("room_locked", undefined, "Locked")}` : ""}
                  </span>
                </span>
                {f.rtts.length > 0 && <PingBars compact lastTime tier={null} ms={f.rtts[f.rtts.length - 1]} />}
                <button
                  type="button"
                  disabled={full || s.locked}
                  onClick={() =>
                    onJoin({ origin: "", invite: null, name: f.host, room: { lobby: f.lobby!, hub, via: "friends", pk: s.pk, password: null, tag: f.tag } })
                  }
                  className="rounded-full bg-[var(--color-accent)] px-3 py-1 text-xs font-semibold text-[var(--color-on-accent,#fff)] disabled:opacity-40"
                  data-testid="rooms-friend-join"
                >
                  {full ? tr("rooms_full", undefined, "Full") : tr("rooms_join", undefined, "Join")}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

/** "Nearby": Play Rooms on this network (mesh presence), asked without a code. */
function NearbyGames({ onJoin }: { onJoin: (l: JoinLink) => void }) {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const meshState = useMeshStore((s) => (host ? meshFor(s, host).state : null));
  const nearby = useMemo(() => nearbyJoins(meshState, ownJoinWs()), [meshState]);
  if (!nearby.length) return null;
  return (
    <section className="space-y-2" data-testid="rooms-nearby">
      <h3 className="text-sm font-semibold">{tr("rooms_nearby", undefined, "Nearby")}</h3>
      <ul className="space-y-1.5">
        {nearby.map(({ device, join }) => {
          const who = join.host_name || deviceName(device);
          const link = linkFromSocket(join.ws, who);
          return (
            <li key={join.ws} className="flex items-center gap-2 rounded-lg border border-[var(--color-border)] p-2.5 text-sm" data-testid="rooms-nearby-row">
              <span className="min-w-0 flex-1 truncate">
                {tr("rooms_nearby_game", { name: who }, `${who}'s game`)}
                {join.game_title ? <span className="ms-1 text-xs text-[var(--color-muted)]">{join.game_title}</span> : null}
              </span>
              <button
                type="button"
                disabled={!link}
                onClick={() => link && onJoin(link)}
                className="rounded-full bg-[var(--color-accent)] px-3 py-1 text-xs font-semibold text-[var(--color-on-accent,#fff)] disabled:opacity-40"
              >
                {tr("rooms_ask", undefined, "Ask to join")}
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
