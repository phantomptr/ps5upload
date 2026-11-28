import { useState } from "react";

import { useTr } from "../state/lang";
import { parseJoinLink, type Ps5Target } from "../lib/joinProtocol";
import { freshInvite, freshTarget, saveInvite, sendToPs5, SITE, type HandoffResult, type SavedInvite } from "./ps5Handoff";

type Tr = ReturnType<typeof useTr>;

/** What happened, in words. */
function resultText(tr: Tr, r: HandoffResult): string {
  switch (r) {
    case "ok":
      return tr("join_ps5_sent", undefined, "Sent. Look at your TV.");
    case "expired":
      return tr("join_ps5_expired", undefined, "That code on your TV has expired. Show it again in PS5Upload Play and scan it.");
    case "refused":
      return tr("join_ps5_refused", undefined, "Your PS5 couldn't take that invite.");
    case "hint":
      return tr("join_ps5_scan", undefined, "Open PS5Upload Play on your PS5 and scan the code on its screen with this phone.");
    default:
      return tr("join_ps5_unreachable", undefined, "Couldn't reach your PS5. Try again.");
  }
}

/** One hand-off at a time, and what it said. */
function useHandoff(): [string | null, boolean, (t: Ps5Target | null, inv: SavedInvite) => void, (s: string) => void] {
  const tr = useTr();
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const send = (t: Ps5Target | null, inv: SavedInvite) => {
    if (busy) return;
    setBusy(true);
    setNote(null);
    void sendToPs5(t, inv).then((r) => {
      setBusy(false);
      setNote(resultText(tr, r));
    });
  };
  return [note, busy, send, setNote];
}

const BUTTON = "w-full rounded-full bg-white py-2.5 text-sm font-semibold text-black disabled:opacity-40";

/** The page a QR code on PS5Upload Play's screen opens (`#ps5=…`), or an invite page on the home
 *  network sends the friend to (`#play=<link>`, its own storage is not this site's). */
export function Ps5Page({ target }: { target: Ps5Target | null }) {
  const tr = useTr();
  const [invite, setInvite] = useState(() => freshInvite());
  const [text, setText] = useState("");
  const [note, busy, send] = useHandoff();
  const host = invite?.name ?? tr("join_host_fallback", undefined, "your friend");
  return (
    <div className="flex min-h-[100dvh] items-center justify-center bg-black px-6 text-white" data-testid="ps5-page" data-join-ui="">
      <div className="w-full max-w-sm space-y-4 rounded-2xl border border-white/10 bg-neutral-900 p-6">
        <h1 className="text-lg font-semibold">{tr("join_ps5_play", undefined, "Play on my PS5")}</h1>
        {!target ? (
          <p className="text-sm text-white/60">{resultText(tr, "hint")}</p>
        ) : invite ? (
          <button type="button" disabled={busy} className={BUTTON} onClick={() => send(target, invite)} data-testid="ps5-send">
            {tr("join_ps5_play_game", { host }, `Play ${host}'s game on your PS5`)}
          </button>
        ) : (
          <form
            className="space-y-4"
            onSubmit={(e) => {
              e.preventDefault();
              const inv = saveInvite(text);
              if (!inv) return;
              setInvite(inv);
              send(target, inv);
            }}
          >
            {/* Or the link pasted here: Play takes only an invite link (no words for a wrong one:
                the page's size budget). */}
            <label className="block space-y-1.5">
              <span className="block text-sm text-white/60">
                {tr("join_ps5_open_link", undefined, "Open your friend's invite link on this phone, then tap Play on my PS5 there.")}
              </span>
              <input
                value={text}
                onChange={(e) => setText(e.target.value)}
                inputMode="url"
                className="block w-full rounded-lg border border-white/15 bg-black px-3 py-2 text-base outline-none focus:border-white/50"
                data-testid="ps5-link"
              />
            </label>
            <button type="submit" disabled={busy || !parseJoinLink(text)?.invite} className={BUTTON} data-testid="ps5-paste-send">
              {tr("join_ps5_play", undefined, "Play on my PS5")}
            </button>
          </form>
        )}
        {note && (
          <p className="text-sm text-white/60" data-testid="ps5-note">
            {note}
          </p>
        )}
      </div>
    </div>
  );
}

/** "Play on my PS5" beside Join on an invite page: to the PS5 scanned on this phone, or how to
 *  scan it. A page on the home network (the host's own) goes to the website with the link: the
 *  scanned PS5 is in the website's storage, and only its origin may use the hub. */
export function PlayOnPs5() {
  const tr = useTr();
  const [note, busy, send, setNote] = useHandoff();
  return (
    <>
      <button
        type="button"
        disabled={busy}
        className="w-full rounded-full bg-white/15 px-4 py-2 text-sm"
        data-testid="ps5-play"
        onClick={() => {
          const link = location.href;
          if (location.origin !== SITE) {
            location.href = `${SITE}/join/#play=${encodeURIComponent(link)}`;
            return;
          }
          const inv = saveInvite(link);
          const t = freshTarget();
          if (inv && t) send(t, inv);
          else setNote(resultText(tr, "hint"));
        }}
      >
        {tr("join_ps5_play", undefined, "Play on my PS5")}
      </button>
      {note && (
        <p className="text-center text-sm text-white/60" data-testid="ps5-note">
          {note}
        </p>
      )}
    </>
  );
}
