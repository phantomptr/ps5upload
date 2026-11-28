import { ArrowLeft } from "lucide-react";
import { t } from "../strings";

/**
 * Play Rooms' responsibility notice (the rooms design 7B, the same words as PS5Upload and PS5Upload
 * Play), reachable from Settings › About and from the "Your room" card. Read only: accepting it
 * happens where rooms are made and joined, not on the console page.
 */
export function RoomRules({ onBack }: { onBack?: () => void }) {
  return (
    <div className="cw-settings cw-doc" data-page="room-rules">
      {onBack ? (
        <button type="button" className="cw-focusable cw-button cw-doc-back" data-action="back" onClick={onBack}>
          <ArrowLeft aria-hidden />
          <span>{t("cw_nav_settings")}</span>
        </button>
      ) : null}
      <h1 className="cw-h1">{t("cw_room_notice_title")}</h1>
      <p>{t("cw_room_notice_body")}</p>
      <p>{t("cw_room_notice_parents")}</p>
      <h2 className="cw-h2">{t("cw_about_disclaimer_title")}</h2>
      <p className="cw-dim">{t("cw_about_disclaimer")}</p>
    </div>
  );
}
