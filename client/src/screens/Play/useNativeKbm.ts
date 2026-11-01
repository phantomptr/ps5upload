// Native keyboard & mouse on the stream screen (lib/kbmInput.ts): which sink the keys and the
// mouse go to in the running game's mode, and the PS5's virtual devices kept, fed and let go.
// Controller mode is the stream as it always was: this hook then sends nothing and takes no key.

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import {
  KBM_BLOCK_TEXT,
  KbmSender,
  decodeReport,
  devicesFor,
  effectiveMode,
  fromHex,
  nextMode,
  onPadKbm,
  routeFor,
  unionCaps,
  type KbmBlock,
  type KbmCaps,
  type KbmMode,
  type Route,
} from "../../lib/kbmInput";
import { ShortcutListener, shortcutLabel } from "../../lib/kbmShortcuts";
import { EscapeAttemptDetector } from "../../lib/kbmEscape";
import type { Translator } from "../../state/lang";
import type { RemotePlayStream } from "../../lib/remotePlayStream";
import { pushNotification } from "../../state/notifications";
import { rpControls } from "../../state/gameControls";
import { fetchKbmCaps, rememberedMode, useKbmStore } from "../../state/kbm";
import { rpInput } from "../../state/remotePlay";
import { trStatic } from "../../lib/trStatic";
import { captureDelta, captureState, releaseCapture, subscribeCapture } from "../../lib/pointerCapture";

/** How long the big "how to get them back" card shows: the first capture, and an attempt. */
const HINT_FIRST_MS = 3500;
const HINT_ATTEMPT_MS = 4000;
/** An escape attempt shows the card again at most this often. */
const HINT_COOLDOWN_MS = 15000;
/** How long the mode switch's toast shows. */
const TOAST_MS = 2500;
/** A phone that sent nothing for this long has let go (its heartbeat is every 500 ms). */
const GUEST_TIMEOUT_MS = 2000;
const NO_DEVICES: KbmCaps = { mouse: false, keyboard: false };

/** How long a quick Esc holds its key down on the console. */
const ESC_TAP_MS = 80;
/** How often the console is asked again while its keyboard & mouse helper starts. */
const CAPS_STARTING_POLL_MS = 2000;

/** A text box of the app has the keyboard: its keys are typing, not play. */
function typingInApp(): boolean {
  const el = typeof document !== "undefined" ? (document.activeElement as HTMLElement | null) : null;
  if (!el) return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable;
}

/** The release shortcut as this keyboard prints it ("⌃ ⌥", "Ctrl+Alt"). */
export function releaseKeyLabel(): string {
  return shortcutLabel(useKbmStore.getState().shortcuts.release);
}

/** Picks a mode for the running game (or the home screen), with the one-time explanation. */
export function chooseKbmMode(title: string | null, mode: KbmMode): void {
  const s = useKbmStore.getState();
  s.setMode(title, mode);
  if (mode !== "controller" && !s.hinted) {
    s.markHinted();
    pushNotification(
      "info",
      trStatic(
        "rp_kbm_first_hint",
        "Click the picture to play with your keyboard and mouse. Press {key} to get them back. If the game ignores the mouse, switch back to Controller.",
      ).replace("{key}", releaseKeyLabel()),
    );
  }
}

/** A mode's name, as Settings › Controls and the stream bar show it. */
export function kbmModeLabel(tr: Translator, mode: KbmMode): string {
  if (mode === "native") return tr("rp_kbm_native", undefined, "Keyboard & mouse");
  if (mode === "mixed") return tr("rp_kbm_mixed", undefined, "Mixed");
  return tr("rp_kbm_controller", undefined, "Controller");
}

/** Why this console takes no keyboard & mouse mode, in words. */
export function kbmBlockText(tr: Translator, block: KbmBlock): string {
  const t = KBM_BLOCK_TEXT[block];
  return tr(t.key, undefined, t.en);
}

interface Opts {
  stream: RemotePlayStream | null;
  /** This page shows the session and it plays (spectators and other windows never route). */
  owner: boolean;
  host: string | null;
  title: string | null;
  stage: React.RefObject<HTMLElement | null>;
  full: boolean;
  settingsOpen: boolean;
  /** Clicking the picture captures the pointer in Controller mode too (mouse aim). */
  capturePointer?: boolean;
  /** The touch screen is the PS5 mouse's touchpad (TouchpadOverlay): the devices are wanted. */
  touchpad?: boolean;
  /** The keyboard and mouse were given back (the shortcut, a lost capture): the page lets go of
   *  its pad keys and shows the stream bar. */
  onReleased?: () => void;
}

/** The big card over the picture: the first capture on this device, or someone trying to get out. */
export type KbmHint = { reason: "first" | "attempt"; n: number };

export function useNativeKbm({
  stream,
  owner,
  host,
  title,
  stage,
  full,
  settingsOpen,
  capturePointer = true,
  touchpad = false,
  onReleased,
}: Opts) {
  const modes = useKbmStore((s) => s.modes);
  const caps = useKbmStore((s) => s.caps);
  const err = useKbmStore((s) => s.status?.err ?? null);
  const setCaps = useKbmStore((s) => s.setCaps);
  const remembered = rememberedMode(modes, title);
  const hostCaps = caps && caps.host === host ? caps : null;
  const mode = effectiveMode(remembered, hostCaps);
  // The picture has the mouse: the browser's lock or the desktop app's grab (lib/pointerCapture.ts).
  const cap = useSyncExternalStore(subscribeCapture, captureState, captureState);
  const locked = cap.kind !== "none" && !!cap.el && cap.el === stage.current;
  // The PS5 has the keyboard and mouse: the picture was clicked, nothing released them since.
  const engaged = !!cap.engaged && cap.engaged === stage.current;
  // …with the mouse too (captured, or a click where nothing can capture): the hints show.
  const pointerHeld = engaged && (locked || cap.failed);
  const onReleasedRef = useRef(onReleased);
  useEffect(() => {
    onReleasedRef.current = onReleased;
  });

  // What the console can make, asked once per console the stream shows.
  useEffect(() => {
    if (!owner || !host) return;
    let live = true;
    void fetchKbmCaps(host).then((c) => live && setCaps(c));
    return () => {
      live = false;
    };
  }, [owner, host, setCaps]);

  // Asked again when the engine's answer changes its error (the console's helper starting, up, or
  // unable to start), and every few seconds while the console says it is starting: the bar button
  // and Settings › Controls follow it.
  const lastErr = useRef(err);
  const starting = hostCaps?.companion === "starting";
  useEffect(() => {
    if (lastErr.current === err) return;
    lastErr.current = err;
    if (!owner || !host) return;
    let live = true;
    void fetchKbmCaps(host).then((c) => live && setCaps(c));
    return () => {
      live = false;
    };
  }, [err, owner, host, setCaps]);
  useEffect(() => {
    if (!owner || !host || !starting) return;
    // A reply that lands after the console changed is the old console's: dropped.
    let live = true;
    const id = window.setInterval(
      () => void fetchKbmCaps(host).then((c) => live && setCaps(c)),
      CAPS_STARTING_POLL_MS,
    );
    return () => {
      live = false;
      window.clearInterval(id);
    };
  }, [owner, host, starting, setCaps]);

  // One sender per stream: its socket is the stream's.
  const sender = useMemo(
    () => (stream ? new KbmSender({ send: (m) => stream.sendKbm(m) }) : null),
    [stream],
  );
  useEffect(() => () => sender?.dispose(), [sender]);

  /** `over`: the pointer is over the picture (counts only where nothing can capture it). */
  const route = useCallback(
    (over = false): Route =>
      routeFor(mode, {
        owner,
        appFirst: settingsOpen || rpInput.paused || typingInApp(),
        pointerLocked: locked,
        captured: engaged,
        hoverFallback: over && cap.failed,
      }),
    [mode, owner, settingsOpen, locked, engaged, cap.failed],
  );

  // Phones playing along with the phone link's Touchpad & keyboard (the engine forwards them):
  // what each wants and holds, merged into this page's reports. They play whatever this page's
  // mode is, and let go when they leave or go quiet.
  const [guestWant, setGuestWant] = useState<KbmCaps>(NO_DEVICES);
  const guests = useRef(new Map<number, { want: KbmCaps; at: number }>());
  useEffect(() => {
    if (!sender) return;
    const map = guests.current;
    const sync = () => {
      const w = unionCaps(NO_DEVICES, ...[...map.values()].map((g) => g.want));
      setGuestWant((cur) => (cur.mouse === w.mouse && cur.keyboard === w.keyboard ? cur : w));
    };
    const drop = (id: number) => {
      map.delete(id);
      sender.guest(id, null);
      sync();
    };
    const off = onPadKbm((m) => {
      if (m.left || m.on === false) return drop(m.id);
      const g = map.get(m.id) ?? { want: NO_DEVICES, at: 0 };
      g.at = performance.now();
      if (m.on === true) g.want = { mouse: !!m.mouse, keyboard: !!m.keyboard };
      map.set(m.id, g);
      if (m.r !== undefined) {
        const bytes = fromHex(m.r);
        const r = bytes && decodeReport(bytes);
        if (r) {
          sender.guestMotion(r.dx, r.dy, r.wheel, r.hwheel);
          sender.guest(m.id, { buttons: r.buttons, mods: r.mods, keys: r.keys });
        }
      }
      sync();
    });
    const id = window.setInterval(() => {
      const now = performance.now();
      for (const [gid, g] of map) if (now - g.at > GUEST_TIMEOUT_MS) drop(gid);
    }, 500);
    return () => {
      off();
      window.clearInterval(id);
      for (const gid of [...map.keys()]) sender.guest(gid, null);
      map.clear();
      setGuestWant(NO_DEVICES);
    };
  }, [sender]);

  // A mode or game change: everything held in the old sinks is let go first (the pad neutral,
  // the PS5's keys and buttons up). The capture stays: the mode is how the keyboard and mouse
  // are used, the capture whether the PS5 has them (Controller without pointer capture has no
  // use for it).
  const modeKey = `${title ?? ""}|${mode}|${owner}`;
  const lastKey = useRef(modeKey);
  useEffect(() => {
    if (!sender) return;
    if (lastKey.current !== modeKey) {
      lastKey.current = modeKey;
      rpInput.held.clear();
      rpControls.mouse.left = rpControls.mouse.right = false;
      rpControls.mouse.vx = rpControls.mouse.vy = 0;
      stream?.sendPad();
      sender.releaseAll();
      if (mode === "controller" && !capturePointer) releaseCapture();
    }
  }, [sender, stream, modeKey, mode, capturePointer]);

  // The devices asked for: the mode's, the touchpad's, and the phones'.
  const touchWant = touchpad && hostCaps?.mouse ? { mouse: true, keyboard: !!hostCaps.keyboard } : NO_DEVICES;
  const wantMouse = owner && (devicesFor(mode).mouse || touchWant.mouse || guestWant.mouse);
  const wantKeyboard = owner && (devicesFor(mode).keyboard || touchWant.keyboard || guestWant.keyboard);
  useEffect(() => {
    sender?.setWant({ mouse: wantMouse, keyboard: wantKeyboard });
  }, [sender, wantMouse, wantKeyboard]);

  // The shortcuts (release, switch mode) and the signs of someone trying to get out.
  const listener = useMemo(() => new ShortcutListener(() => useKbmStore.getState().shortcuts), []);
  const escape = useMemo(() => new EscapeAttemptDetector(), []);
  const [hint, setHint] = useState<KbmHint | null>(null);
  const lastAttempt = useRef(-Infinity);
  const showHint = useCallback((reason: KbmHint["reason"]) => {
    if (reason === "attempt") {
      const now = performance.now();
      if (now - lastAttempt.current < HINT_COOLDOWN_MS) return;
      lastAttempt.current = now;
    }
    setHint((h) => ({ reason, n: (h?.n ?? 0) + 1 }));
  }, []);
  const dismissHint = useCallback(() => setHint(null), []);
  useEffect(() => {
    if (!hint) return;
    const id = window.setTimeout(() => setHint(null), hint.reason === "first" ? HINT_FIRST_MS : HINT_ATTEMPT_MS);
    return () => window.clearTimeout(id);
  }, [hint]);

  /** Gives the keyboard and mouse back: everything held let go first, then the capture. */
  const release = useCallback(() => {
    listener.reset();
    sender?.releaseAll();
    releaseCapture();
    setHint(null);
    onReleasedRef.current?.();
  }, [listener, sender]);

  // Where the last press on the picture was: where the virtual pointer starts on a capture.
  const pressAt = useRef<{ x: number; y: number } | null>(null);
  useEffect(() => {
    const down = (e: MouseEvent) => {
      const r = stage.current?.getBoundingClientRect();
      if (r && !isCapturedNow()) pressAt.current = { x: e.clientX - r.left, y: e.clientY - r.top };
    };
    const isCapturedNow = () => captureState().kind !== "none";
    document.addEventListener("mousedown", down, true);
    return () => document.removeEventListener("mousedown", down, true);
  }, [stage]);

  // Captured, and released (by anything: the shortcut, the browser's own Esc, blur, Settings).
  const wasEngaged = useRef(engaged);
  useEffect(() => {
    const was = wasEngaged.current;
    wasEngaged.current = engaged;
    if (engaged && !was) {
      listener.reset();
      const r = stage.current?.getBoundingClientRect();
      const at = pressAt.current ?? { x: (r?.width ?? 0) / 2, y: (r?.height ?? 0) / 2 };
      escape.start(at.x, at.y, r?.width ?? 0, r?.height ?? 0);
    }
    if (!engaged && was) {
      // Nothing stays held on the PS5.
      listener.reset();
      sender?.releaseAll();
      setHint(null);
      onReleasedRef.current?.();
    }
  }, [engaged, owner, sender, listener, escape, stage, showHint]);

  // The first capture on this device: how to get the keyboard and mouse back.
  useEffect(() => {
    if (!pointerHeld || !owner) return;
    const s = useKbmStore.getState();
    if (s.captureHinted) return;
    s.markCaptureHinted();
    showHint("first");
  }, [pointerHeld, owner, showHint]);

  // Captured with the pointer hidden: pushes against the edges, and clicks where the bar is.
  useEffect(() => {
    if (!locked || !owner) return;
    const move = (e: MouseEvent) => escape.move(e.movementX || 0, e.movementY || 0) && showHint("attempt");
    const down = () => escape.click() && showHint("attempt");
    document.addEventListener("mousemove", move);
    document.addEventListener("mousedown", down);
    return () => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mousedown", down);
    };
  }, [locked, owner, escape, showHint]);

  // App first: Settings open, or typing in one of the app's text boxes.
  useEffect(() => {
    if (settingsOpen) sender?.releaseAll();
  }, [settingsOpen, sender]);
  useEffect(() => {
    if (!sender) return;
    const onFocus = () => typingInApp() && sender.releaseAll();
    const onBlur = () => {
      listener.reset();
      sender.releaseAll();
    };
    const onVis = () => document.visibilityState === "hidden" && sender.releaseAll();
    document.addEventListener("focusin", onFocus);
    window.addEventListener("blur", onBlur);
    document.addEventListener("visibilitychange", onVis);
    return () => {
      document.removeEventListener("focusin", onFocus);
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [sender, listener]);

  // The mouse: only while it is locked to the picture, and only in a mode that sends it natively.
  const mouseNative = owner && mode !== "controller";
  useEffect(() => {
    if (!sender || !mouseNative) return;
    const over = (e: Event) => !!stage.current && e.target instanceof Node && stage.current.contains(e.target);
    const native = (e: Event) => route(over(e)).mouse === "native";
    const move = (e: MouseEvent) => {
      // Read on every move: the screen position is the delta where a grab leaves the cursor free.
      const [dx, dy] = captureDelta(e);
      if (native(e)) sender.move(dx, dy);
    };
    const down = (e: MouseEvent) => {
      if (!native(e)) return;
      e.preventDefault();
      sender.button(e.button, true);
    };
    // A release always counts: a button pressed while locked is never left down.
    const up = (e: MouseEvent) => sender.button(e.button, false);
    const wheel = (e: WheelEvent) => {
      if (!native(e)) return;
      e.preventDefault();
      sender.scroll(e.deltaY, e.deltaX, e.deltaMode);
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mousedown", down);
    document.addEventListener("mouseup", up);
    document.addEventListener("wheel", wheel, { passive: false });
    return () => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mousedown", down);
      document.removeEventListener("mouseup", up);
      document.removeEventListener("wheel", wheel);
    };
  }, [sender, mouseNative, route, stage]);

  // In full screen, Keyboard Lock (where there is one) gives the game keys the system would keep.
  const keysNative = owner && mode === "native";
  useEffect(() => {
    const kb = (navigator as Navigator & { keyboard?: { lock?: () => Promise<void>; unlock?: () => void } }).keyboard;
    if (!full || !keysNative || !engaged || !kb?.lock) return;
    void kb.lock().catch(() => {});
    return () => kb.unlock?.();
  }, [full, keysNative, engaged]);

  // The mode switch's toast: the new mode, and the release key while captured.
  const [toast, setToast] = useState<{ mode: KbmMode; captured: boolean; n: number } | null>(null);
  useEffect(() => {
    if (!toast) return;
    const id = window.setTimeout(() => setToast(null), TOAST_MS);
    return () => window.clearTimeout(id);
  }, [toast]);

  /** The next mode the console can take, for the running game (the shortcut, the bar button). */
  const cycle = useCallback(() => {
    if (!owner) return;
    const next = nextMode(mode, hostCaps);
    chooseKbmMode(title, next);
    setToast((t) => ({ mode: next, captured: engaged, n: (t?.n ?? 0) + 1 }));
  }, [owner, title, mode, hostCaps, engaged]);

  /** A key seen while captured: Esc twice or the ⌘ / Windows key shows the release key again. */
  const noteKey = useCallback(
    (e: Pick<KeyboardEvent, "code" | "repeat">, down: boolean) => {
      if (engaged && owner && escape.key(e.code, down, e.repeat)) showHint("attempt");
    },
    [engaged, owner, escape, showHint],
  );

  /** The stage's key handler asks first: true when the key was taken (a shortcut, or a key for
   *  the PS5's keyboard). */
  const onKey = useCallback(
    (e: React.KeyboardEvent | KeyboardEvent, down: boolean): boolean => {
      if (!owner) return false;
      const act = listener.feed(e, down);
      if (act === "cycle" || act === "consume") {
        e.preventDefault();
        e.stopPropagation();
        if (act === "cycle") cycle();
        return true;
      }
      if (act === "release" && (engaged || down)) {
        e.preventDefault();
        e.stopPropagation();
        // The chord's modifiers went to the PS5 as they happened: release lets go of them.
        if (engaged) release();
        return true;
      }
      noteKey(e, down);
      if (route().keys !== "native" || !sender) return false;
      // The game's key, not the app's: no browser default, no app shortcut (Ctrl+K, ?) behind it.
      e.preventDefault();
      e.stopPropagation();
      sender.key(e.code, down);
      return true;
    },
    [owner, listener, cycle, engaged, release, noteKey, route, sender],
  );

  /** A quick Esc in full screen: in Native mode it is the PS5 keyboard's Esc. False otherwise
   *  (the game's Circle, as before). */
  const escTap = useCallback((): boolean => {
    if (!sender || route().keys !== "native") return false;
    sender.key("Escape", true);
    window.setTimeout(() => sender.key("Escape", false), ESC_TAP_MS);
    return true;
  }, [sender, route]);

  /** Lets go of everything native (Esc held to leave full screen, stop). */
  const releaseAll = useCallback(() => sender?.releaseAll(), [sender]);

  /** A native mouse mode, and the picture does not have the mouse: say how to give it. */
  const captureHint = !!mouseNative && !locked && !engaged;
  /** Released in Keyboard & mouse or Mixed: keys are this computer's (app shortcuts, the bar). */
  const keysToApp = owner && mode !== "controller" && !engaged;

  /** The touchpad's and the phone keyboard's input: straight to the PS5, no capture (a touch
   *  screen has no pointer to capture). */
  const touch = useMemo(
    () =>
      sender && owner
        ? {
            move: (dx: number, dy: number) => sender.move(dx, dy),
            button: (b: number, d: boolean) => sender.button(b, d),
            scroll: (dy: number, dx: number) => sender.scroll(dy, dx, 0),
            key: (code: string, d: boolean) => sender.key(code, d),
            shiftHeld: () => sender.shiftHeld(),
          }
        : null,
    [sender, owner],
  );

  return {
    mode,
    remembered,
    hostCaps,
    mouseNative,
    keysNative,
    onKey,
    escTap,
    releaseAll,
    release,
    cycle,
    captureHint,
    captured: engaged,
    pointerHeld,
    locked,
    noteKey,
    keysToApp,
    hint,
    dismissHint,
    toast,
    touch,
  };
}
