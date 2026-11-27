import { useEffect, useRef, useState } from "react";
import { Expand, EyeOff, Gamepad2, LogOut, Minimize, MoreHorizontal, RotateCw, Users, Volume2, VolumeX } from "lucide-react";

import type { JoinStage } from "../lib/joinClient";
import { delayTone, playerColor, type JoinStatus } from "../lib/joinProtocol";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import {
  HomeScreenTip,
  hasFullscreenApi,
  showsFullButton,
  turnStep,
  useCollapseBars,
  useHomeScreenTip,
  useTurns,
} from "../lib/pageFill";
import { registerBackDismiss } from "../lib/focusTrap";
import { tvNavActive } from "../lib/tvNav";
import { gamepadsOf } from "../lib/nativePad";
import { isWeak8BitDo, padPlatform, type PadKind } from "../lib/padMappings";
import { padKind } from "../lib/padProfiles";
import { EightBitDoHint } from "../screens/Play/PadHint";
import { TouchControls } from "../screens/Play/TouchControls";
import { useTr } from "../state/lang";
import { devicePad, useJoinSession, type JoinSession } from "./session";
import { useAutoHide, usePickHint } from "./autoHide";
import { PadCheck } from "./PadCheck";
import { OpenOnWifi } from "./OpenOnWifi";
import type { NetPath } from "./netPath";
import { clock } from "../lib/clockText";

type Tr = ReturnType<typeof useTr>;

/** Set once the guide card ("press PS, pick Quick Play") was dismissed. */
const GUIDED_KEY = "ps5upload.join.guided";
/** The host's stream's own controller (the engine's `STREAM_SEAT`). */
const STREAM_SEAT = 1;

/** The first-time guide ("press PS, pick a user"): never on the stream's controller, which plays
 *  as the host's user; again after moving off it onto a controller of their own. */
export function showsGuide(o: { player: number; guided: boolean; movedOff: boolean; reconnecting: boolean }): boolean {
  return o.player !== STREAM_SEAT && (!o.guided || o.movedOff) && !o.reconnecting;
}

/** 42 → "0:42". */
export { clock };

/** While the TV may ask who is using their controller: the PS5 offers the host's own user first,
 *  and picking it (even with a plain ✕ on the first question) ends the host's game. What to do
 *  instead, said more firmly once it happened; null when nothing is being picked. */
export function pickWarning(
  tr: Tr,
  s: Pick<JoinStatus, "state" | "moveFirst" | "hostUser" | "tookHost"> | null,
  host: string,
): string | null {
  if (!s || !(s.state === "picking_user" || s.moveFirst)) return null;
  if (s.tookHost)
    return tr(
      "join_took_host",
      { host },
      `${host}'s game stopped because their PS5 user was picked on your controller. This time, move to Quick Play first, then press ✕.`,
    );
  return s.hostUser
    ? tr(
        "join_not_host_user",
        { user: s.hostUser, host },
        `Move to Quick Play before pressing ✕. Don't pick ${s.hostUser}: that's ${host}'s user, and it ends their game.`,
      )
    : tr(
        "join_not_host_user_unnamed",
        { host },
        `Move to Quick Play before pressing ✕. Don't pick ${host}'s PS5 user: it ends their game.`,
      );
}

/** Which way the connection goes, beside the delay: the home network, Tailscale, the internet. */
export function pathLabel(tr: Tr, path: NetPath): string {
  switch (path) {
    case "lan":
      return tr("join_path_lan", undefined, "Home network");
    case "tailscale":
      return tr("join_path_tailscale", undefined, "Tailscale");
    default:
      return tr("join_path_internet", undefined, "Internet");
  }
}

/** Off the host's stream's controller (they needed it back) with no other controller free. */
function seatTakenText(tr: Tr, host: string): string {
  return tr(
    "join_seat_taken",
    { host },
    `${host} needed their controller back and no other controller was free on the PS5, so you're out of the game. Ask ${host} to let you in again.`,
  );
}

/** The message for a guest who is not (or no longer) in the game. `internet`: an internet invite
 *  (WebRTC), where "the same network" is not the advice. */
export function endText(tr: Tr, stage: JoinStage, host: string, internet = false): string {
  if (stage.kind === "refused") {
    switch (stage.why) {
      case "key":
        return tr("join_refused_key", { host }, `This invite has expired. Ask ${host} for a new link.`);
      case "expired":
        return tr("join_refused_expired", { host }, `Your place in the game was lost. Ask ${host} to let you in again.`);
      case "full":
        return tr("join_refused_full", undefined, "The game is full: four players already.");
      case "denied":
        return tr("join_refused_denied", { host }, `${host} didn't let you in this time.`);
      case "no_answer":
        return tr("join_refused_no_answer", { host }, `${host} didn't answer in time. Tell ${host}, then try again.`);
      case "closed":
        return tr("join_refused_closed", { host }, `${host} isn't hosting Play Together right now.`);
      case "old_helper":
        return tr("join_refused_old_helper", { host }, `${host}'s PS5 needs an updated helper for Play Together.`);
      case "not_playing":
        return tr("join_refused_not_playing", { host }, `${host} isn't playing right now. Ask ${host} to start Play first.`);
      case "unreachable":
        return tr("join_refused_unreachable", { host }, `${host}'s computer couldn't reach their PS5. Try again in a moment.`);
      case "slow_down":
        return tr("join_refused_slow_down", undefined, "Too many wrong codes. Wait a minute, then try again.");
      case "address":
        return tr(
          "join_refused_address",
          { host },
          `This link's address is outside your home network, so the app will not use it. Ask ${host} for a link with their computer's IP address.`,
        );
      case "seat_taken":
        return seatTakenText(tr, host);
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
      case "replaced":
        return tr("join_ended_replaced", undefined, "This game was opened in another tab or on another device, so it stopped here.");
      case "pad_lost":
        return tr("join_ended_pad_lost", { host }, `The PS5 lost your controller, so you're out of the game. Ask ${host} to let you in again.`);
      case "gave_up":
        return tr("join_ended_gave_up", { host }, `The connection to ${host} didn't come back in time.`);
      case "lost":
        if (internet)
          return tr("join_net_lost", { host }, `The connection to ${host} dropped. Check your internet connection, then ask again.`);
        return tr("join_ended_lost", { host }, `Couldn't reach ${host}. Check that you're on the same network.`);
      case "left":
        return tr("join_ended_left", undefined, "You left the game.");
      case "seat_taken":
        return seatTakenText(tr, host);
      // An internet invite's connection (join/rtcTransport).
      case "mismatch":
        return tr("join_net_mismatch", undefined, "This link doesn't match the host; ask them for a new one.");
      case "no_direct":
        return tr(
          "join_net_no_direct",
          undefined,
          "A direct connection isn't possible from this network (both sides' routers are strict). Try Wi-Fi instead of mobile data, or ask the host to turn on UPnP.",
        );
      case "no_room":
        return tr("join_net_no_room", { host }, `This invite isn't open any more. Ask ${host} for a new link.`);
      case "net_full":
        return tr("join_net_full", { host }, `${host}'s game has no room for another friend right now. Try again in a moment.`);
      case "no_hub":
        return tr("join_net_no_hub", undefined, "Couldn't reach the PS5Upload invite service. Check your internet connection, then try again.");
      case "offer":
        return tr("join_net_offer", { host }, `${host}'s computer couldn't connect to this browser. Try another browser.`);
      case "no_webrtc":
        return tr("join_net_no_webrtc", undefined, "This browser can't make direct connections. Try a recent Chrome, Safari or Firefox.");
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

interface PadReady {
  ready: boolean;
  pressed: boolean;
  /** The first controller: its id, how it plays, and whether it is an 8BitDo in a weak mode. */
  id: string | null;
  kind: PadKind | null;
  weak8BitDo: boolean;
}

/** A controller connected to this device, and whether a button is held (the test press). */
function usePadReady(): PadReady {
  const [state, setState] = useState<PadReady>({ ready: false, pressed: false, id: null, kind: null, weak8BitDo: false });
  useEffect(() => {
    const read = () => {
      const first = gamepadsOf(typeof navigator === "undefined" ? undefined : navigator).find((g): g is Gamepad => !!g?.connected);
      const ready = !!first;
      const p = ready ? devicePad() : null;
      const pressed = !!p && (p.buttons !== 0 || p.l2 > 30 || p.r2 > 30);
      const id = first?.id ?? null;
      const kind = first ? padKind(first) : null;
      const weak8BitDo = !!first && isWeak8BitDo(first);
      setState((s) =>
        s.ready === ready && s.pressed === pressed && s.id === id && s.kind === kind && s.weak8BitDo === weak8BitDo
          ? s
          : { ready, pressed, id, kind, weak8BitDo },
      );
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

/** A press on the on-screen controller (its buttons, sticks, touchpad, menu), not the picture. */
function onController(t: EventTarget | null): boolean {
  return !!(t as Element | null)?.closest?.('[data-testid="rp-touch"]');
}

/** How long a finger rests on the picture to show or hide the buttons (touch). */
export const HOLD_MS = 450;
/** A finger that moves further than this (px) is not holding. */
const HOLD_SLOP = 14;

/** A touch screen. Never on a TV (#926): it has a remote, and a TV's WebView may still say coarse. */
const coarsePointer = () =>
  typeof window !== "undefined" && !tvNavActive() && !!window.matchMedia?.("(pointer: coarse)").matches;

/** Where the focus is, for Back and Esc (#926): on the game (the picture's own focus spot, or
 *  nothing at all, where keys play), on the view's own buttons, or elsewhere on the page. */
export type FocusWhere = "game" | "controls" | "page";

type FocusNode = { closest?(sel: string): unknown } | null;

export function focusWhere(
  active: FocusNode,
  body: unknown,
  view: { contains(n: never): boolean } | null,
): FocusWhere {
  if (!active || active === body || active.closest?.("[data-join-game]")) return "game";
  return view?.contains(active as never) ? "controls" : "page";
}

/** What one press of Back does while in the game (#926), the first that applies: close the
 *  controller check, then the first-time guide; from the game, show the buttons and put the focus
 *  on them; from the buttons, leave full screen. Null: nothing here, Back goes on as usual (the
 *  app's previous screen, the browser's previous page). */
export type BackStep = "close-check" | "close-guide" | "show-controls" | "leave-full" | null;

export function backStep(o: { checking: boolean; guide: boolean; where: FocusWhere; full: boolean }): BackStep {
  if (o.checking) return "close-check";
  if (o.guide) return "close-guide";
  if (o.where === "game") return "show-controls";
  if (o.full) return "leave-full";
  return null;
}

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
      {/* The one thing to press here: a TV's remote is on it at once (#926). */}
      <button
        type="button"
        onClick={onCancel}
        autoFocus
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
  lanUrl = null,
  hubDown = false,
}: {
  session: JoinSession;
  inApp?: boolean;
  /** An internet invite's page on the host's Wi-Fi ("Open on this Wi-Fi", join/lanHint), offered
   *  when the invite service can't be reached; null: none. */
  lanUrl?: string | null;
  /** The invite service did not answer this page's check (OpenOnWifi useHubDown). */
  hubDown?: boolean;
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
  // The whole view (its buttons), and the game's own focus spot: there the keys play (#926).
  const view = useRef<HTMLDivElement>(null);
  const game = useRef<HTMLDivElement>(null);
  const [full, setFull] = useState(false);
  const homeTip = useHomeScreenTip();
  const [guided, setGuided] = useState(() => safeGetItem(GUIDED_KEY) === "1");
  const [portrait, setPortrait] = useState(() => typeof window !== "undefined" && window.innerHeight > window.innerWidth);
  const pad = usePadReady();
  const [checking, setChecking] = useState(false);
  const reconnecting = stage.kind === "reconnecting";
  const now = useNow(reconnecting);
  const live = stage.kind === "joined" || reconnecting;
  const picking = status?.state === "picking_user";
  // Player 1 is the host's stream's own controller (#687): signed in as the host's PS5 user, so
  // there is no user to pick. Moved off it (the host needed it back, #720), the guide comes back.
  const livePlayer = stage.kind === "joined" || stage.kind === "reconnecting" ? stage.player : 0;
  const [movedOff, setMovedOff] = useState(false);
  const lastPlayer = useRef(livePlayer);
  useEffect(() => {
    if (lastPlayer.current === STREAM_SEAT && livePlayer > STREAM_SEAT) setMovedOff(true);
    if (livePlayer) lastPlayer.current = livePlayer;
  }, [livePlayer]);
  // The buttons over the picture hide after a quiet moment where the picture is the screen
  // (full screen; the /join page always), like a streaming app's. A touch on the picture, a
  // mouse move, Esc or the handle brings them back. Never while the guest must see them
  // (reconnecting, picking a user on the TV) or a mouse or keyboard is on them.
  const [barHover, setBarHover] = useState(false);
  const [barFocus, setBarFocus] = useState(false);
  const autoHide = live && (full || !inApp);
  // Touch: a tap is the game's, so the buttons come and go only on a long press on the picture
  // (away from the controller), the controller's menu button or Hide; full screen starts
  // without them. A mouse or keyboard keeps the quiet-moment hiding.
  const touch = coarsePointer();
  // Picking a user on the TV holds the buttons up for a mouse. On touch they're a card in the
  // middle of the picture, right over the console's user menu (#649), so they stay away and a
  // small note at the top says what to do.
  // Only for a while (#916): when the console never tells us the pick happened (a pick made
  // some other way than PS then ✕), the hint would otherwise sit over the game for good.
  const pickHint = usePickHint(picking);
  const chrome = useAutoHide(autoHide, reconnecting || (pickHint && !touch) || barHover || barFocus, touch);
  const { poke } = chrome;
  const hold = useRef<{ id: number; x: number; y: number; timer: ReturnType<typeof setTimeout> } | null>(null);
  const endHold = () => {
    if (hold.current) clearTimeout(hold.current.timer);
    hold.current = null;
  };
  useEffect(() => endHold, []);
  // The first time they go on touch, a hint says how to get them back (once per game).
  const [holdHint, setHoldHint] = useState(false);
  const hinted = useRef(false);
  useEffect(() => {
    // After the first-time guide card, which sits over the same spot.
    if (!autoHide || !touch || chrome.shown || !guided || hinted.current) return;
    hinted.current = true;
    setHoldHint(true);
    const t = setTimeout(() => setHoldHint(false), 6000);
    return () => clearTimeout(t);
  }, [autoHide, touch, chrome.shown, guided]);

  useEffect(() => {
    if (!live) return;
    const el = session.element;
    screen.current?.appendChild(el);
    return () => el.remove();
  }, [live, session]);

  // Sound needs a tap (#657): the first touch, click or key anywhere in the game starts it, as
  // the "Tap for sound" button does. Captured, so the on-screen controller's touches count too;
  // these are the events a browser lets start sound (a touch's pointerdown is not one).
  useEffect(() => {
    if (!live || snap.sound) return;
    const unlock = () => session.startSound();
    const evs = ["pointerup", "touchend", "click", "keydown"] as const;
    for (const e of evs) window.addEventListener(e, unlock, true);
    return () => {
      for (const e of evs) window.removeEventListener(e, unlock, true);
    };
  }, [live, snap.sound, session]);

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

  // A phone in the app turned sideways (or already sideways when the game starts): full screen,
  // as the host's player. Only on a turn: leaving full screen sideways stays left.
  const fullNow = useRef(full);
  useEffect(() => {
    fullNow.current = full;
  }, [full]);
  useEffect(() => {
    if (!inApp || !live || !coarsePointer() || typeof window === "undefined" || !window.matchMedia) return;
    const mq = window.matchMedia("(orientation: landscape)");
    const turn = () => {
      if (!mq.matches || fullNow.current) return;
      setFull(true);
      void onFullscreen?.(true);
    };
    turn();
    mq.addEventListener?.("change", turn);
    return () => mq.removeEventListener?.("change", turn);
  }, [inApp, live, onFullscreen]);

  // A phone in a browser turned sideways: the page's full layout, and the real full screen where
  // the browser has one (Android Chrome lets the page ask inside the turn itself); upright again
  // leaves it. An iPhone browser has no full screen to ask for: the bars fold away where Safari
  // lets them (useCollapseBars).
  useTurns(!inApp && live && touch, (sideways) => {
    const step = turnStep(sideways, fullNow.current);
    if (!step) return;
    const next = step === "enter";
    fullNow.current = next;
    setFull(next);
    void onFullscreen?.(next);
  });
  useCollapseBars(!inApp && live && full && touch && !hasFullscreenApi());

  // The app on a TV (#926): the game fills the screen from the start, as a TV app's does. Back
  // shows the buttons, Back again leaves full screen; Full screen is among the buttons.
  useEffect(() => {
    if (!inApp || !live || !tvNavActive() || fullNow.current) return;
    setFull(true);
    void onFullscreen?.(true);
  }, [inApp, live, onFullscreen]);

  // Esc shows the buttons and leaves full screen (the desktop app's window has no Esc of its
  // own; a browser that kept Esc for the page gets it here). A guest's Esc is never the pad's.
  // Without full screen it moves between the game and its buttons (#926), and it closes the
  // controller check first.
  useEffect(() => {
    if (!live) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (checking) return void latestBack.current();
      poke();
      if (full) return setFull(false);
      const at = focusWhere(document.activeElement, document.body, view.current);
      if (at === "game") latestBack.current();
      else if (at === "controls") game.current?.focus({ preventScroll: true });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [live, full, poke, checking]);

  // System Back on a phone in full screen: back to the picture's place, still playing, as the
  // host's player does (StreamPanel). Same state as the page's own entry: the router sees no
  // navigation.
  useEffect(() => {
    if (!full || !coarsePointer()) return;
    window.history.pushState(window.history.state, "");
    const entry = window.location.href;
    let popped = false;
    const onPop = () => {
      popped = true;
      setFull(false);
    };
    window.addEventListener("popstate", onPop);
    return () => {
      window.removeEventListener("popstate", onPop);
      if (!popped && window.location.href === entry) window.history.back();
    };
  }, [full]);

  // The browser left full screen itself (Esc): back to the picture's place. Not on a touch
  // screen, where Android drops it on a system gesture mid-game and the picture keeps filling.
  useEffect(() => {
    if (!full || typeof document === "undefined") return;
    const onChange = () => {
      if (document.fullscreenElement || coarsePointer()) return;
      setFull(false);
    };
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  }, [full]);

  // ── A TV's remote, a keyboard (#926) ──────────────────────────────────────────────────────
  // The game has a focus spot of its own (data-join-game, over the picture): there, and with
  // nothing focused, the arrows and Enter play. Back (the remote's, the app's system Back, the
  // browser's) works through backStep; Esc as before, and between the game and its buttons.
  const tv = tvNavActive();
  const guideOpen = live && showsGuide({ player: livePlayer, guided, movedOff, reconnecting }) && !status?.playsAs;
  const setFullscreen = async (next: boolean) => {
    setFull(next);
    await onFullscreen?.(next);
  };
  const focusGame = () => game.current?.focus({ preventScroll: true });
  const dismissGuide = () => {
    safeSetItem(GUIDED_KEY, "1");
    setGuided(true);
    setMovedOff(false);
  };
  /** The buttons over the picture, the focus on the first (Reconnecting's Leave while that shows). */
  const showControls = () => {
    poke();
    const sel = reconnecting ? '[data-testid="join-reconnecting-leave"]' : '[data-testid="join-bar"] button';
    view.current?.querySelector<HTMLElement>(sel)?.focus({ preventScroll: true });
  };
  const closeCheck = () => {
    // Back on the button it was opened from, else the game.
    const opener = view.current?.querySelector<HTMLElement>('[data-testid="join-check-open"]');
    if (opener) opener.focus({ preventScroll: true });
    else focusGame();
    setChecking(false);
  };
  const whereNow = () => focusWhere(document.activeElement, document.body, view.current);
  const [where, setWhere] = useState<FocusWhere>("game");
  useEffect(() => {
    if (!live) return;
    const read = () => setWhere(whereNow());
    // On focusout the next element has the focus only a moment later.
    const later = () => window.setTimeout(read, 0);
    read();
    document.addEventListener("focusin", read);
    document.addEventListener("focusout", later);
    return () => {
      document.removeEventListener("focusin", read);
      document.removeEventListener("focusout", later);
    };
  }, [live]);
  const runBack = (): boolean => {
    const step = live ? backStep({ checking, guide: guideOpen, where: whereNow(), full }) : null;
    switch (step) {
      case "close-check":
        closeCheck();
        return true;
      case "close-guide":
        dismissGuide();
        focusGame();
        return true;
      case "show-controls":
        showControls();
        return true;
      case "leave-full":
        void setFullscreen(false);
        return true;
      default:
        return false;
    }
  };
  const latestBack = useRef(runBack);
  useEffect(() => {
    latestBack.current = runBack;
  });
  // The app's system Back (lib/systemBack) comes here while a step applies. Not on a phone: there
  // Back leaves full screen at once (below), and the buttons come with a long press.
  const backHere = inApp && !touch && live && backStep({ checking, guide: guideOpen, where, full }) !== null;
  useEffect(() => {
    if (!backHere) return;
    return registerBackDismiss(() => void latestBack.current());
  }, [backHere]);
  // The /join page: the browser's Back (a TV's remote) comes here first, through an extra entry
  // in its history; when no step applies it goes on back, out of the page.
  const guardHistory = !inApp && live && !touch;
  useEffect(() => {
    if (!guardHistory) return;
    const entry = window.location.href;
    window.history.pushState(window.history.state, "");
    let held = true;
    const onPop = () => {
      held = false;
      if (!latestBack.current()) return window.history.back();
      window.history.pushState(window.history.state, "");
      held = true;
    };
    window.addEventListener("popstate", onPop);
    return () => {
      window.removeEventListener("popstate", onPop);
      if (held && window.location.href === entry) window.history.back();
    };
  }, [guardHistory]);
  // In the game, the focus starts on it (so the remote's arrows play), or on the guide's Got it on a
  // TV (its OK would press the cross button in the game). Never away from a control the guest is on.
  const joinedNow = stage.kind === "joined";
  useEffect(() => {
    if (!joinedNow) return;
    const a = document.activeElement;
    if (a && a !== document.body) return;
    const ok = tv && guideOpen ? view.current?.querySelector<HTMLElement>('[data-testid="join-guide-ok"]') : null;
    if (ok) ok.focus({ preventScroll: true });
    else focusGame();
    // When the game starts, not as the guide comes and goes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [joinedNow]);
  // A press on the controller is the game's: off the buttons, which then hide again.
  useEffect(() => {
    if (pad.pressed && live && !checking && whereNow() === "controls") focusGame();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pad.pressed]);

  const leave = () => {
    session.leave();
    onLeave();
  };

  if (stage.kind === "connecting" || stage.kind === "asking") {
    return (
      <div
        className={inApp ? "rounded-[var(--radius-card)] bg-neutral-900 text-white" : "flex min-h-[100dvh] flex-col items-center justify-center gap-4 bg-black text-white"}
        data-join-ui=""
      >
        {stage.kind === "connecting" && hubDown && lanUrl && <OpenOnWifi href={lanUrl} />}
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
    // A link that is not the host's never works: no asking again with it.
    const again = stage.kind === "refused" || (stage.why !== "left" && stage.why !== "mismatch");
    return (
      <div
        className={
          inApp
            ? "flex flex-col items-center gap-4 rounded-[var(--radius-card)] bg-neutral-900 px-6 py-10 text-center text-white"
            : "flex min-h-[100dvh] flex-col items-center justify-center gap-4 bg-black px-6 text-center text-white"
        }
        data-testid="join-ended"
        data-why={stage.why}
        data-join-ui=""
      >
        <Users size={28} className="text-white/60" />
        <p className="max-w-sm text-base" data-testid="join-ended-text">
          {endText(tr, stage, host, session.opts.internet ?? !!session.opts.transport)}
        </p>
        {/* The invite service can't be reached: on the host's Wi-Fi, their own page still works. */}
        {lanUrl && stage.why === "no_hub" && <OpenOnWifi href={lanUrl} />}
        <div className="flex flex-wrap justify-center gap-2">
          {again && (
            <button
              type="button"
              onClick={onAgain}
              autoFocus
              className="rounded-full bg-white px-4 py-2 text-sm font-medium text-black"
              data-testid="join-again"
            >
              {stage.kind === "refused" && stage.why === "not_playing"
                ? tr("join_retry", undefined, "Try again")
                : tr("join_again", undefined, "Ask to join again")}
            </button>
          )}
          <button
            type="button"
            onClick={onLeave}
            autoFocus={!again}
            className="rounded-full bg-white/15 px-4 py-2 text-sm"
            data-testid="join-done"
          >
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
  const coarse = coarsePointer();
  const showTouch = live && !reconnecting && !pad.ready && coarse && (!inApp || full);
  const showGuide = showsGuide({ player, guided, movedOff, reconnecting });
  const onSeat = player === STREAM_SEAT;
  // Seated in the profile the host chose: nothing to pick on the TV.
  const playsAs = status?.playsAs ?? null;
  // Picking a user on the TV: never the host's own (it ends their game).
  const warn = !onSeat && !playsAs ? pickWarning(tr, status, host) : null;
  const leftS = reconnecting ? (stage.deadline - now) / 1000 : 0;
  const toggleFull = () => setFullscreen(!full);
  // A phone in the app, the game in its small box: the buttons go in a bar under the picture
  // (big, nothing over the game) and turning the phone sideways goes full screen, as the host's
  // player does. Full screen keeps them over the picture, hidden until asked for.
  const below = inApp && !full && coarse && live;

  const info = (
    <>
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
          data-path={snap.path ?? undefined}
        >
          {snap.path && <span className="text-white/80">{pathLabel(tr, snap.path)} · </span>}
          {Math.round(delay)} ms
        </span>
      )}
      {onSeat && (
        <span className="rounded-full bg-black/60 px-2.5 py-1 text-white/85" data-testid="join-seat-stream">
          {tr("join_seat_stream", { host }, `You're playing as ${host}'s PS5 user. No need to pick one.`)}
        </span>
      )}
      {playsAs && !onSeat && (
        <span className="rounded-full bg-black/60 px-2.5 py-1 text-white/85" data-testid="join-plays-as">
          {tr("join_plays_as", { profile: playsAs }, `You're playing as ${playsAs}. No need to pick one.`)}
        </span>
      )}
      {picking && !onSeat && status?.seatNote && (
        <span className="rounded-full bg-black/60 px-2.5 py-1 text-white/85" data-testid="join-seat-note">
          {tr("join_seat_tv_asks", { host }, `${host} chose a profile for you, but it couldn't be used. The TV will ask who you are.`)}
        </span>
      )}
      {picking && !onSeat && (
        <span className="rounded-full bg-white/90 px-2.5 py-1 font-medium text-black" data-testid="join-picking">
          {tr("join_picking", undefined, "Press PS, then pick Quick Play on the TV")}
        </span>
      )}
      {warn && (
        <span className="rounded-full bg-amber-300 px-2.5 py-1 font-medium text-black" data-testid="join-not-host-user">
          {warn}
        </span>
      )}
    </>
  );
  const actions = (
    <>
      {autoHide && touch && chrome.shown && !reconnecting && (
        <button
          type="button"
          onClick={chrome.hide}
          className="flex items-center gap-1 rounded-full bg-white/15 px-2.5 py-1 font-medium"
          data-testid="join-hide-controls"
        >
          <EyeOff size={13} />
          {tr("join_hide_controls", undefined, "Hide")}
        </button>
      )}
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
      {snap.sound && !snap.noSound && (
        <button
          type="button"
          onClick={() => session.setMuted(!snap.muted)}
          aria-pressed={snap.muted}
          className="flex items-center gap-1 rounded-full bg-white/15 px-2.5 py-1 hover:bg-white/25"
          data-testid="join-mute"
        >
          {snap.muted ? <VolumeX size={13} /> : <Volume2 size={13} />}
          {snap.muted ? tr("rp_unmute", undefined, "Sound on") : tr("rp_mute", undefined, "Mute")}
        </button>
      )}
      {snap.noSound && (
        <span className="flex items-center gap-1 rounded-full bg-black/60 px-2.5 py-1 text-white/80" data-testid="join-no-sound">
          <VolumeX size={13} />
          {tr("join_no_sound", undefined, "No sound in this browser")}
        </span>
      )}
      {showsFullButton(inApp, full) && (
        <button
          type="button"
          onClick={() => {
            // No Fullscreen API (every iPhone browser): the page's full layout, and once the tip.
            if (!inApp && !hasFullscreenApi() && !full) homeTip.showIfNeeded();
            void toggleFull();
          }}
          aria-label={full ? tr("join_exit_fullscreen", undefined, "Leave full screen") : tr("pad_fill", undefined, "Full screen")}
          className="flex items-center gap-1 rounded-full bg-white/15 px-2.5 py-1 hover:bg-white/25"
          data-testid="join-full"
        >
          {full ? <Minimize size={13} /> : <Expand size={13} />}
        </button>
      )}
      <button
        type="button"
        onClick={() => setChecking(true)}
        aria-label={tr("join_check_title", undefined, "Check your controller")}
        title={tr("join_check_title", undefined, "Check your controller")}
        className="flex items-center gap-1 rounded-full bg-white/15 px-2.5 py-1 hover:bg-white/25"
        data-testid="join-check-open"
      >
        <Gamepad2 size={13} />
      </button>
      <button
        type="button"
        onClick={leave}
        className="flex items-center gap-1 rounded-full bg-[var(--color-danger,#dc2626)] px-3 py-1 font-medium"
        data-testid="join-leave"
      >
        <LogOut size={13} />
        {tr("join_leave", undefined, "Leave")}
      </button>
    </>
  );
  // Hidden buttons stay in the page and focusable: a keyboard reaching one shows them all.
  const fade = chrome.shown ? "opacity-100" : "pointer-events-none opacity-0";
  const barProps = {
    "data-testid": "join-bar",
    "data-shown": chrome.shown ? "1" : "0",
    onFocus: (e: React.FocusEvent<HTMLDivElement>) => {
      // On a TV any focus is the remote's (#926).
      if (tv || (e.target as Element).matches?.(":focus-visible")) setBarFocus(true);
    },
    onBlur: (e: React.FocusEvent<HTMLDivElement>) => {
      if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setBarFocus(false);
    },
    onPointerEnter: (e: React.PointerEvent) => {
      if (e.pointerType === "mouse") setBarHover(true);
    },
    onPointerLeave: () => setBarHover(false),
  };

  // In the app's Play screen the box is 16:9 and no taller than the window leaves under the
  // page's header (#929: a 1080p window had the picture's bottom below the window), as the host's
  // own picture box (StreamPanel), never under 20rem wide.
  const box =
    full || !inApp
      ? "fixed inset-0 z-[200] bg-black"
      : "relative mx-auto aspect-video w-full max-w-[max(20rem,calc((100dvh_-_22rem)*16/9))] overflow-hidden rounded-[var(--radius-card)] bg-black shadow-[var(--shadow-2)]";

  // Full screen in the app covers the app: the remote's arrows stay inside, as in a dialog.
  const fullInApp = inApp && full;
  return (
    <div className="space-y-2" ref={view} data-join-ui="">
      {homeTip.tip && <HomeScreenTip onClose={homeTip.hide} />}
      <div
        className={`${box} select-none text-white [-webkit-touch-callout:none]`}
        style={{ touchAction: "none" }}
        role={fullInApp ? "dialog" : undefined}
        aria-modal={fullInApp ? true : undefined}
        aria-label={fullInApp ? tr("pad_fill", undefined, "Full screen") : undefined}
        onPointerMove={(e) => {
          if (e.pointerType === "mouse") return poke();
          const h = hold.current;
          if (h && h.id === e.pointerId && Math.hypot(e.clientX - h.x, e.clientY - h.y) > HOLD_SLOP) endHold();
        }}
        onPointerDown={(e) => {
          if (onController(e.target)) return;
          if (e.pointerType === "mouse" || !touch) return poke();
          // A press on a button (the bar's, the guide's, Tap for sound) is that button's.
          if ((e.target as Element).closest?.('[data-testid="join-bar"], button, a, input, [role="button"]')) return;
          endHold();
          const { pointerId: id, clientX: x, clientY: y } = e;
          hold.current = {
            id,
            x,
            y,
            timer: setTimeout(() => {
              hold.current = null;
              navigator.vibrate?.(15);
              chrome.toggle();
            }, HOLD_MS),
          };
        }}
        onPointerUp={endHold}
        onPointerCancel={endHold}
        onPointerLeave={endHold}
        data-testid="join-stage-view"
      >
        <div ref={screen} className="pointer-events-none absolute inset-0 z-0 flex items-center justify-center" aria-hidden />

        {/* The buttons are hidden, and the delay with them: a small reading stays in the corner. */}
        {autoHide && !chrome.shown && !reconnecting && delay != null && (
          <span
            className="pointer-events-none absolute z-10 rounded-full bg-black/40 px-1.5 py-0.5 text-[10px] leading-none tabular-nums"
            style={{
              color: TONE_COLOR[tone],
              top: "max(0.5rem, env(safe-area-inset-top))",
              right: "max(0.5rem, env(safe-area-inset-right))",
            }}
            data-testid="join-delay-corner"
          >
            {Math.round(delay)} ms
          </span>
        )}

        {showTouch ? (
          // Over the on-screen controller: a card in the middle of the picture, which the
          // controller keeps clear (its top row is the shoulders and the touchpad).
          <div
            className={`absolute left-1/2 top-1/2 z-20 flex w-max max-w-[min(24rem,calc(100%-2rem))] -translate-x-1/2 -translate-y-1/2 flex-col items-center gap-2 rounded-2xl bg-black/70 p-3 text-xs backdrop-blur-sm transition-opacity duration-300 ${fade}`}
            {...barProps}
          >
            <div className="flex flex-wrap items-center justify-center gap-2">{info}</div>
            <div className="flex flex-wrap items-center justify-center gap-2">{actions}</div>
          </div>
        ) : below ? null : (
          <div
            className={`absolute inset-x-0 top-0 z-20 flex flex-wrap items-center gap-2 bg-gradient-to-b from-black/70 to-transparent px-3 pb-5 pt-[max(8px,env(safe-area-inset-top))] text-xs transition-opacity duration-300 ${fade}`}
            {...barProps}
          >
            {info}
            <span className="flex-1" />
            {actions}
          </div>
        )}

        {/* The buttons hidden: a faint handle, top middle, brings them back (the controller has
            its own, bottom middle). */}
        {!chrome.shown && !showTouch && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              poke();
            }}
            aria-label={tr("join_show_controls", undefined, "Show controls")}
            className="absolute left-1/2 top-[max(6px,env(safe-area-inset-top))] z-20 flex h-7 w-11 -translate-x-1/2 items-center justify-center rounded-full bg-black/40 text-white/70 opacity-60 hover:opacity-100 focus-visible:opacity-100"
            data-testid="join-show-controls"
          >
            <MoreHorizontal size={16} aria-hidden />
          </button>
        )}

        {holdHint && !chrome.shown && (
          <p
            className="pointer-events-none absolute left-1/2 top-[max(10px,env(safe-area-inset-top))] z-20 -translate-x-1/2 rounded-full bg-black/60 px-3 py-1 text-xs text-white/85"
            data-testid="join-hold-hint"
          >
            {tr("join_hold_hint", undefined, "Press and hold the picture to show the buttons")}
          </p>
        )}

        {snap.problem && (
          <p className="absolute inset-x-4 top-1/2 z-10 -translate-y-1/2 text-center text-sm text-white/85" data-testid="join-problem">
            {snap.problem === "no_hevc"
              ? tr("pad_watch_no_hevc", undefined, "This device can't play this stream's picture: the computer streams HEVC, and this device plays only H.264.")
              : tr("pad_watch_decode", undefined, "The picture could not be played here.")}
          </p>
        )}

        {checking && <PadCheck onClose={closeCheck} />}

        {showTouch && <TouchControls stream={session} layout="overlay" onMenu={() => (chrome.shown ? chrome.hide() : poke())} />}

        {!showTouch && !reconnecting && (
          <div
            className={`absolute bottom-3 left-3 z-20 flex items-center gap-2 rounded-2xl bg-black/70 px-3 py-1.5 text-xs transition-opacity duration-300 ${fade}`}
            data-testid="join-pad"
          >
            <Gamepad2 size={14} style={{ color: pad.pressed ? `rgb(${light[0]},${light[1]},${light[2]})` : undefined }} />
            {pad.ready
              ? (
                <span className="flex flex-col items-start gap-1">
                  <span>{tr("join_pad_ready", undefined, "Your controller is ready")}</span>
                  {pad.weak8BitDo && pad.id && <EightBitDoHint id={pad.id} className="text-amber-200/90" />}
                  {pad.kind === "guess" && padPlatform() && (
                    <span className="flex flex-wrap items-center gap-2 text-white/75" data-testid="join-pad-unknown">
                      {tr("join_check_unknown_short", undefined, "Not on our list: buttons may be mixed up.")}
                      <button
                        type="button"
                        onClick={() => setChecking(true)}
                        className="rounded-full bg-white px-2.5 py-0.5 font-medium text-black"
                        data-testid="join-pad-unknown-check"
                      >
                        {tr("join_check_title", undefined, "Check your controller")}
                      </button>
                    </span>
                  )}
                </span>
              )
              : coarse && inApp && !full
                ? tr("join_full_to_play", undefined, "Tap full screen to play with the on-screen controller.")
                : (
                  <span className="flex flex-col">
                    <span>{tr("join_pad_none", undefined, "Connect a controller (USB or Bluetooth) and press any button.")}</span>
                    <span className="text-white/60" data-testid="join-pad-keys">
                      {tr("join_pad_keys", undefined, "Or use the keyboard: WASD move, arrows D-pad, Enter ✕, Backspace ○, Q □, E △, Tab Options.")}
                    </span>
                  </span>
                )}
            {/* A TV's remote has no pointer: Back is how the buttons come (#926). */}
            {tv && (
              <span className="max-w-[14rem] border-l border-white/20 pl-2 text-white/70" data-testid="join-tv-back">
                {tr("join_tv_back_hint", undefined, "Press Back on the remote for the buttons.")}
              </span>
            )}
          </div>
        )}

        {status?.blockSystem && status.state === "playing" && !showTouch && (
          <p
            className={`absolute bottom-3 right-3 z-20 rounded-full bg-black/60 px-3 py-1 text-[11px] text-white/70 transition-opacity duration-300 ${fade}`}
            data-testid="join-blocked"
          >
            {tr("join_block_note", { host }, `${host} turned off PS and Create for friends.`)}
          </p>
        )}

        {pickHint && !onSeat && touch && !chrome.shown && !reconnecting && (
          <p
            className="pointer-events-none absolute left-1/2 top-2 z-20 -translate-x-1/2 whitespace-nowrap rounded-full bg-white/90 px-2.5 py-1 text-xs font-medium text-black"
            data-testid="join-picking-note"
          >
            {tr("join_picking", undefined, "Press PS, then pick Quick Play on the TV")}
          </p>
        )}
        {warn && pickHint && touch && !chrome.shown && !reconnecting && (
          <p
            className="pointer-events-none absolute left-1/2 top-10 z-20 w-max max-w-[calc(100%-2rem)] -translate-x-1/2 rounded-2xl bg-amber-300 px-2.5 py-1 text-center text-xs font-medium text-black"
            data-testid="join-not-host-user-note"
          >
            {warn}
          </p>
        )}

        {showGuide && !playsAs && (
          <div
            className="absolute bottom-14 left-1/2 z-30 w-[min(22rem,calc(100%-2rem))] -translate-x-1/2 rounded-2xl border border-white/10 bg-neutral-900/90 p-4 text-sm shadow-2xl backdrop-blur-md"
            data-testid="join-guide"
          >
            <p className="font-semibold">{tr("join_guide_title", undefined, "One more step on the TV")}</p>
            <p className="mt-1 text-white/75">
              {tr(
                "join_guide_body",
                undefined,
                'On your controller, press PS once. The TV asks "Who\'s using this controller?". Pick Quick Play or your own user. The host doesn\'t need to do anything.',
              )}
            </p>
            {warn && <p className="mt-1 font-medium text-amber-300">{warn}</p>}
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
                dismissGuide();
                focusGame();
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
            {/* Never stuck waiting: Leave is here too (the buttons above sit under this). */}
            <button
              type="button"
              onClick={leave}
              className="absolute right-3 top-[max(10px,env(safe-area-inset-top))] flex items-center gap-1 rounded-full bg-white/15 px-3 py-1.5 text-sm font-medium hover:bg-white/25"
              data-testid="join-reconnecting-leave"
            >
              <LogOut size={14} />
              {tr("join_leave", undefined, "Leave")}
            </button>
            <span className="h-7 w-7 animate-spin rounded-full border-2 border-white/30 border-t-white" aria-hidden />
            {stage.hostStopped ? (
              <>
                <p className="max-w-sm text-base font-medium" data-testid="join-host-away">
                  {tr(
                    "join_host_away",
                    { time: clock(leftS) },
                    `The host stopped playing — waiting for them to start again (${clock(leftS)})`,
                  )}
                </p>
                {status?.tookHost && (
                  <p className="max-w-sm text-sm text-amber-300" data-testid="join-took-host">
                    {tr(
                      "join_took_host_away",
                      { host },
                      `${host}'s PS5 user was picked on your controller, which ends their game. When it's back and the TV asks who you are, move to Quick Play first, then press ✕.`,
                    )}
                  </p>
                )}
              </>
            ) : (
              <>
                <p className="text-base font-medium">{tr("join_reconnecting", undefined, "Reconnecting…")}</p>
                <p className="text-sm text-white/70" data-testid="join-reconnecting-left">
                  {tr("join_reconnecting_left", { time: clock(leftS) }, `Your place is kept for ${clock(leftS)}.`)}
                </p>
              </>
            )}
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

        {/* The game's own focus spot (#926): with the focus here the arrows and Enter play, as with
            none. Last, so Shift+Tab goes back to the buttons; it takes no touches or clicks. */}
        <div
          ref={game}
          tabIndex={0}
          data-own-arrows=""
          data-join-game=""
          aria-label={tr("join_game_label", { host }, `${host}'s game`)}
          className="pointer-events-none absolute inset-0 outline-none"
          data-testid="join-game"
        />
      </div>
      {below && (
        // Under the picture on a phone: big, easy to hit, nothing over the game.
        <div
          className="flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] bg-neutral-900 px-3 py-2 text-xs text-white"
          data-testid="join-bar"
          data-shown="1"
          data-below="1"
        >
          <div className="flex w-full min-w-0 flex-wrap items-center gap-2">{info}</div>
          <button
            type="button"
            onClick={() => void toggleFull()}
            className="flex h-11 min-w-0 flex-1 items-center justify-center gap-1.5 rounded-full bg-white/15 px-3 text-sm font-medium hover:bg-white/25"
            data-testid="join-fullscreen-below"
          >
            <Expand size={15} />
            {tr("pad_fill", undefined, "Full screen")}
          </button>
          {snap.sound && !snap.noSound ? (
            <button
              type="button"
              onClick={() => session.setMuted(!snap.muted)}
              aria-pressed={snap.muted}
              aria-label={snap.muted ? tr("rp_unmute", undefined, "Sound on") : tr("rp_mute", undefined, "Mute")}
              className="flex h-11 w-11 items-center justify-center rounded-full bg-white/15 hover:bg-white/25"
            >
              {snap.muted ? <VolumeX size={16} /> : <Volume2 size={16} />}
            </button>
          ) : !snap.sound ? (
            <button
              type="button"
              onClick={() => session.startSound()}
              className="flex h-11 items-center justify-center gap-1.5 rounded-full bg-white px-3 text-sm font-medium text-black"
            >
              <Volume2 size={15} />
              {tr("join_sound", undefined, "Tap for sound")}
            </button>
          ) : null}
          <button
            type="button"
            onClick={leave}
            className="flex h-11 items-center justify-center gap-1.5 rounded-full bg-[var(--color-danger,#dc2626)] px-4 text-sm font-medium"
            data-testid="join-leave-below"
          >
            <LogOut size={15} />
            {tr("join_leave", undefined, "Leave")}
          </button>
        </div>
      )}
    </div>
  );
}
