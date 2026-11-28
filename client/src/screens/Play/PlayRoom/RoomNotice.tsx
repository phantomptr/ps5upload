import { useCallback, useRef, useState, type ReactNode } from "react";
import { ShieldCheck } from "lucide-react";

import { Button, Modal } from "../../../components";
import { RoomNoticeText, useRoomNoticeTitle } from "../../../components/RoomNoticeText";
import { acceptNotice, noticeAccepted, withNotice, type NoticeFor } from "../../../lib/room/notice";
import { useTr } from "../../../state/lang";

/**
 * The responsibility notice as a modal (design 7B): `ask` before the first chat, voice or public
 * room (true once accepted; "Not now" keeps the feature off), `show` from "Rules & responsibility"
 * at any time. `run(what, go)` asks only when needed, then runs `go`.
 */
export function useRoomNotice(): {
  ask: (what: NoticeFor) => Promise<boolean>;
  run: (what: NoticeFor, go: () => void | Promise<void>) => Promise<boolean>;
  show: () => void;
  dialog: ReactNode;
} {
  const tr = useTr();
  const title = useRoomNoticeTitle();
  const [open, setOpen] = useState<null | "ask" | "show">(null);
  const resolver = useRef<((ok: boolean) => void) | null>(null);
  const finish = useCallback((ok: boolean) => {
    if (ok) acceptNotice();
    setOpen(null);
    resolver.current?.(ok);
    resolver.current = null;
  }, []);
  const ask = useCallback((): Promise<boolean> => {
    if (noticeAccepted()) return Promise.resolve(true);
    return new Promise<boolean>((resolve) => {
      resolver.current?.(false);
      resolver.current = resolve;
      setOpen("ask");
    });
  }, []);
  const run = useCallback((what: NoticeFor, go: () => void | Promise<void>) => withNotice(what, ask, go), [ask]);
  const show = useCallback(() => setOpen("show"), []);
  const accepted = noticeAccepted();
  const dialog = open ? (
    <Modal
      open
      onClose={() => finish(false)}
      title={title}
      titleIcon={<ShieldCheck size={16} />}
      size="md"
      footer={
        open === "show" && accepted ? (
          <Button variant="primary" onClick={() => finish(false)} data-testid="room-notice-close">
            {tr("rp_phone_close", undefined, "Close")}
          </Button>
        ) : (
          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="ghost" onClick={() => finish(false)} data-testid="room-notice-later">
              {tr("room_notice_later", undefined, "Not now")}
            </Button>
            <Button variant="primary" onClick={() => finish(true)} data-testid="room-notice-accept">
              {tr("room_notice_accept", undefined, "I understand")}
            </Button>
          </div>
        )
      }
    >
      <div data-testid="room-notice">
        <RoomNoticeText />
      </div>
    </Modal>
  ) : null;
  return { ask, run, show, dialog };
}

/** "Rules & responsibility": the notice again, from any room screen. */
export function RulesLink({ onShow }: { onShow: () => void }) {
  const tr = useTr();
  return (
    <button type="button" onClick={onShow} className="text-xs text-[var(--color-muted)] underline hover:text-[var(--color-text)]" data-testid="room-rules">
      {tr("room_notice_link", undefined, "Rules & responsibility")}
    </button>
  );
}
