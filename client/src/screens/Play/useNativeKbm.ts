// Native keyboard & mouse on the stream screen (lib/kbmInput.ts): which sink the keys and the
// mouse go to in the running game's mode, and the PS5's virtual devices kept, fed and let go.
// Controller mode is the stream as it always was: this hook then sends nothing and takes no key.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  KbmSender,
  devicesFor,
  effectiveMode,
  isCycleHotkey,
  nextMode,
  routeFor,
  type KbmMode,
  type Route,
} from "../../lib/kbmInput";
import type { RemotePlayStream } from "../../lib/remotePlayStream";
import { pushNotification } from "../../state/notifications";
import { rpControls } from "../../state/gameControls";
import { fetchKbmCaps, rememberedMode, useKbmStore } from "../../state/kbm";
import { rpInput } from "../../state/remotePlay";
import { trStatic } from "../../lib/trStatic";

/** How long a quick Esc holds its key down on the console. */
const ESC_TAP_MS = 80;

/** A text box of the app has the keyboard: its keys are typing, not play. */
function typingInApp(): boolean {
  const el = typeof document !== "undefined" ? (document.activeElement as HTMLElement | null) : null;
  if (!el) return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable;
}

/** Picks a mode for the running game (or the home screen), with the one-time hint. */
export function chooseKbmMode(title: string | null, mode: KbmMode): void {
  const s = useKbmStore.getState();
  s.setMode(title, mode);
  if (mode !== "controller" && !s.hinted) {
    s.markHinted();
    pushNotification("info", trStatic("rp_kbm_hint", "If the game ignores the mouse, switch back to Controller."));
  }
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
}

export function useNativeKbm({ stream, owner, host, title, stage, full, settingsOpen }: Opts) {
  const modes = useKbmStore((s) => s.modes);
  const caps = useKbmStore((s) => s.caps);
  const setCaps = useKbmStore((s) => s.setCaps);
  const remembered = rememberedMode(modes, title);
  const hostCaps = caps && caps.host === host ? caps : null;
  const mode = effectiveMode(remembered, hostCaps);
  const [locked, setLocked] = useState(false);

  // What the console can make, asked once per console the stream shows.
  useEffect(() => {
    if (!owner || !host) return;
    let live = true;
    void fetchKbmCaps(host).then((c) => live && setCaps(c));
    return () => {
      live = false;
    };
  }, [owner, host, setCaps]);

  // One sender per stream: its socket is the stream's.
  const sender = useMemo(
    () => (stream ? new KbmSender({ send: (m) => stream.sendKbm(m) }) : null),
    [stream],
  );
  useEffect(() => () => sender?.dispose(), [sender]);

  const route = useCallback(
    (): Route => routeFor(mode, { owner, appFirst: settingsOpen || rpInput.paused || typingInApp(), pointerLocked: locked }),
    [mode, owner, settingsOpen, locked],
  );

  // A mode or game change: everything held in the old sinks is let go first (the pad neutral,
  // the PS5's keys and buttons up), then the new mode's devices are asked for.
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
    }
    sender.setWant(owner ? devicesFor(mode) : { mouse: false, keyboard: false });
  }, [sender, stream, modeKey, mode, owner]);

  // App first: Settings open, or typing in one of the app's text boxes.
  useEffect(() => {
    if (settingsOpen) sender?.releaseAll();
  }, [settingsOpen, sender]);
  useEffect(() => {
    if (!sender) return;
    const onFocus = () => typingInApp() && sender.releaseAll();
    const onBlur = () => sender.releaseAll();
    const onVis = () => document.visibilityState === "hidden" && sender.releaseAll();
    document.addEventListener("focusin", onFocus);
    window.addEventListener("blur", onBlur);
    document.addEventListener("visibilitychange", onVis);
    return () => {
      document.removeEventListener("focusin", onFocus);
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [sender]);

  // The mouse: only while it is locked to the picture, and only in a mode that sends it natively.
  const mouseNative = owner && mode !== "controller";
  useEffect(() => {
    const isLocked = () => !!stage.current && document.pointerLockElement === stage.current;
    const lockChange = () => {
      const l = isLocked();
      setLocked(l);
      if (!l) sender?.releaseAll();
    };
    setLocked(isLocked());
    document.addEventListener("pointerlockchange", lockChange);
    return () => document.removeEventListener("pointerlockchange", lockChange);
  }, [stage, sender]);
  useEffect(() => {
    if (!sender || !mouseNative) return;
    const native = () => route().mouse === "native";
    const move = (e: MouseEvent) => native() && sender.move(e.movementX, e.movementY);
    const down = (e: MouseEvent) => {
      if (!native()) return;
      e.preventDefault();
      sender.button(e.button, true);
    };
    // A release always counts: a button pressed while locked is never left down.
    const up = (e: MouseEvent) => sender.button(e.button, false);
    const wheel = (e: WheelEvent) => {
      if (!native()) return;
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
  }, [sender, mouseNative, route]);

  // In full screen, Keyboard Lock (where there is one) gives the game keys the system would keep.
  const keysNative = owner && mode === "native";
  useEffect(() => {
    const kb = (navigator as Navigator & { keyboard?: { lock?: () => Promise<void>; unlock?: () => void } }).keyboard;
    if (!full || !keysNative || !kb?.lock) return;
    void kb.lock().catch(() => {});
    return () => kb.unlock?.();
  }, [full, keysNative]);

  /** The stage's key handler asks first: true when the key was taken (the hotkey, or a key for
   *  the PS5's keyboard). */
  const onKey = useCallback(
    (e: React.KeyboardEvent | KeyboardEvent, down: boolean): boolean => {
      if (!owner) return false;
      if (isCycleHotkey(e)) {
        e.preventDefault();
        e.stopPropagation();
        if (down && !e.repeat) chooseKbmMode(title, nextMode(mode, hostCaps));
        return true;
      }
      if (route().keys !== "native" || !sender) return false;
      // The game's key, not the app's: no browser default, no app shortcut (Ctrl+K, ?) behind it.
      e.preventDefault();
      e.stopPropagation();
      sender.key(e.code, down);
      return true;
    },
    [owner, title, mode, hostCaps, route, sender],
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

  return { mode, remembered, mouseNative, keysNative, onKey, escTap, releaseAll };
}
