import { useCallback, useRef, useState, type ReactNode } from "react";

import { RoomNoticeText, useRoomNoticeTitle } from "../components/RoomNoticeText";
import { acceptNotice, noticeAccepted } from "../lib/room/notice";
import { useTr } from "../state/lang";

/**
 * The responsibility notice on the guest pages (design 7B): a sheet before the first chat, voice or
 * anything from Rooms on this device; "Not now" keeps those off. `show` opens it from "Rules &
 * responsibility" at any time. Accepted once per device (this browser's storage).
 */
export function usePageNotice(): { ask: () => Promise<boolean>; show: () => void; sheet: ReactNode } {
  const tr = useTr();
  const title = useRoomNoticeTitle();
  const [open, setOpen] = useState<null | "ask" | "show">(null);
  const res = useRef<((ok: boolean) => void) | null>(null);
  const done = useCallback((ok: boolean) => {
    if (ok) acceptNotice();
    setOpen(null);
    res.current?.(ok);
    res.current = null;
  }, []);
  const ask = useCallback(() => {
    if (noticeAccepted()) return Promise.resolve(true);
    return new Promise<boolean>((r) => {
      res.current?.(false);
      res.current = r;
      setOpen("ask");
    });
  }, []);
  const show = useCallback(() => setOpen("show"), []);
  const sheet = open ? (
    <div className="fixed inset-0 z-[60] flex items-end justify-center bg-black/70 sm:items-center" data-join-ui="">
      <div role="dialog" aria-modal="true" aria-labelledby="room-notice-title" className="w-full max-w-md space-y-4 rounded-t-2xl bg-neutral-900 p-5 text-white sm:rounded-2xl" data-testid="room-notice">
        <h2 id="room-notice-title" className="text-base font-semibold">
          {title}
        </h2>
        <RoomNoticeText className="space-y-2 text-sm text-white/85" />
        <div className="flex justify-end gap-2">
          {open === "show" && noticeAccepted() ? (
            <button type="button" onClick={() => done(false)} className="rounded-full bg-white px-4 py-2 text-sm font-semibold text-black" data-testid="room-notice-close">
              {tr("rp_phone_close", undefined, "Close")}
            </button>
          ) : (
            <>
              <button type="button" onClick={() => done(false)} className="rounded-full bg-white/15 px-4 py-2 text-sm" data-testid="room-notice-later">
                {tr("room_notice_later", undefined, "Not now")}
              </button>
              <button type="button" onClick={() => done(true)} autoFocus className="rounded-full bg-white px-4 py-2 text-sm font-semibold text-black" data-testid="room-notice-accept">
                {tr("room_notice_accept", undefined, "I understand")}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  ) : null;
  return { ask, show, sheet };
}
