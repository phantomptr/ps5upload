import { useEffect, useState } from "react";

import { tierLabel } from "../join/routeTier";
import { carriesStream, streamParts, type RoomPreview } from "../lib/room/preview";
import { hubMissText, type HubMiss } from "../lib/room/hubRooms";
import { regionLabel } from "../lib/room/regions";
import type { Expected, RoomCard } from "../lib/room/rooms";
import { useTr } from "../state/lang";
import { PingBars } from "./PingBars";

type Tr = ReturnType<typeof useTr>;

function approvalText(tr: Tr, a: RoomPreview["room"]["approval"]): string {
  switch (a) {
    case "friends":
      return tr("rooms_approval_friends", undefined, "People the host played with get in without asking; others ask");
    case "password":
      return tr("rooms_approval_password", undefined, "The password lets you in without asking");
    case "anyone":
      return tr("rooms_approval_anyone", undefined, "Anyone can come in");
    default:
      return tr("rooms_approval_ask", undefined, "Asks the host");
  }
}

/**
 * A room's Details before joining (design 6.4): the host, game, players with their seats and
 * quality, the stream, how long they've played, how people get in, and the viewer's own expected
 * connection ("measured" on a direct way, else "about"). `load` fetches the host's preview.
 */
export function RoomDetails({
  card,
  expected,
  load,
  downKbps = null,
  dark = false,
  onAsk,
  askLabel,
}: {
  card: RoomCard;
  expected: Expected | null;
  load: () => Promise<RoomPreview | HubMiss>;
  /** This viewer's measured download (a direct way), for "Your connection can carry this stream". */
  downKbps?: number | null;
  dark?: boolean;
  onAsk: () => void;
  askLabel: string;
}) {
  const tr = useTr();
  const [p, setP] = useState<RoomPreview | HubMiss | null>(null);
  useEffect(() => {
    let alive = true;
    void load().then((r) => alive && setP(r));
    return () => {
      alive = false;
    };
  }, [load]);
  const muted = dark ? "text-white/60" : "text-[var(--color-muted)]";
  const preview = p && typeof p === "object" ? p : null;
  const carries = preview?.stream ? carriesStream(downKbps, preview.stream.kbps) : null;
  return (
    <div className="space-y-3 text-sm" data-testid="room-details">
      <div>
        <p className="text-base font-semibold">{card.name}</p>
        <p className={muted}>
          {card.host} {card.tag.toUpperCase()}
          {card.region ? ` · ${regionLabel(tr, card.region)}` : ""}
        </p>
      </div>
      <p className="flex flex-wrap items-center gap-2" data-testid="room-details-you">
        <span className={muted}>{tr("rooms_your_connection", { host: card.host }, `Your connection to ${card.host}`)}</span>
        {expected ? (
          <PingBars
            tier={expected.tier}
            ms={expected.ms}
            estimate={expected.kind === "about"}
            measured={expected.kind === "measured"}
            lastTime={expected.kind === "last_time"}
          />
        ) : (
          <span>—</span>
        )}
      </p>
      {expected?.kind === "about" && card.region && (
        <p className={`text-xs ${muted}`}>{tr("rooms_estimate_note", undefined, "An estimate from where you both are. You see the real number once you're in.")}</p>
      )}
      {p === null && <p className={muted}>{tr("rooms_details_loading", undefined, "Getting the details…")}</p>}
      {typeof p === "string" && (
        <p className={muted} data-testid="room-details-miss" data-why={p}>
          {p === "no_answer" || p === "needs_direct" ? hubMissText(tr, p) : tr("rooms_details_old", undefined, "More details need the host to update PS5Upload.")}
        </p>
      )}
      {preview && (
        <>
          {preview.game && (
            <p data-testid="room-details-game">
              <span className={muted}>{tr("rooms_game", undefined, "Game")}: </span>
              {preview.game.title}
              {preview.game.players ? ` · ${tr("rooms_game_players", { n: preview.game.players }, `${preview.game.players} players`)}` : ""}
            </p>
          )}
          {preview.players.length > 0 && (
            <ul className="space-y-1" data-testid="room-details-players">
              {preview.players.map((pl) => (
                <li key={`${pl.seat}-${pl.name}`} className="flex items-center gap-2">
                  <span className={`w-6 text-xs ${muted}`}>{pl.seat || "·"}</span>
                  <span className="min-w-0 flex-1 truncate">
                    {pl.name}
                    {pl.kind === "host" ? ` (${tr("rooms_host", undefined, "host")})` : ""}
                  </span>
                  {pl.tier && <PingBars compact tier={pl.tier} ms={pl.rttMs} testId="room-details-player-ping" />}
                </li>
              ))}
            </ul>
          )}
          {preview.stream && (
            <p data-testid="room-details-stream">
              <span className={muted}>{tr("rooms_stream", undefined, "Stream")}: </span>
              {streamParts(preview.stream).join(" · ")}
              {preview.stream.hdr ? " · HDR" : ""}
            </p>
          )}
          {carries != null && (
            <p className={carries ? muted : "text-amber-500"} data-testid="room-details-carries">
              {carries
                ? tr("rooms_carries", undefined, "Your connection can carry this stream")
                : tr("rooms_carries_not", { mbps: Math.round((preview.stream?.kbps ?? 0) / 1000) }, `This stream may be too much for your connection (${Math.round((preview.stream?.kbps ?? 0) / 1000)} Mbps)`)}
            </p>
          )}
          {preview.hostLink?.tier && (
            <p className={`text-xs ${muted}`}>
              {tr("rooms_host_ps5", undefined, "Host's PS5")}: {tierLabel(tr, preview.hostLink.tier)}
            </p>
          )}
          {preview.host.playingForMin != null && (
            <p className={`text-xs ${muted}`}>{tr("room_playing_for", { n: preview.host.playingForMin }, `Playing for ${preview.host.playingForMin} min`)}</p>
          )}
          <p className={`text-xs ${muted}`} data-testid="room-details-approval">
            {approvalText(tr, preview.room.approval)}
          </p>
        </>
      )}
      <button
        type="button"
        onClick={onAsk}
        className={
          dark
            ? "w-full rounded-full bg-white py-2.5 text-sm font-semibold text-black"
            : "w-full rounded-full bg-[var(--color-accent)] py-2 text-sm font-semibold text-[var(--color-on-accent,#fff)]"
        }
        data-testid="room-details-ask"
      >
        {askLabel}
      </button>
    </div>
  );
}
