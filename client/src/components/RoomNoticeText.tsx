import { useTr } from "../state/lang";

/** The Play Rooms responsibility notice's words (design 7B), shared by the app's modal, the guest
 *  pages' sheet and About › Disclaimer. Plain language, all 21 languages. */
export function RoomNoticeText({ className = "space-y-2 text-sm" }: { className?: string }) {
  const tr = useTr();
  return (
    <div className={className} data-testid="room-notice-text">
      <p>
        {tr(
          "room_notice_body",
          undefined,
          "PS5Upload only gives you the tool to play together. We don't run, watch, record or moderate rooms, chat or voice, and we aren't responsible for what anyone says, shares or does in them, including names and room names. You're responsible for your own conduct and for following the law and the rules of the platforms and games you use. Only play with people you trust; you can mute, hide, remove or block anyone.",
        )}
      </p>
      <p>{tr("room_notice_parents", undefined, "Parents: please supervise children who use Play Rooms.")}</p>
      <p>
        {tr(
          "room_notice_safety",
          undefined,
          "Stay safe: don't share personal details, passwords or account information with people you don't know, and leave any room that makes you uncomfortable. Chat and voice go only between the people in the room and aren't saved.",
        )}
      </p>
    </div>
  );
}

/** The notice's title. */
export function useRoomNoticeTitle(): string {
  const tr = useTr();
  return tr("room_notice_title", undefined, "Play Rooms: you're responsible for what you say and do");
}
