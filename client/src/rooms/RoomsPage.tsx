import { useMemo, useState } from "react";
import { LayoutGrid } from "lucide-react";

import { RoomsBrowserCore, Sheet } from "../components/RoomsBrowserCore";
import { usePageNotice } from "../join/PageNotice";
import { roomFragment } from "../join/roomFragment";
import { DEFAULT_HUB, SITE } from "../join/ps5Handoff";
import { joinAppLink } from "../lib/joinProtocol";
import { ownRegion } from "../lib/room/regions";
import { APP_VERSION } from "../lib/room/version";
import type { RoomCard } from "../lib/room/rooms";
import { useTr } from "../state/lang";

/** A PS5's own browser: it can't play from a web page; PS5Upload Play can. */
export function onPs5Browser(ua: string): boolean {
  return /PlayStation 5/i.test(ua);
}

/** Where a card's room goes: this site's join page with the room in the fragment. */
export function joinUrl(card: Pick<RoomCard, "room" | "host" | "tag" | "pw">, hub: string, base: string = SITE): string {
  return `${base}/join/${roomFragment({ lobby: card.room, hub, host: card.host, tag: card.tag, pw: card.pw, via: "rooms" })}`;
}

/**
 * The website's Rooms page: the list (RoomsBrowserCore) and, for a room, the three ways in:
 * Join in this browser, Play on my PS5 (the PS5 scanned on this phone, the join page's own
 * hand-off), Open in PS5Upload.
 */
export function RoomsPage() {
  const tr = useTr();
  const notice = usePageNotice();
  const [pick, setPick] = useState<RoomCard | null>(null);
  const me = useMemo(() => ownRegion(), []);
  const hub = DEFAULT_HUB;
  const ps5 = typeof navigator !== "undefined" && onPs5Browser(navigator.userAgent);
  const [first, setFirst] = useState(() => {
    try {
      return localStorage.getItem("ps5upload.rooms.seen") !== "1";
    } catch {
      return false;
    }
  });
  return (
    <div className="min-h-[100dvh] bg-black px-4 py-6 text-white" data-join-ui="">
      <div className="mx-auto max-w-2xl space-y-4">
        <header className="space-y-1">
          <h1 className="flex items-center gap-2 text-xl font-semibold">
            <LayoutGrid size={20} aria-hidden />
            {tr("rooms_title", undefined, "Rooms")}
          </h1>
          <p className="text-sm text-white/60">{tr("rooms_page_intro", undefined, "Open Play Rooms you can ask to join. The host decides who plays.")}</p>
        </header>
        {first && (
          <div className="flex items-start gap-3 rounded-lg bg-white/10 p-3 text-sm" data-testid="rooms-first-run">
            <p className="flex-1">{tr("rooms_first_run", undefined, "Rooms are open games you can ask to join. The host decides who plays.")}</p>
            <button
              type="button"
              className="rounded-full bg-white px-3 py-1 text-xs font-semibold text-black"
              onClick={() => {
                try {
                  localStorage.setItem("ps5upload.rooms.seen", "1");
                } catch {
                  // Shown again next time: harmless.
                }
                setFirst(false);
              }}
            >
              {tr("rooms_got_it", undefined, "Got it")}
            </button>
          </div>
        )}
        {ps5 ? (
          <p className="rounded-lg bg-white/10 p-4 text-sm" data-testid="rooms-on-ps5">
            {tr(
              "rooms_on_ps5",
              undefined,
              "On a PS5? Play on the TV with PS5Upload Play: install it from ProsperoStore or from PS5Upload, open it and pick Join. The same rooms are there.",
            )}
          </p>
        ) : (
          <RoomsBrowserCore
            dark
            source={{ base: `https://${hub}`, viaEngine: false }}
            me={me}
            version={APP_VERSION}
            defaultName=""
            knockForm={false}
            ensureNotice={notice.ask}
            onJoin={(c) => setPick(c)}
          />
        )}
        <button type="button" className="text-xs text-white/60 underline" onClick={notice.show} data-testid="room-rules">
          {tr("room_notice_link", undefined, "Rules & responsibility")}
        </button>
      </div>
      {pick && (
        <Sheet dark onClose={() => setPick(null)}>
          <div className="space-y-3" data-testid="rooms-ways">
            <h2 className="text-base font-semibold">{pick.name}</h2>
            <a href={joinUrl(pick, hub, location.origin)} className="block w-full rounded-full bg-white py-2.5 text-center text-sm font-semibold text-black" data-testid="rooms-join-browser">
              {tr("rooms_join_browser", undefined, "Join in this browser")}
            </a>
            <a
              href={`${location.origin}/join/#play=${encodeURIComponent(joinUrl(pick, hub, location.origin))}`}
              className="block w-full rounded-full bg-white/15 py-2.5 text-center text-sm"
              data-testid="rooms-join-ps5"
            >
              {tr("join_ps5_play", undefined, "Play on my PS5")}
            </a>
            <a href={joinAppLink(joinUrl(pick, hub, location.origin))} className="block w-full rounded-full bg-white/15 py-2.5 text-center text-sm" data-testid="rooms-join-app">
              {tr("rooms_open_app", undefined, "Open in PS5Upload")}
            </a>
          </div>
        </Sheet>
      )}
      {notice.sheet}
    </div>
  );
}
