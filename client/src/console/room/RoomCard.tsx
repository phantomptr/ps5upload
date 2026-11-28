import { useState } from "react";
import { Lock, Play, Users } from "lucide-react";
import { call } from "../api";
import { t } from "../strings";
import { playAppErrorText } from "../home/PlayAppCard";
import { pingBars, useRoom, type Room, type RoomPlayer } from "./room";

/** "Private" · "Friends" · "Public". */
export function visibilityLabel(v: Room["visibility"]): string {
  return t(`cw_room_vis_${v}`);
}

/** One player: name, bars (with the word for screen readers and the TV: colour is never the only
 *  signal) and the round trip. */
function PlayerChip({ p }: { p: RoomPlayer }) {
  const bars = pingBars(p);
  const word = bars ? t(`cw_room_bars_${bars}`) : t("cw_room_ping_none");
  return (
    <li className="cw-room-player" data-bars={bars}>
      <span className="cw-room-player-name">{p.name}</span>
      <span className={`cw-room-bars is-${bars}`} role="img" aria-label={word} title={word}>
        {[1, 2, 3, 4].map((i) => (
          <i key={i} className={i <= bars ? "is-on" : undefined} />
        ))}
      </span>
      {p.ping_ms != null ? <span className="cw-dim">{t("cw_room_ping", { ms: Math.round(p.ping_ms) })}</span> : null}
    </li>
  );
}

export interface RoomCardViewProps {
  room: Room;
  message?: string | null;
  onOpenPlay?: () => void;
  onOpenRules?: (() => void) | null;
}

/** "Your room", read only (Minion may be reachable from the LAN: nothing here changes the room). */
export function RoomCardView({ room, message, onOpenPlay, onOpenRules }: RoomCardViewProps) {
  const parts = [room.game?.name || room.game?.title_id || null, visibilityLabel(room.visibility)].filter(Boolean);
  const count = room.size ? t("cw_room_count", { n: room.playing, size: room.size }) : t("cw_room_count_n", { n: room.playing });
  return (
    <section className="cw-card cw-room" aria-label={t("cw_room_title")} data-room={room.engine_id}>
      <div className="cw-room-head">
        <Users className="cw-game-icon" aria-hidden />
        <div className="cw-game-text">
          <div className="cw-dim">{t("cw_room_title")}</div>
          <div className="cw-game-title">
            {room.name}
            {room.locked ? <Lock className="cw-room-lock" aria-label={t("cw_room_locked")} /> : null}
          </div>
          <div className="cw-dim">{[...parts, count].join(" · ")}</div>
        </div>
        {onOpenPlay ? (
          <button type="button" className="cw-focusable cw-button" data-action="room-open-play" onClick={onOpenPlay}>
            <Play aria-hidden />
            <span>{t("cw_room_open_play")}</span>
          </button>
        ) : null}
      </div>
      {room.players.length ? (
        <ul className="cw-room-players">
          {room.players.map((p, i) => (
            <PlayerChip key={`${i}:${p.name}`} p={p} />
          ))}
        </ul>
      ) : (
        <p className="cw-dim cw-room-line">{t("cw_room_no_players")}</p>
      )}
      <p className="cw-dim cw-room-line">
        {room.asks > 0 ? `${t("cw_room_asks", { n: room.asks })} · ` : ""}
        {room.hosted_from
          ? t(room.online ? "cw_room_hosted_from" : "cw_room_host_away", { device: room.hosted_from })
          : null}
      </p>
      {message ? (
        <div className="cw-msg is-error" role="alert">
          {message}
        </div>
      ) : null}
      {onOpenRules ? (
        <button type="button" className="cw-focusable cw-button cw-room-rules" data-action="room-rules" onClick={onOpenRules}>
          <span>{t("cw_room_rules")}</span>
        </button>
      ) : null}
    </section>
  );
}

/** Home's "Your room" card: shown only while an engine hosts a Play Room on this console. */
export function RoomCard({ onOpenRules }: { onOpenRules?: (() => void) | null }) {
  const room = useRoom();
  const [message, setMessage] = useState<string | null>(null);
  if (!room) return null;
  const openPlay = () => {
    setMessage(null);
    call("/api/play-app/open", { intent: "host" }).then(
      () => {},
      (e: unknown) => setMessage(playAppErrorText(e)),
    );
  };
  return <RoomCardView room={room} message={message} onOpenPlay={openPlay} onOpenRules={onOpenRules} />;
}
