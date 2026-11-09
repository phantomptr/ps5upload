import { useEffect, useState } from "react";
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

export { NAME_KEY, defaultName } from "./guestName";

/** A saved place for this invite (a reload while joined): go straight back in. */
function hasPlace(invite: string | null): boolean {
  try {
    return !!sessionStorage.getItem(tokenKey(invite));
  } catch {
    return false;
  }
}

/** Fill the screen and hold it sideways, where the browser allows (Android Chrome does). */
async function pageFullscreen(on: boolean): Promise<void> {
  try {
    if (on) {
      await document.documentElement.requestFullscreen?.({ navigationUI: "hide" });
      await (screen.orientation as ScreenOrientation & { lock?: (o: string) => Promise<void> }).lock?.("landscape");
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
}) {
  const tr = useTr();
  const hubDown = useHubDown(transport && internet !== false ? url : null, lanUrl);
  const [name, setName] = useState(() => lastName(defaultName(typeof navigator === "undefined" ? "" : navigator.userAgent)));
  const [session, setSession] = useState<JoinSession | null>(null);
  const [round, setRound] = useState(0);

  const go = (n: string) => {
    // Remembered on this device: the next invite has it filled in.
    const clean = rememberName(n);
    if (!clean) return;
    const s = new JoinSession({ url, invite, name: clean, host, transport, path, ...(internet === undefined ? {} : { internet }) });
    setSession(s);
    setRound((r) => r + 1);
    s.start();
  };

  // A reload while playing resumes the place without asking again.
  useEffect(() => {
    if (!session && hasPlace(invite) && name.trim()) go(name);
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
          go(name);
        }}
        onFullscreen={pageFullscreen}
      />
    );
  }

  const shownHost = host ?? tr("join_host_fallback", undefined, "your friend");
  const nameStep = (
    <div className="flex min-h-[100dvh] items-center justify-center bg-black px-6 text-white" data-testid="join-name-step">
      <form
        className="w-full max-w-sm space-y-4 rounded-2xl border border-white/10 bg-neutral-900 p-6"
        onSubmit={(e) => {
          e.preventDefault();
          go(name);
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
        <button
          type="submit"
          disabled={!name.trim()}
          className="w-full rounded-full bg-white py-2.5 text-sm font-semibold text-black disabled:opacity-40"
          data-testid="join-go"
        >
          {tr("join_button", undefined, "Join")}
        </button>
        {hubDown && lanUrl && <OpenOnWifi href={lanUrl} />}
      </form>
    </div>
  );
  // A link opened in a browser: offer the app first (Slack-style); a typed code has no link. The
  // app joins an internet invite too (state/joinSession), so it is offered for every link.
  return invite ? <OpenInAppGate appLink={joinAppLink(location.href)}>{nameStep}</OpenInAppGate> : nameStep;
}
