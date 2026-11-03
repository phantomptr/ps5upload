import { useEffect, useRef, useState } from "react";
import { Expand, Gamepad2, LogOut, Minimize, RotateCw, Users, Volume2 } from "lucide-react";

import type { JoinStage } from "../lib/joinClient";
import { delayTone, playerColor } from "../lib/joinProtocol";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { gamepadsOf } from "../lib/nativePad";
import { TouchControls } from "../screens/Play/TouchControls";
import { useTr } from "../state/lang";
import { devicePad, useJoinSession, type JoinSession } from "./session";

type Tr = ReturnType<typeof useTr>;

/** Set once the guide card ("press PS, pick Quick Play") was dismissed. */
const GUIDED_KEY = "ps5upload.join.guided";

/** 42 → "0:42". */
export function clock(s: number): string {
  const t = Math.max(0, Math.ceil(s));
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
}

/** The message for a guest who is not (or no longer) in the game. */
export function endText(tr: Tr, stage: JoinStage, host: string): string {
  if (stage.kind === "refused") {
    switch (stage.why) {
      case "key":
        return tr("join_refused_key", { host }, `This invite has expired. Ask ${host} for a new link.`);
      case "full":
        return tr("join_refused_full", undefined, "The game is full: four players already.");
      case "denied":
        return tr("join_refused_denied", { host }, `${host} didn't let you in this time.`);
      case "closed":
        return tr("join_refused_closed", { host }, `${host} isn't hosting Play Together right now.`);
      case "old_helper":
        return tr("join_refused_old_helper", { host }, `${host}'s PS5 needs an updated helper for Play Together.`);
      case "not_playing":
        return tr("join_refused_not_playing", { host }, `${host} isn't playing right now. Ask ${host} to start Play first.`);
      default:
        return tr("join_refused_other", { host }, `${host} didn't let you in.`);
    }
  }
  if (stage.kind === "ended") {
    switch (stage.why) {
      case "kicked":
        return tr("join_ended_kicked", { host }, `${host} took you out of the game.`);
      case "closed":
        return tr("join_ended_closed", { host }, `${host} closed Play Together.`);
      case "host_stopped":
        return tr("join_ended_host_stopped", undefined, "The host stopped playing.");
      case "gave_up":
        return tr("join_ended_gave_up", { host }, `The connection to ${host} didn't come back in time.`);
      case "lost":
        return tr("join_ended_lost", { host }, `Couldn't reach ${host}. Check that you're on the same network.`);
      case "left":
        return tr("join_ended_left", undefined, "You left the game.");
    }
  }
  return "";
}

/** A ticking clock while `on` (for countdowns). */
function useNow(on: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!on) return;
    setNow(Date.now());
    const t = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(t);
  }, [on]);
  return now;
}

/** A controller connected to this device, and whether a button is held (the test press). */
function usePadReady(): { ready: boolean; pressed: boolean } {
  const [state, setState] = useState({ ready: false, pressed: false });
  useEffect(() => {
    const read = () => {
      const ready = gamepadsOf(typeof navigator === "undefined" ? undefined : navigator).some((g) => !!g?.connected);
      const p = ready ? devicePad() : null;
      const pressed = !!p && (p.buttons !== 0 || p.l2 > 30 || p.r2 > 30);
      setState((s) => (s.ready === ready && s.pressed === pressed ? s : { ready, pressed }));
    };
    read();
    const t = window.setInterval(read, 120);
    window.addEventListener("gamepadconnected", read);
    window.addEventListener("gamepaddisconnected", read);
    return () => {
      window.clearInterval(t);
      window.removeEventListener("gamepadconnected", read);
      window.removeEventListener("gamepaddisconnected", read);
    };
  }, []);
  return state;
}

const coarsePointer = () => typeof window !== "undefined" && !!window.matchMedia?.("(pointer: coarse)").matches;

const TONE_COLOR = {
  good: "var(--color-good,#32d074)",
  fair: "var(--color-warn,#ffb340)",
  slow: "var(--color-bad,#ff6961)",
  none: "rgba(255,255,255,.5)",
} as const;

/** "Waiting for {host} to let you in", calmly. */
function Waiting({ text, body, onCancel }: { text: string; body?: string; onCancel: () => void }) {
  const tr = useTr();
  return (
    <div className="flex flex-col items-center gap-5 px-6 py-10 text-center" data-testid="join-waiting">
      <div className="relative flex h-24 w-24 items-center justify-center" aria-hidden>
        <span className="absolute inset-0 animate-ping rounded-full bg-white/10 [animation-duration:2.4s]" />
        <span className="absolute inset-3 animate-pulse rounded-full bg-white/10 [animation-duration:2.4s]" />
        <span className="relative flex h-14 w-14 items-center justify-center rounded-full bg-white/15">
          <Users size={26} />
        </span>
      </div>
      <p className="max-w-sm text-base font-medium" data-testid="join-stage">
        {text}
      </p>
      {body && <p className="max-w-sm text-sm text-white/60">{body}</p>}
      <button
        type="button"
        onClick={onCancel}
        className="rounded-full bg-white/15 px-4 py-2 text-sm hover:bg-white/25"
        data-testid="join-cancel"
      >
        {tr("join_cancel", undefined, "Cancel")}
      </button>
    </div>
  );
}

/**
 * A friend's game (Play Together, #641), joined as Player 2, 3 or 4: the picture, this device's
 * controller (a connected one, else the on-screen one on a touch screen), the delay and the way
 * out. The /join page shows it full window; the app shows it in the Play screen's picture box.
 */
export function JoinView({
  session,
  inApp = false,
  onLeave,
  onAgain,
  onFullscreen,
}: {
  session: JoinSession;
  inApp?: boolean;
  /** After Leave or Cancel. */
  onLeave: () => void;
  /** Ask to join again (a new ask, no saved place). */
  onAgain: () => void;
  /** Full screen on or off; the page asks the browser, the app its window. */
  onFullscreen?: (on: boolean) => Promise<void> | void;
}) {
  const tr = useTr();
  const snap = useJoinSession(session);
  const { stage, status } = snap;
  const host = snap.host ?? tr("join_host_fallback", undefined, "your friend");
  const screen = useRef<HTMLDivElement>(null);
  const [full, setFull] = useState(false);
  const [guided, setGuided] = useState(() => safeGetItem(GUIDED_KEY) === "1");
  const [portrait, setPortrait] = useState(() => typeof window !== "undefined" && window.innerHeight > window.innerWidth);
  const pad = usePadReady();
  const reconnecting = stage.kind === "reconnecting";
  const now = useNow(reconnecting);
  const live = stage.kind === "joined" || reconnecting;

  useEffect(() => {
    if (!live) return;
    const el = session.element;
    screen.current?.appendChild(el);
    return () => el.remove();
  }, [live, session]);

  useEffect(() => {
    const update = () => setPortrait(window.innerHeight > window.innerWidth);
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, []);

  // Leaving the view leaves full screen.
  useEffect(() => {
    if (!full) return;
    return () => void onFullscreen?.(false);
  }, [full, onFullscreen]);

  const leave = () => {
    session.leave();
    onLeave();
  };

  if (stage.kind === "connecting" || stage.kind === "asking") {
    return (
      <div className={inApp ? "rounded-[var(--radius-card)] bg-neutral-900 text-white" : "flex min-h-[100dvh] items-center justify-center bg-black text-white"}>
        <Waiting
          text={
            stage.kind === "connecting"
              ? tr("join_connecting", { host }, `Connecting to ${host}…`)
              : tr("join_waiting", { host }, `Waiting for ${host} to let you in`)
          }
          body={stage.kind === "asking" ? tr("join_waiting_body", { host }, `${host} sees your name on their screen and taps Allow.`) : undefined}
          onCancel={leave}
        />
      </div>
    );
  }

  if (stage.kind === "refused" || stage.kind === "ended") {
    const again = stage.kind === "refused" || stage.why !== "left";
    return (
      <div
        className={
          inApp
            ? "flex flex-col items-center gap-4 rounded-[var(--radius-card)] bg-neutral-900 px-6 py-10 text-center text-white"
            : "flex min-h-[100dvh] flex-col items-center justify-center gap-4 bg-black px-6 text-center text-white"
        }
        data-testid="join-ended"
        data-why={stage.why}
      >
        <Users size={28} className="text-white/60" />
        <p className="max-w-sm text-base" data-testid="join-ended-text">
          {endText(tr, stage, host)}
        </p>
        <div className="flex flex-wrap justify-center gap-2">
          {again && (
            <button
              type="button"
              onClick={onAgain}
              className="rounded-full bg-white px-4 py-2 text-sm font-medium text-black"
              data-testid="join-again"
            >
              {stage.kind === "refused" && stage.why === "not_playing"
                ? tr("join_retry", undefined, "Try again")
                : tr("join_again", undefined, "Ask to join again")}
            </button>
          )}
          <button type="button" onClick={onLeave} className="rounded-full bg-white/15 px-4 py-2 text-sm" data-testid="join-done">
            {tr("join_done", undefined, "Done")}
          </button>
        </div>
      </div>
    );
  }

  const player = stage.player;
  const light = snap.light ?? playerColor(player);
  const delay = snap.rttMs ?? status?.rttMs ?? null;
  const tone = delayTone(delay);
  const picking = status?.state === "picking_user";
  const coarse = coarsePointer();
  const showTouch = live && !reconnecting && !pad.ready && coarse && (!inApp || full);
  const showGuide = !guided && !reconnecting;
  const leftS = reconnecting ? (stage.deadline - now) / 1000 : 0;
  const toggleFull = async () => {
    const next = !full;
    setFull(next);
    await onFullscreen?.(next);
  };

  const box = full || !inApp ? "fixed inset-0 z-[200] bg-black" : "relative aspect-video w-full overflow-hidden rounded-[var(--radius-card)] bg-black shadow-[var(--shadow-2)]";

  return (
    <div className="space-y-2">
      <div
        className={`${box} select-none text-white [-webkit-touch-callout:none]`}
        style={{ touchAction: "none" }}
        data-testid="join-stage-view"
      >
        <div ref={screen} className="pointer-events-none absolute inset-0 z-0 flex items-center justify-center" aria-hidden />

        <div className="absolute inset-x-0 top-0 z-20 flex flex-wrap items-center gap-2 bg-gradient-to-b from-black/70 to-transparent px-3 pb-5 pt-[max(8px,env(safe-area-inset-top))] text-xs">
          <span
            className="flex items-center gap-1.5 rounded-full bg-black/60 px-2.5 py-1 font-medium"
            data-testid="join-player"
          >
            <span className="inline-block h-2.5 w-2.5 rounded-full" style={{ background: `rgb(${light[0]},${light[1]},${light[2]})` }} />
            {inApp && (
              <span className="text-white/80">
                {tr("join_playing_at", { host }, `Playing at ${host}'s`)}
                {" · "}
              </span>
            )}
            {tr("join_you_are", { n: player }, `You are Player ${player}`)}
          </span>
          {delay != null && (
            <span
              className="rounded-full bg-black/60 px-2 py-1 tabular-nums"
              style={{ color: TONE_COLOR[tone] }}
              data-testid="join-delay"
              data-tone={tone}
            >
              {Math.round(delay)} ms
            </span>
          )}
          {picking && (
            <span className="rounded-full bg-white/90 px-2.5 py-1 font-medium text-black" data-testid="join-picking">
              {tr("join_picking", undefined, "On the TV: pick Quick Play")}
            </span>
          )}
          <span className="flex-1" />
          {!snap.sound && (
            <button
              type="button"
              onClick={() => session.startSound()}
              className="flex items-center gap-1 rounded-full bg-white/90 px-2.5 py-1 font-medium text-black"
              data-testid="join-sound"
            >
              <Volume2 size={13} />
              {tr("join_sound", undefined, "Tap for sound")}
            </button>
          )}
          {(inApp || (typeof document !== "undefined" && !!document.documentElement.requestFullscreen)) && (
            <button
              type="button"
              onClick={() => void toggleFull()}
              aria-label={full ? tr("join_exit_fullscreen", undefined, "Leave full screen") : tr("pad_fill", undefined, "Full screen")}
              className="flex items-center gap-1 rounded-full bg-white/15 px-2.5 py-1 hover:bg-white/25"
              data-testid="join-full"
            >
              {full ? <Minimize size={13} /> : <Expand size={13} />}
            </button>
          )}
          <button
            type="button"
            onClick={leave}
            className="flex items-center gap-1 rounded-full bg-[var(--color-danger,#dc2626)] px-3 py-1 font-medium"
            data-testid="join-leave"
          >
            <LogOut size={13} />
            {tr("join_leave", undefined, "Leave")}
          </button>
        </div>

        {snap.problem && (
          <p className="absolute inset-x-4 top-1/2 z-10 -translate-y-1/2 text-center text-sm text-white/85" data-testid="join-problem">
            {snap.problem === "no_hevc"
              ? tr("pad_watch_no_hevc", undefined, "This device can't play this stream's picture: the computer streams HEVC, and this device plays only H.264.")
              : tr("pad_watch_decode", undefined, "The picture could not be played here.")}
          </p>
        )}

        {showTouch && <TouchControls stream={session} layout="overlay" />}

        {!showTouch && !reconnecting && (
          <div className="absolute bottom-3 left-3 z-20 flex items-center gap-2 rounded-full bg-black/70 px-3 py-1.5 text-xs" data-testid="join-pad">
            <Gamepad2 size={14} style={{ color: pad.pressed ? `rgb(${light[0]},${light[1]},${light[2]})` : undefined }} />
            {pad.ready
              ? tr("join_pad_ready", undefined, "Your controller is ready")
              : coarse && inApp && !full
                ? tr("join_full_to_play", undefined, "Tap full screen to play with the on-screen controller.")
                : tr("join_pad_none", undefined, "Connect a controller (USB or Bluetooth) and press any button.")}
          </div>
        )}

        {status?.blockSystem && status.state === "playing" && !showTouch && (
          <p className="absolute bottom-3 right-3 z-20 rounded-full bg-black/60 px-3 py-1 text-[11px] text-white/70" data-testid="join-blocked">
            {tr("join_block_note", { host }, `${host} turned off PS and Create for friends.`)}
          </p>
        )}

        {showGuide && (
          <div
            className="absolute bottom-14 left-1/2 z-30 w-[min(22rem,calc(100%-2rem))] -translate-x-1/2 rounded-2xl border border-white/10 bg-neutral-900/90 p-4 text-sm shadow-2xl backdrop-blur-md"
            data-testid="join-guide"
          >
            <p className="font-semibold">{tr("join_guide_title", undefined, "One more step on the TV")}</p>
            <p className="mt-1 text-white/75">
              {tr("join_guide_body", undefined, "Press PS on your controller, then on the TV pick Quick Play.")}
            </p>
            <p className="mt-1 text-white/60">
              {tr(
                "join_guide_menus",
                undefined,
                "On the PS5's home screen only the host controls the menus; your controller plays inside games.",
              )}
            </p>
            <button
              type="button"
              onClick={() => {
                safeSetItem(GUIDED_KEY, "1");
                setGuided(true);
              }}
              className="mt-3 rounded-full bg-white px-3 py-1 text-xs font-medium text-black"
              data-testid="join-guide-ok"
            >
              {tr("join_guide_ok", undefined, "Got it")}
            </button>
          </div>
        )}

        {reconnecting && (
          <div className="absolute inset-0 z-30 flex flex-col items-center justify-center gap-3 bg-black/70 px-6 text-center" data-testid="join-reconnecting">
            <span className="h-7 w-7 animate-spin rounded-full border-2 border-white/30 border-t-white" aria-hidden />
            <p className="text-base font-medium">{tr("join_reconnecting", undefined, "Reconnecting…")}</p>
            <p className="text-sm text-white/70" data-testid="join-reconnecting-left">
              {tr("join_reconnecting_left", { time: clock(leftS) }, `Your place is kept for ${clock(leftS)}.`)}
            </p>
          </div>
        )}

        {!inApp && live && portrait && coarse && (
          <div className="pointer-events-none absolute inset-x-0 bottom-16 z-20 flex justify-center">
            <span className="flex items-center gap-2 rounded-full bg-black/70 px-3 py-1.5 text-xs text-white/85">
              <RotateCw size={14} />
              {tr("pad_turn", undefined, "Turn the phone sideways")}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
