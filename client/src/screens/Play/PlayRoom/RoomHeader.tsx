import { Lock, LockOpen, MicOff, Timer, X } from "lucide-react";

import { Button } from "../../../components";
import { PingBars } from "../../../components/PingBars";
import { useConfirm } from "../../../components/ConfirmDialog";
import { minutesSince, peopleIn, type RoomInfo } from "../../../lib/room/room";
import { useTr } from "../../../state/lang";
import { friendsIn, type JoinHostState } from "../playTogetherState";
import { Chip, notListedText, visibilityText } from "./roomControls";
import { RulesLink } from "./RoomNotice";
import type { RoomActions } from "./useRoomActions";

type Tr = ReturnType<typeof useTr>;

/** "Close your room? Sam and Mia leave the game." (design 5): the names, joined in this
 *  language's way, or just the question with nobody in. */
export function closeQuestion(tr: Tr, names: string[], lang = "en"): string {
  if (!names.length) return tr("room_close_q_empty", undefined, "Close your room?");
  let list: string;
  try {
    // Intl.ListFormat (ES2021) where the runtime has it; the app targets an older lib.
    const LF = (Intl as unknown as { ListFormat?: new (l: string, o: object) => { format: (x: string[]) => string } }).ListFormat;
    list = LF ? new LF(lang, { style: "long", type: "conjunction" }).format(names) : names.join(", ");
  } catch {
    list = names.join(", ");
  }
  return names.length === 1
    ? tr("room_close_q_one", { name: list }, `Close your room? ${list} leaves the game.`)
    : tr("room_close_q", { names: list }, `Close your room? ${list} leave the game.`);
}

/** The listed state in words (Friends and Public only). */
export function listedText(tr: Tr, room: RoomInfo): string | null {
  if (room.visibility === "private") return null;
  if (room.locked) return tr("room_locked_note", undefined, "No one new can ask");
  switch (room.listed.state) {
    case "listed":
      return room.visibility === "public" ? tr("room_listed", undefined, "Listed in Rooms") : tr("room_listed_friends", undefined, "Friends can see it's open");
    case "listing":
      return tr("room_listing", undefined, "Listing…");
    case "refused":
    case "old_hub":
      return notListedText(tr, room.listed.state === "old_hub" ? "old_hub" : room.listed.why);
    default:
      return null;
  }
}

/**
 * "Your room · <name>" with its chips (visibility, password, locked), the facts line (game,
 * players, listed, chat), the host's own PS5 link, and Lock / Close room (design 7.1).
 */
export function RoomHeader({
  state,
  actions,
  onClose,
  onRules,
  lang,
  now,
}: {
  state: JoinHostState;
  actions: RoomActions;
  /** Close the room (after the confirm). */
  onClose: () => void;
  onRules: () => void;
  lang: string;
  now: number;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const room = state.room;
  const vis = visibilityText(tr, room.visibility);
  const inRoom = peopleIn(state.guests);
  const full = inRoom >= room.size;
  const listed = listedText(tr, room);
  const mins = minutesSince(room.openedAt, now);
  const name = room.name || tr("room_your_room", undefined, "Your room");
  const close = async () => {
    const names = friendsIn(state).map((g) => g.name || tr("pt_friend_fallback", undefined, "Friend"));
    const ok = await confirm({
      title: closeQuestion(tr, names, lang),
      confirmLabel: tr("room_close", undefined, "Close room"),
      cancelLabel: tr("room_keep_open", undefined, "Keep it open"),
      destructive: names.length > 0,
    });
    if (ok) onClose();
  };
  return (
    <div className="space-y-1.5" data-testid="room-header">
      <div className="flex flex-wrap items-center gap-2">
        <h3 id="pt-panel-title" className="min-w-0 truncate text-sm font-semibold" data-testid="room-title">
          {tr("room_your_room_named", { name }, `Your room · ${name}`)}
        </h3>
        <Chip testId="room-chip-visibility">{vis.label}</Chip>
        {room.password && (
          <Chip testId="room-chip-password">
            <Lock size={11} aria-hidden />
            {tr("room_password", undefined, "Password")}
          </Chip>
        )}
        {room.locked && (
          <Chip tone="warn" testId="room-chip-locked">
            {tr("room_locked", undefined, "Locked")}
          </Chip>
        )}
        {full && (
          <Chip tone="warn" testId="room-chip-full">
            {tr("room_full_n", { n: inRoom, max: room.size }, `Full (${inRoom} of ${room.size})`)}
          </Chip>
        )}
      </div>
      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-[var(--color-muted)]" data-testid="room-facts">
        {room.game && <span data-testid="room-game">{room.game.title}</span>}
        <span data-testid="room-players-count">{tr("room_players_of", { n: inRoom, max: room.size }, `${inRoom} of ${room.size} players`)}</span>
        {listed && <span data-testid="room-listed" data-state={room.listed.state}>{listed}</span>}
        {mins != null && <span>{tr("room_playing_for", { n: mins }, `Playing for ${mins} min`)}</span>}
        {room.known && room.chat !== "on" && (
          <span>{room.chat === "off" ? tr("room_chat_is_off", undefined, "Chat is off") : tr("room_chat_quick_only", undefined, "Chat: quick only")}</span>
        )}
      </p>
      {room.hostLink && (
        <p className="flex items-center gap-2 text-xs" data-testid="room-host-link">
          <span className="text-[var(--color-muted)]">{tr("room_your_ps5_link", undefined, "Your PS5 link")}</span>
          <PingBars tier={room.hostLink.tier} ms={room.hostLink.rttMs} testId="room-host-link-bars" />
        </p>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {room.known && (
          <Button
            size="sm"
            variant="ghost"
            leftIcon={room.locked ? <LockOpen size={14} /> : <Lock size={14} />}
            onClick={() => void actions.lock(!room.locked)}
            data-testid="room-lock"
          >
            {room.locked ? tr("room_unlock", undefined, "Unlock") : tr("room_lock", undefined, "Lock room")}
          </Button>
        )}
          {room.known && room.voice !== "off" && friendsIn(state).length > 0 && (
            <Button size="sm" variant="ghost" leftIcon={<MicOff size={14} />} onClick={() => void actions.mute({ all: true, voice: true })} data-testid="room-mute-all">
              {tr("room_mute_all", undefined, "Mute all")}
            </Button>
          )}
          {room.known && friendsIn(state).length > 0 && !room.ready && (
            <Button size="sm" variant="ghost" leftIcon={<Timer size={14} />} onClick={() => void actions.ready()} data-testid="room-ready-check">
              {tr("room_ready_check", undefined, "Ready check")}
            </Button>
          )}
          {room.ready && (
            <span className="text-xs" data-testid="room-ready-running">
              {room.ready.all
                ? tr("room_ready_all", undefined, "Everyone's ready")
                : tr("room_ready_running", { n: room.ready.ready.length, s: Math.ceil(room.ready.leftS) }, `Ready? ${room.ready.ready.length} ready · ${Math.ceil(room.ready.leftS)} s`)}
            </span>
          )}
          <Button size="sm" variant="secondary" leftIcon={<X size={14} />} onClick={() => void close()} data-testid="pt-close">
            {tr("room_close", undefined, "Close room")}
          </Button>
          <RulesLink onShow={onRules} />
      </div>
      {dialog}
    </div>
  );
}
