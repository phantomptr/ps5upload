import { useCallback, useEffect, useRef, useState } from "react";

/** How long the buttons over a friend's game stay after the last touch, click or mouse move. */
export const HIDE_AFTER_MS = 3000;

/**
 * The buttons over the picture, hidden after a quiet moment like a streaming app's (Moonlight,
 * Steam Link): while `enabled` (full screen, a phone) and not `pinned` (a pointer or keyboard
 * focus on them, a status the guest must see), they show for HIDE_AFTER_MS after each poke and
 * then go. Not enabled or pinned, they always show.
 */
export class AutoHide {
  private enabled = false;
  private pinned = false;
  /** Touch: the buttons come and go only when asked (a long press, the Hide button), never by
   *  themselves; a tap is the game's. */
  private sticky = false;
  private shown = true;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly onChange: (shown: boolean) => void,
    private readonly delay = HIDE_AFTER_MS,
  ) {}

  /** Whether the buttons are on screen now. */
  get visible(): boolean {
    return !this.enabled || this.pinned || this.shown;
  }

  set(enabled: boolean, pinned: boolean, sticky = false): void {
    const was = this.enabled && !this.pinned;
    const wasEnabled = this.enabled;
    const wasSticky = this.sticky;
    this.enabled = enabled;
    this.pinned = pinned;
    this.sticky = sticky;
    if (!enabled || pinned) {
      this.stop();
      this.show(true);
    } else if (sticky) {
      this.stop();
      // Touch, just started hiding (full screen on): straight to the picture. A pin let go
      // (a press or focus on the buttons) leaves them showing: only asking hides them.
      if (!wasEnabled || !wasSticky) this.show(false);
      else if (!was) this.show(true);
    } else if (!was || wasSticky) {
      // Just started hiding (full screen on, or a pin let go): a full moment before they go.
      this.show(true);
      this.arm();
    }
  }

  /** A click, key or mouse move: show them (again) for a moment. Touch (sticky): show them,
   *  and they stay until hidden. */
  poke(): void {
    this.show(true);
    if (this.enabled && !this.pinned && !this.sticky) this.arm();
  }

  /** A long press on the picture: show them if hidden, hide them if shown. */
  toggle(): void {
    if (this.visible) this.hide();
    else this.poke();
  }

  /** Hide them now (the handle or the Hide button). */
  hide(): void {
    if (!this.enabled || this.pinned) return;
    this.stop();
    this.show(false);
  }

  dispose(): void {
    this.stop();
  }

  private arm(): void {
    this.stop();
    this.timer = setTimeout(() => {
      this.timer = null;
      this.show(false);
    }, this.delay);
  }

  private stop(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  private show(on: boolean): void {
    if (this.shown === on) return;
    this.shown = on;
    this.onChange(on);
  }
}

/** AutoHide for a component: whether the buttons show, and the poke and hide for its events. */
export function useAutoHide(
  enabled: boolean,
  pinned: boolean,
  sticky = false,
): { shown: boolean; poke: () => void; hide: () => void; toggle: () => void } {
  const [shown, setShown] = useState(true);
  const ctl = useRef<AutoHide | null>(null);
  useEffect(() => {
    const a = new AutoHide(setShown);
    ctl.current = a;
    return () => {
      a.dispose();
      ctl.current = null;
    };
  }, []);
  useEffect(() => {
    ctl.current?.set(enabled, pinned, sticky);
  }, [enabled, pinned, sticky]);
  const poke = useCallback(() => ctl.current?.poke(), []);
  const hide = useCallback(() => ctl.current?.hide(), []);
  const toggle = useCallback(() => ctl.current?.toggle(), []);
  return { shown: !enabled || pinned || shown, poke, hide, toggle };
}
