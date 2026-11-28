import { useCallback, useEffect, useState } from "react";
import { Users } from "lucide-react";

import { useTr } from "../state/lang";
import { tokenKey } from "../lib/joinClient";
import { joinAppLink } from "../lib/joinProtocol";
import { OpenInAppGate } from "../components/OpenInAppGate";
import { JoinView } from "./JoinView";
import { holdWhileJoined, JoinSession } from "./session";
import type { JoinTransport } from "./rtcTransport";
import type { NetPath } from "./netPath";
import { defaultName, lastName, rememberName } from "./guestName";
import { OpenOnWifi, useHubDown } from "./OpenOnWifi";
import { PlayOnPs5 } from "./Ps5Page";
import { sidewaysNow } from "../lib/pageFill";
import { fetchStatus } from "../lib/room/hubRooms";
import { knockHello } from "../lib/room/knock";
import { deviceId } from "../lib/room/played";
import { APP_VERSION } from "../lib/room/version";
import { rtcTransport } from "./rtcTransport";
import { RoomOverlay } from "./RoomOverlay";
import { usePageNotice } from "./PageNotice";
import type { RoomFragment } from "./roomFragment";

export { NAME_KEY, defaultName } from "./guestName";

/** A saved place for this invite (a reload while joined): go straight back in. */
function hasPlace(invite: string | null): boolean {
  try {
    return !!sessionStorage.getItem(tokenKey(invite));
  } catch {
    return false;
  }
}

/** Fill the screen, where the browser allows (Android Chrome does). From upright (the Full screen
 *  button) it also holds the phone sideways; from a turn it does not, so turning upright again
 *  can leave. */
async function pageFullscreen(on: boolean): Promise<void> {
  try {
    if (on) {
      const upright = !sidewaysNow();
      if (!document.fullscreenElement) await document.documentElement.requestFullscreen?.({ navigationUI: "hide" });
      if (upright) await (screen.orientation as ScreenOrientation & { lock?: (o: string) => Promise<void> }).lock?.("landscape");
    } else if (document.fullscreenElement) {
      await document.exitFullscreen();
    }
  } catch {
    // Not here (iPhone Safari): the page still fills the window.
  }
}

/**
 * The /join page a friend opens from the host's QR code or link: their name, then the wait for
 * the host's yes, then the game (JoinView).
 */
export function JoinFlow({
  url,
  invite,
  host,
  transport,
  lanUrl = null,
  path = null,
  internet,
  room = null,
}: {
  url: string;
  invite: string | null;
  host: string | null;
  /** An internet invite's WebRTC transport (rtcTransport.ts); absent: the LAN WebSocket. */
  transport?: JoinTransport;
  /** An internet invite's page on the host's Wi-Fi (lanHint.ts), offered when the hub can't be
   *  reached; null: the link names none (an older host). */
  lanUrl?: string | null;
  /** The way the plain socket goes (join/netPath hostPath of this page's address). */
  path?: NetPath | null;
  /** An internet invite (absent: one when it has a transport). */
  internet?: boolean;
  /** Play Rooms: a room to knock on (from the website's Rooms page), no invite. */
  room?: RoomFragment | null;
}) {
  const tr = useTr();
  const hubDown = useHubDown(transport && internet !== false ? url : null, lanUrl);
  const [name, setName] = useState(() => lastName(defaultName(typeof navigator === "undefined" ? "" : navigator.userAgent)));
  const [session, setSession] = useState<JoinSession | null>(null);
  const [round, setRound] = useState(0);
  const notice = usePageNotice();
  const [password, setPassword] = useState("");
  // The room's key (to seal the password), from the hub's lookup of this room.
  const [pk, setPk] = useState<string | null>(null);
  useEffect(() => {
    if (!room?.pw) return;
    let alive = true;
    void fetchStatus({ base: `https://${room.hub}`, viaEngine: false }, [room.lobby]).then((r) => {
      if (alive && typeof r === "object") setPk(r.rooms[room.lobby]?.pk ?? null);
    });
    return () => {
      alive = false;
    };
  }, [room]);
  // When the name step shows (after the "Open in the app?" question too), a TV's remote or a
  // keyboard starts on Join, or on the name when there is none yet (#926). Not the name on a
  // touch screen, where its keyboard would cover the page.
  const form = useCallback((el: HTMLFormElement | null) => {
    if (!el) return;
    const touch = typeof window !== "undefined" && !!window.matchMedia?.("(pointer: coarse)").matches;
    const named = !!el.querySelector<HTMLInputElement>('[data-testid="join-name"]')?.value.trim();
    const id = named ? "join-go" : touch ? null : "join-name";
    if (id) el.querySelector<HTMLElement>(`[data-testid="${id}"]`)?.focus({ preventScroll: true });
  }, []);

  const go = async (n: string, pw: string = password) => {
    // Remembered on this device: the next invite has it filled in.
    const clean = rememberName(n);
    if (!clean) return;
    // A room from Rooms: the responsibility notice first, once per device (design 7B).
    if (room && room.via === "rooms" && !(await notice.ask())) return;
    const t = room
      ? rtcTransport(
          { room: room.lobby, key: "", fp: "", hub: room.hub },
          {
            video: true,
            log: (m) => console.info(`[join] ${m}`),
            knock: {
              hello: knockHello({ via: room.via, name: clean, device: deviceId(), client: "web", ver: APP_VERSION }),
              secret: room.pw && pw ? { pw } : null,
              pk,
            },
          },
        )
      : transport;
    const s = new JoinSession({
      url,
      invite,
      name: clean,
      host,
      transport: t,
      path,
      ...(room ? { internet: true, ...(room.tag ? { hostTag: room.tag } : {}) } : internet === undefined ? {} : { internet }),
    });
    setSession(s);
    setRound((r) => r + 1);
    s.start();
  };

  // A reload while playing resumes the place without asking again.
  useEffect(() => {
    if (!session && !room && hasPlace(invite) && name.trim()) void go(name);
    // Once, on load.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // This page has no store: the session is its own.
  useEffect(() => (session ? holdWhileJoined(session) : undefined), [session]);

  useEffect(() => {
    if (!session) return;
    const away = () => session.stop();
    window.addEventListener("pagehide", away);
    return () => window.removeEventListener("pagehide", away);
  }, [session]);

  if (session) {
    return (
      <JoinView
        key={round}
        session={session}
        lanUrl={lanUrl}
        hubDown={hubDown}
        onLeave={() => setSession(null)}
        onAgain={() => {
          session.stop();
          void go(name);
        }}
        onAgainWith={(o) => {
          session.stop();
          if (o.name) setName(o.name);
          if (o.password) setPassword(o.password);
          void go(o.name ?? name, o.password ?? password);
        }}
        onFullscreen={pageFullscreen}
        roomExtras={
          <>
            <RoomOverlay
              link={session.room}
              ownRttMs={null}
              ownTier={null}
              ensureNotice={() => notice.ask()}
              onRules={notice.show}
              touch={typeof window !== "undefined" && !!window.matchMedia?.("(pointer: coarse)").matches}
            />
            {notice.sheet}
          </>
        }
      />
    );
  }

  const shownHost = host ?? tr("join_host_fallback", undefined, "your friend");
  const nameStep = (
    <div className="flex min-h-[100dvh] items-center justify-center bg-black px-6 text-white" data-testid="join-name-step" data-join-ui="">
      <form
        ref={form}
        className="w-full max-w-sm space-y-4 rounded-2xl border border-white/10 bg-neutral-900 p-6"
        onSubmit={(e) => {
          e.preventDefault();
          void go(name);
        }}
      >
        <div className="flex items-center gap-3">
          <span className="flex h-10 w-10 items-center justify-center rounded-full bg-white/10">
            <Users size={20} />
          </span>
          <div>
            <h1 className="text-lg font-semibold">{tr("join_title", undefined, "Join a friend's game")}</h1>
            <p className="text-sm text-white/60">{tr("join_title_body", { host: shownHost }, `Play on ${shownHost}'s PS5 as Player 2, 3 or 4.`)}</p>
          </div>
        </div>
        <label className="block space-y-1.5">
          <span className="text-sm font-medium">{tr("join_name_label", undefined, "Your name")}</span>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            maxLength={32}
            autoComplete="nickname"
            className="block w-full rounded-lg border border-white/15 bg-black px-3 py-2 text-base outline-none focus:border-white/50"
            data-testid="join-name"
          />
          <span className="block text-xs text-white/50">{tr("join_name_hint", undefined, "Your friend sees this when you ask to join.")}</span>
        </label>
        {room?.pw && (
          <label className="block space-y-1.5">
            <span className="text-sm font-medium">{tr("room_password", undefined, "Password")}</span>
            <input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              maxLength={64}
              autoComplete="current-password"
              className="block w-full rounded-lg border border-white/15 bg-black px-3 py-2 text-base outline-none focus:border-white/50"
              data-testid="join-password"
            />
          </label>
        )}
        <button
          type="submit"
          disabled={!name.trim() || (!!room?.pw && password.length < 4)}
          className="w-full rounded-full bg-white py-2.5 text-sm font-semibold text-black disabled:opacity-40"
          data-testid="join-go"
        >
          {tr("join_button", undefined, "Join")}
        </button>
        {(invite || room) && <PlayOnPs5 />}
        {hubDown && lanUrl && <OpenOnWifi href={lanUrl} />}
        {room && (
          <button type="button" className="block w-full text-center text-xs text-white/60 underline" onClick={notice.show} data-testid="room-rules">
            {tr("room_notice_link", undefined, "Rules & responsibility")}
          </button>
        )}
        <p className="text-center text-[11px] leading-snug text-white/40" data-testid="join-legal">
          {tr(
            "join_legal",
            undefined,
            "PS5Upload is free, unofficial and not affiliated with Sony. You're responsible for what you say and do while you play.",
          )}
        </p>
      </form>
      {notice.sheet}
    </div>
  );
  // A link opened in a browser: offer the app first (Slack-style); a typed code has no link. The
  // app joins an internet invite too (state/joinSession), so it is offered for every link.
  return invite ? <OpenInAppGate appLink={joinAppLink(location.href)}>{nameStep}</OpenInAppGate> : nameStep;
}
