import { useCallback, useEffect, useId, useRef, useState } from "react";
import { X } from "lucide-react";

import { useTr } from "../../state/lang";
import { rpInput, useRemotePlayStore } from "../../state/remotePlay";
import { activateFocusTrap } from "../../lib/focusTrap";
import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { SettingsBody, type SettingsTab } from "./Settings";

/** The tab Settings opens on: the one last looked at, on this device. */
const TAB_KEY = "ps5upload.rp_settings_tab";
const TABS: SettingsTab[] = ["picture", "sound", "controls", "session"];

/** A drag down this far (px) on the phone sheet's top closes it. */
const SWIPE_CLOSE_PX = 72;

/** Marks the buttons that open and close Settings: a press on one is theirs (it toggles), not
 *  a press outside the drawer. */
export const SETTINGS_OPENER = { "data-rp-settings-opener": "" };

/** When a press outside last closed Settings (no backdrop): that press is not play. */
let outsideClosedAt = 0;
/** Whether a press that just landed closed Settings, so it should do nothing else. */
export function pressClosedSettings(): boolean {
  return performance.now() - outsideClosedAt < 500;
}

function savedTab(): SettingsTab {
  const v = safeGetItem(TAB_KEY);
  return TABS.includes(v as SettingsTab) ? (v as SettingsTab) : "picture";
}

/** Presses and keys inside the drawer are the drawer's: never the picture's, never the pad's
 *  (the drawer sits inside the stage in full screen, and React events bubble to it). */
const keep = {
  onClick: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onDoubleClick: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onPointerDown: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onPointerUp: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onPointerMove: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onContextMenu: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onKeyDown: (e: { stopPropagation(): void }) => e.stopPropagation(),
  onKeyUp: (e: { stopPropagation(): void }) => e.stopPropagation(),
};

/**
 * Remote Play's settings, over the stream: a panel at the right edge, or a sheet from the
 * bottom on a phone. In full screen it is drawn inside the picture's full-screen view, so
 * opening it never leaves full screen and the picture keeps playing beside or above it.
 *
 * While it is open the console gets no keys, controller or touch (rpInput.paused), so arrows,
 * typing and a controller work in the settings. Esc, ✕, a press outside, the Settings button
 * again and (on the sheet) a swipe down close it; focus goes back to where it was.
 */
export function SettingsDrawer({
  open,
  onClose,
  running,
  layout,
  contained,
  underBar,
  backdrop,
  onFocusBack,
}: {
  open: boolean;
  onClose: () => void;
  /** A stream is running: settings that apply only on the next start say so. */
  running: boolean;
  /** At the right edge, or a sheet from the bottom (phones). */
  layout: "side" | "sheet";
  /** Inside the full-screen picture (positioned in it), rather than over the page. */
  contained: boolean;
  /** Leave room at the top for the stream bar, which stays usable. */
  underBar: boolean;
  /** A light backdrop that takes presses beside the drawer (and closes it). Without one, a
   *  press anywhere outside still closes it but also does what it pressed. */
  backdrop: boolean;
  /** Where focus goes on close when the button that opened it is gone (a menu tile). */
  onFocusBack?: () => void;
}) {
  const tr = useTr();
  const idBase = useId();
  const [tab, setTabState] = useState<SettingsTab>(savedTab);
  const panel = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState(0);
  const dragFrom = useRef<{ y: number; id: number; moved: boolean } | null>(null);

  const setTab = (t: SettingsTab) => {
    setTabState(t);
    safeSetItem(TAB_KEY, t);
  };

  // Read through refs: the parent passes inline functions, and the open effect must not run
  // again (re-pausing, re-grabbing focus) on every render.
  const onCloseRef = useRef(onClose);
  const onFocusBackRef = useRef(onFocusBack);
  useEffect(() => {
    onCloseRef.current = onClose;
    onFocusBackRef.current = onFocusBack;
  });

  useEffect(() => {
    if (!open) return;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    // The console gets an idle pad from now until close: nothing held stays held.
    rpInput.paused = true;
    rpInput.held.clear();
    rpInput.touch = null;
    useRemotePlayStore.getState().stream?.sendPad();
    // A hidden pointer could not reach the settings.
    if (document.pointerLockElement) document.exitPointerLock?.();
    queueMicrotask(() => {
      const el = panel.current?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]');
      (el ?? panel.current)?.focus({ preventScroll: true });
    });
    // Tab stays inside; Esc closes the drawer (and is not the game's ○ or leave full screen).
    // On the document, capturing: before the picture's own key handling. A key being set in
    // Keyboard controls listens on the window first and keeps its Esc.
    const doc = {
      addEventListener: (t: "keydown", fn: (e: KeyboardEvent) => void) => document.addEventListener(t, fn, true),
      removeEventListener: (t: "keydown", fn: (e: KeyboardEvent) => void) =>
        document.removeEventListener(t, fn, true),
    };
    const release = activateFocusTrap(() => panel.current, doc, {
      onEscape: (e) => {
        e.preventDefault();
        e.stopPropagation();
        onCloseRef.current();
      },
    });
    return () => {
      release();
      rpInput.paused = false;
      useRemotePlayStore.getState().stream?.sendPad();
      queueMicrotask(() => {
        if (opener?.isConnected && opener.getClientRects().length > 0) opener.focus({ preventScroll: true });
        else onFocusBackRef.current?.();
      });
    };
  }, [open]);

  // Without a backdrop: a press anywhere outside closes it (the buttons that toggle it excepted).
  useEffect(() => {
    if (!open || backdrop) return;
    const away = (e: PointerEvent) => {
      const t = e.target as Element | null;
      if (!t || panel.current?.contains(t) || t.closest?.("[data-rp-settings-opener]")) return;
      outsideClosedAt = performance.now();
      onCloseRef.current();
    };
    document.addEventListener("pointerdown", away, true);
    return () => document.removeEventListener("pointerdown", away, true);
  }, [open, backdrop]);

  // The sheet follows a finger dragging its top down; far enough, it closes.
  const onDragDown = useCallback((e: React.PointerEvent) => {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    dragFrom.current = { y: e.clientY, id: e.pointerId, moved: false };
  }, []);
  const onDragMove = useCallback((e: React.PointerEvent) => {
    const d = dragFrom.current;
    if (!d || d.id !== e.pointerId) return;
    const dy = e.clientY - d.y;
    if (!d.moved) {
      if (dy < 8) return;
      d.moved = true;
      // Once it is a drag, the drag has the finger (a tap on a tab stays a tap).
      (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    }
    setDrag(Math.max(0, dy));
  }, []);
  const onDragEnd = useCallback(
    (e: React.PointerEvent) => {
      const d = dragFrom.current;
      if (!d || d.id !== e.pointerId) return;
      dragFrom.current = null;
      const dy = e.clientY - d.y;
      setDrag(0);
      if (d.moved && dy > SWIPE_CLOSE_PX) onClose();
    },
    [onClose],
  );

  if (!open) return null;

  const sheet = layout === "sheet";
  const pos = contained ? "absolute" : "fixed";
  const labels: Record<SettingsTab, string> = {
    picture: tr("rp_set_picture", undefined, "Picture"),
    sound: tr("rp_set_sound", undefined, "Sound"),
    controls: tr("rp_set_controls", undefined, "Controls"),
    session: tr("rp_set_session", undefined, "Session"),
  };
  const onTabKey = (e: React.KeyboardEvent) => {
    const i = TABS.indexOf(tab);
    let next = -1;
    if (e.key === "ArrowRight") next = (i + 1) % TABS.length;
    else if (e.key === "ArrowLeft") next = (i - 1 + TABS.length) % TABS.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = TABS.length - 1;
    if (next < 0) return;
    e.preventDefault();
    setTab(TABS[next]);
    document.getElementById(`${idBase}-tab-${TABS[next]}`)?.focus();
  };
  // Under the stream bar in full screen on a computer; else a small margin from the edges.
  const top = underBar ? "calc(max(0.75rem, env(safe-area-inset-top)) + 3.25rem)" : "max(0.5rem, env(safe-area-inset-top))";
  const tabList = (
    <div
      role="tablist"
      aria-label={tr("rp_settings", undefined, "Settings")}
      // On a phone the four share the row evenly; at the side they sit at their own width.
      className={sheet ? "grid min-w-0 flex-1 grid-cols-4 gap-1" : "flex min-w-0 flex-1 gap-1.5"}
      onKeyDown={onTabKey}
    >
      {TABS.map((t) => (
        <button
          key={t}
          id={`${idBase}-tab-${t}`}
          type="button"
          role="tab"
          aria-selected={tab === t}
          aria-controls={`${idBase}-panel`}
          tabIndex={tab === t ? 0 : -1}
          onClick={() => setTab(t)}
          className={`chip min-h-8 whitespace-nowrap text-xs ${sheet ? "min-w-0 justify-center overflow-hidden px-1" : "shrink-0 px-3"}`}
          data-testid={`rp-settings-tab-${t}`}
        >
          <span className="truncate">{labels[t]}</span>
        </button>
      ))}
    </div>
  );
  const closeButton = (
    <button
      type="button"
      onClick={onClose}
      aria-label={tr("rp_settings_close", undefined, "Close settings")}
      title={tr("rp_settings_close", undefined, "Close settings")}
      className="grid h-9 w-9 shrink-0 place-items-center rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] text-[var(--color-muted)] shadow-[var(--edge-highlight)] hover:text-[var(--color-text)]"
      data-testid="rp-settings-close"
    >
      <X size={16} aria-hidden />
    </button>
  );
  const dragProps = sheet
    ? { onPointerDown: onDragDown, onPointerMove: onDragMove, onPointerUp: onDragEnd, onPointerCancel: onDragEnd }
    : {};

  return (
    <>
      {backdrop && (
        <div
          className={`${pos} inset-x-0 bottom-0 z-40 ${sheet ? "bg-black/30" : "bg-black/10"} anim-scrim`}
          style={{ top: underBar ? top : 0 }}
          onClick={(e) => {
            e.stopPropagation();
            onClose();
          }}
          onDoubleClick={(e) => e.stopPropagation()}
          onPointerDown={(e) => e.stopPropagation()}
          onPointerUp={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.preventDefault()}
          data-testid="rp-settings-backdrop"
        />
      )}
      <div
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${idBase}-title`}
        tabIndex={-1}
        data-testid="rp-settings"
        data-layout={layout}
        className={`${pos} z-50 flex flex-col overflow-hidden text-[var(--color-text)] outline-none glass-float elev-3 ${
          sheet
            ? "inset-x-0 bottom-0 mx-auto w-full max-w-xl rounded-t-[var(--radius-panel)] border-b-0 anim-sheet-up"
            : "right-2 w-[min(26rem,calc(100%-1rem))] rounded-[var(--radius-panel)] anim-side"
        }`}
        style={
          sheet
            ? {
                maxHeight: contained ? "85%" : "85dvh",
                paddingBottom: "env(safe-area-inset-bottom)",
                // `translate`, not `transform`: the entrance animation holds the transform.
                translate: drag ? `0 ${drag}px` : undefined,
                transition: drag ? "none" : "translate 160ms ease-out",
              }
            : {
                top,
                bottom: "max(0.5rem, env(safe-area-inset-bottom))",
                marginRight: "env(safe-area-inset-right)",
              }
        }
        {...keep}
      >
        <div className={sheet ? "touch-none" : undefined} {...dragProps}>
          {sheet && (
            <div className="flex justify-center pt-2" aria-hidden data-testid="rp-settings-grip">
              <span className="h-1 w-10 rounded-full bg-[var(--color-muted)] opacity-40" />
            </div>
          )}
          {/* On a phone the tabs share the row with ✕: the sheet keeps its height for settings. */}
          <div className={`flex items-center gap-3 px-5 pb-3 ${sheet ? "pt-1" : "pt-4"}`}>
            <h2 id={`${idBase}-title`} className={sheet ? "sr-only" : "min-w-0 flex-1 truncate text-base font-semibold"}>
              {tr("rp_settings", undefined, "Settings")}
            </h2>
            {sheet && tabList}
            {closeButton}
          </div>
          {!sheet && <div className="px-5 pb-3">{tabList}</div>}
        </div>
        <div
          id={`${idBase}-panel`}
          role="tabpanel"
          aria-labelledby={`${idBase}-tab-${tab}`}
          className="min-h-0 flex-1 overflow-y-auto overscroll-contain border-t border-[var(--color-border)] px-5 pb-5 pt-4 text-sm"
        >
          {running && (
            <p className="mb-4 text-xs text-[var(--color-muted)]" data-testid="rp-settings-paused">
              {tr(
                "rp_settings_paused",
                undefined,
                "The game gets no keys or controller presses while Settings is open.",
              )}
            </p>
          )}
          <SettingsBody tab={tab} running={running} />
        </div>
      </div>
    </>
  );
}
