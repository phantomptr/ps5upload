import { OverflowMenu } from "../../../components";
import { useConfirm } from "../../../components/ConfirmDialog";
import { useTr } from "../../../state/lang";
import type { JoinGuest } from "../playTogetherState";
import type { RoomActions } from "./useRoomActions";

type Tr = ReturnType<typeof useTr>;

export type PlayerAction =
  | { kind: "move"; to: number }
  | { kind: "give1" }
  | { kind: "take1" }
  | { kind: "mute_voice"; on: boolean }
  | { kind: "mute_chat"; on: boolean }
  | { kind: "remove" };

/**
 * What the host may do to one player (design 5): move to a free seat, give or take back Player 1
 * (never offered for someone from the public Rooms list, and only with "Friends may get Player 1"
 * on and no picture here), mute voice or chat, remove. Pure, for tests.
 */
export function playerActions(
  g: Pick<JoinGuest, "player" | "status" | "streamSeat" | "room">,
  o: { size: number; taken: number[]; streamSeatAllowed: boolean; pictureHere: boolean; voice: boolean; chat: boolean },
): PlayerAction[] {
  if (g.status === "left") return [];
  const out: PlayerAction[] = [];
  for (let to = 2; to <= Math.max(2, o.size); to++) {
    if (to !== g.player && !o.taken.includes(to)) out.push({ kind: "move", to });
  }
  if (g.player === 1 && g.streamSeat) out.push({ kind: "take1" });
  else if (o.streamSeatAllowed && !o.pictureHere && !g.room.fromRooms && !o.taken.includes(1)) out.push({ kind: "give1" });
  if (o.voice) out.push({ kind: "mute_voice", on: !g.room.muted.voice });
  if (o.chat) out.push({ kind: "mute_chat", on: !g.room.muted.chat });
  out.push({ kind: "remove" });
  return out;
}

function label(tr: Tr, a: PlayerAction): string {
  switch (a.kind) {
    case "move":
      return tr("room_move_to", { n: a.to }, `Move to Player ${a.to}`);
    case "give1":
      return tr("room_give_p1", undefined, "Give Player 1");
    case "take1":
      return tr("room_take_p1", undefined, "Take Player 1 back");
    case "mute_voice":
      return a.on ? tr("room_mute_voice", undefined, "Mute voice") : tr("room_unmute_voice", undefined, "Unmute voice");
    case "mute_chat":
      return a.on ? tr("room_mute_chat", undefined, "Mute chat") : tr("room_unmute_chat", undefined, "Unmute chat");
    case "remove":
      return tr("room_remove", undefined, "Remove…");
  }
}

/** The ⋯ menu on a player's row. */
export function PlayerMenu({
  guest,
  name,
  actions,
  options,
}: {
  guest: JoinGuest;
  name: string;
  actions: RoomActions;
  options: Parameters<typeof playerActions>[1];
}) {
  const tr = useTr();
  const { choose, dialog } = useConfirm();
  const list = playerActions(guest, options);
  if (!list.length) return null;
  const run = async (a: PlayerAction) => {
    switch (a.kind) {
      case "move":
        return actions.seat(guest.player, a.to);
      case "give1":
        return actions.seat(guest.player, 1);
      case "take1":
        return actions.seat(guest.player, "back");
      case "mute_voice":
        return actions.mute({ player: guest.player, voice: a.on });
      case "mute_chat":
        return actions.mute({ player: guest.player, chat: a.on });
      case "remove": {
        // People from the public list: "don't let them back in" is the default (design 5).
        const room = { value: "room", label: tr("room_remove_block_room", undefined, "Remove, and don't let them back in this room") };
        const plain = { value: "none", label: tr("room_remove_plain", undefined, "Remove (they can ask again)") };
        const always = { value: "always", label: tr("room_remove_block_always", undefined, "Remove, and block on this computer") };
        const pick = await choose({
          title: tr("room_remove_title", { name }, `Remove ${name}? They leave the game now.`),
          confirmLabel: tr("room_remove_go", undefined, "Remove"),
          destructive: true,
          choices: guest.room.fromRooms ? [room, plain, always] : [plain, room, always],
        });
        if (pick) await actions.kick(guest.player, pick === "none" ? null : (pick as "room" | "always"));
        return;
      }
    }
  };
  return (
    <>
      <OverflowMenu
        ariaLabel={tr("room_player_menu", { name }, `${name}: more`)}
        items={list.map((a) => ({
          label: label(tr, a),
          destructive: a.kind === "remove",
          separator: a.kind === "remove",
          onSelect: () => void run(a),
          testId: `room-player-${a.kind}${a.kind === "move" ? `-${a.to}` : ""}`,
        }))}
      />
      {dialog}
    </>
  );
}

