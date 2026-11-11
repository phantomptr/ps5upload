import { useEffect, useId, useRef, useState } from "react";
import { AlertTriangle } from "lucide-react";

import { Button } from ".";
import { activateFocusTrap } from "../lib/focusTrap";
import { useScrollLock } from "../lib/useScrollLock";
import { useTr } from "../state/lang";

/**
 * In-tree confirm modal that replaces the browser-native
 * `window.confirm()` and `tauri-plugin-dialog`'s `confirm` command.
 *
 * Why we don't use either of those:
 * - `window.confirm` is a no-op inside the Tauri webview (returns
 *   `undefined`/`false` silently with no UI), so calls fall through
 *   to `tauri-plugin-dialog`'s `confirm` command.
 * - `tauri-plugin-dialog`'s `confirm` is ACL-gated. Granting the
 *   permission in `capabilities/` would let us use a native dialog,
 *   but the rest of the app already uses custom React modals
 *   (PlaylistsPanel delete, Library Move) for consistency, and the
 *   native dialog can't carry our theming.
 *
 * Use the `useConfirm()` hook to get an imperative API:
 *   const { confirm, dialog } = useConfirm();
 *   const ok = await confirm({
 *     title: "Delete file",
 *     message: "This can't be undone.",
 *     destructive: true,
 *   });
 *
 * Render `{dialog}` somewhere in the component tree (anywhere; it
 * renders into a fixed full-screen overlay).
 */

export interface ConfirmOptions {
  title: string;
  message?: string;
  /** A list that goes with the message (the files this acts on), scrolled when long. */
  details?: string[];
  /** Label of the affirmative button. Defaults to "Confirm". */
  confirmLabel?: string;
  /** Label of the cancel button. Defaults to "Cancel". */
  cancelLabel?: string;
  /** When true, the confirm button uses the destructive (red)
   *  variant — appropriate for delete/unmount/discard actions. */
  destructive?: boolean;
  /** Close as a Cancel after this long: a question nobody is there to answer must not hold up
   *  work behind it. */
  cancelAfterMs?: number;
}

interface PendingState extends ConfirmOptions {
  resolve: (ok: boolean) => void;
}

/** Which button a confirm opens on. The affirmative one, so Enter confirms without a Tab, except
 *  for a destructive action: there an Enter still held from the press that opened the dialog
 *  (or pressed by habit) must cancel, not delete. */
export function confirmInitialFocus(destructive: boolean | undefined): "confirm" | "cancel" {
  return destructive ? "cancel" : "confirm";
}

export function useConfirm(): {
  confirm: (opts: ConfirmOptions) => Promise<boolean>;
  dialog: React.ReactNode;
} {
  const tr = useTr();
  const [pending, setPending] = useState<PendingState | null>(null);
  useScrollLock(!!pending);
  // Stable id pair so aria-labelledby/-describedby on the dialog wire
  // up to the right elements without colliding with other dialogs.
  const titleId = useId();
  const bodyId = useId();
  // Element that had focus when confirm() was called — restored on
  // close so keyboard users return to the trigger button instead of
  // dropping back to <body>. Captured imperatively in confirm().
  const previousFocusRef = useRef<HTMLElement | null>(null);
  // The panel, for the focus trap: Tab must stay on the dialog's own buttons
  // rather than walk on to whatever sits behind the scrim.
  const panelRef = useRef<HTMLDivElement | null>(null);

  const confirm = (opts: ConfirmOptions) =>
    new Promise<boolean>((resolve) => {
      // Capture focus owner before the modal mounts and steals focus
      // via autoFocus on the confirm button.
      if (typeof document !== "undefined") {
        const active = document.activeElement;
        previousFocusRef.current =
          active instanceof HTMLElement ? active : null;
      }
      setPending({ ...opts, resolve });
    });

  const settle = (ok: boolean) => {
    if (pending) {
      pending.resolve(ok);
      setPending(null);
      // Restore focus to whichever control opened the dialog. Done
      // after state set so React's commit has cleared the modal first
      // — focusing the previous element while the modal is still
      // mounted would put the user back inside it.
      const prev = previousFocusRef.current;
      previousFocusRef.current = null;
      if (prev && typeof document !== "undefined") {
        // Defer one tick to let React unmount the dialog first;
        // otherwise focus would land on the modal's now-removed
        // confirm button.
        queueMicrotask(() => prev.focus({ preventScroll: true }));
      }
    }
  };

  // Escape closes as cancel, Enter as confirm. Matches the affordance
  // a user expects from a modal dialog and removes a friction point
  // (some users hit Enter to dismiss; without this they were getting
  // confused that nothing happened).
  useEffect(() => {
    if (!pending) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Enter") {
        // Enter on a focused button is that button's own press. Without this
        // check, Tab to "Cancel" then Enter confirmed the action instead.
        const active = document.activeElement;
        if (
          active instanceof HTMLButtonElement &&
          panelRef.current?.contains(active)
        ) {
          return;
        }
        e.preventDefault();
        settle(true);
      }
    };
    window.addEventListener("keydown", handler);
    const expiry = pending.cancelAfterMs
      ? setTimeout(() => settle(false), pending.cancelAfterMs)
      : undefined;
    // Escape goes through the shared modal stack so it closes only this
    // dialog, not a Modal it was opened from.
    const releaseTrap = activateFocusTrap(() => panelRef.current, window, {
      onEscape: (e) => {
        e.preventDefault();
        settle(false);
      },
    });
    return () => {
      window.removeEventListener("keydown", handler);
      if (expiry) clearTimeout(expiry);
      releaseTrap();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending]);

  const dialog = pending ? (
    <div
      className="anim-scrim scrim fixed inset-0 z-[60] flex items-center justify-center p-4"
      onClick={() => settle(false)}
    >
      <div
        // alertdialog (rather than plain dialog) tells assistive tech
        // this is a destructive/important confirmation needing
        // immediate attention. aria-modal + aria-labelledby +
        // aria-describedby give screen readers the title and body
        // copy as soon as the modal opens.
        ref={panelRef}
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={pending.message ? bodyId : undefined}
        className="anim-pop glass-float elev-3 max-h-[calc(100dvh-2rem)] w-full max-w-md overflow-y-auto rounded-[var(--radius-panel)] p-6"
        onClick={(e) => e.stopPropagation()}
      >
        <header
          id={titleId}
          className="mb-2 flex items-center gap-2 text-base font-semibold"
        >
          {pending.destructive && (
            <AlertTriangle size={14} className="text-[var(--color-bad)]" />
          )}
          <span>{pending.title}</span>
        </header>
        {pending.message && (
          <p
            id={bodyId}
            className="mb-5 whitespace-pre-line text-sm text-[var(--color-muted)]"
          >
            {pending.message}
          </p>
        )}
        {pending.details && pending.details.length > 0 && (
          <ul
            className="mb-5 max-h-48 overflow-auto rounded-[var(--radius-field)] border border-[var(--color-border)] px-3 py-2 font-mono text-xs"
            data-testid="confirm-details"
          >
            {pending.details.map((line) => (
              <li key={line} className="break-all">
                {line}
              </li>
            ))}
          </ul>
        )}
        <div className="flex items-center justify-end gap-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => settle(false)}
            // A destructive confirm starts on Cancel: an Enter still held from the press that
            // opened the dialog (or pressed by habit) must not delete anything.
            autoFocus={confirmInitialFocus(pending.destructive) === "cancel"}
            data-testid="confirm-cancel"
          >
            {pending.cancelLabel ?? tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant={pending.destructive ? "danger" : "primary"}
            size="sm"
            onClick={() => settle(true)}
            // Auto-focus so the user can confirm with Enter without
            // tabbing first. Not for a destructive action: Cancel has the focus there.
            autoFocus={confirmInitialFocus(pending.destructive) === "confirm"}
            data-testid="confirm-ok"
          >
            {pending.confirmLabel ?? tr("confirm", undefined, "Confirm")}
          </Button>
        </div>
      </div>
    </div>
  ) : null;

  return { confirm, dialog };
}

/* ─────────────────────────────────────────────────────────────────────
 * useAlert + usePrompt — sibling hooks for the other window.* dialogs
 * that are no-ops inside Tauri's webview. Same shape as useConfirm:
 * imperative API returning a Promise, plus a `dialog` element to mount.
 *
 * Why we ship our own rather than enable tauri-plugin-dialog's native
 * dialogs: same reasons documented for useConfirm above — ACL gates,
 * theming consistency, and the existing modal pattern.
 * ──────────────────────────────────────────────────────────────────── */

export interface AlertOptions {
  title: string;
  message?: string;
  /** Button label. Defaults to "OK". */
  okLabel?: string;
}

interface AlertPending extends AlertOptions {
  resolve: () => void;
}

export function useAlert(): {
  alert: (opts: AlertOptions) => Promise<void>;
  dialog: React.ReactNode;
} {
  const tr = useTr();
  const [pending, setPending] = useState<AlertPending | null>(null);
  useScrollLock(!!pending);
  const titleId = useId();
  const bodyId = useId();
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);

  const alert = (opts: AlertOptions) =>
    new Promise<void>((resolve) => {
      if (typeof document !== "undefined") {
        const active = document.activeElement;
        previousFocusRef.current =
          active instanceof HTMLElement ? active : null;
      }
      setPending({ ...opts, resolve });
    });

  const settle = () => {
    if (pending) {
      pending.resolve();
      setPending(null);
      const prev = previousFocusRef.current;
      previousFocusRef.current = null;
      if (prev && typeof document !== "undefined") {
        queueMicrotask(() => prev.focus({ preventScroll: true }));
      }
    }
  };

  useEffect(() => {
    if (!pending) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        settle();
      }
    };
    window.addEventListener("keydown", handler);
    const releaseTrap = activateFocusTrap(() => panelRef.current, window, {
      onEscape: (e) => {
        e.preventDefault();
        settle();
      },
    });
    return () => {
      window.removeEventListener("keydown", handler);
      releaseTrap();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending]);

  const dialog = pending ? (
    <div
      className="anim-scrim scrim fixed inset-0 z-[60] flex items-center justify-center p-4"
      onClick={settle}
    >
      <div
        ref={panelRef}
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={pending.message ? bodyId : undefined}
        className="anim-pop glass-float elev-3 max-h-[calc(100dvh-2rem)] w-full max-w-md overflow-y-auto rounded-[var(--radius-panel)] p-6"
        onClick={(e) => e.stopPropagation()}
      >
        <header
          id={titleId}
          className="mb-2 flex items-center gap-2 text-base font-semibold"
        >
          <span>{pending.title}</span>
        </header>
        {pending.message && (
          <p
            id={bodyId}
            className="mb-5 whitespace-pre-wrap text-sm text-[var(--color-muted)]"
          >
            {pending.message}
          </p>
        )}
        <div className="flex items-center justify-end">
          <Button variant="primary" size="sm" onClick={settle} autoFocus>
            {pending.okLabel ?? tr("ok", undefined, "OK")}
          </Button>
        </div>
      </div>
    </div>
  ) : null;

  return { alert, dialog };
}

export interface PromptOptions {
  title: string;
  message?: string;
  /** Pre-filled value. */
  defaultValue?: string;
  /** Placeholder text when empty. */
  placeholder?: string;
  /** Affirmative button label. Defaults to "OK". */
  okLabel?: string;
  /** Cancel button label. Defaults to "Cancel". */
  cancelLabel?: string;
}

interface PromptPending extends PromptOptions {
  resolve: (val: string | null) => void;
}

export function usePrompt(): {
  prompt: (opts: PromptOptions) => Promise<string | null>;
  dialog: React.ReactNode;
} {
  const tr = useTr();
  const [pending, setPending] = useState<PromptPending | null>(null);
  useScrollLock(!!pending);
  const [value, setValue] = useState("");
  const titleId = useId();
  const bodyId = useId();
  const inputRef = useRef<HTMLInputElement | null>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);

  const prompt = (opts: PromptOptions) =>
    new Promise<string | null>((resolve) => {
      if (typeof document !== "undefined") {
        const active = document.activeElement;
        previousFocusRef.current =
          active instanceof HTMLElement ? active : null;
      }
      setValue(opts.defaultValue ?? "");
      setPending({ ...opts, resolve });
    });

  const settle = (val: string | null) => {
    if (pending) {
      pending.resolve(val);
      setPending(null);
      setValue("");
      const prev = previousFocusRef.current;
      previousFocusRef.current = null;
      if (prev && typeof document !== "undefined") {
        queueMicrotask(() => prev.focus({ preventScroll: true }));
      }
    }
  };

  useEffect(() => {
    if (!pending) return;
    // Focus the input on mount so the user can type immediately.
    queueMicrotask(() => inputRef.current?.select());
    const releaseTrap = activateFocusTrap(() => panelRef.current, window, {
      onEscape: (e) => {
        e.preventDefault();
        settle(null);
      },
    });
    return releaseTrap;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending]);

  const dialog = pending ? (
    <div
      className="anim-scrim scrim fixed inset-0 z-[60] flex items-center justify-center p-4"
      onClick={() => settle(null)}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={pending.message ? bodyId : undefined}
        className="anim-pop glass-float elev-3 max-h-[calc(100dvh-2rem)] w-full max-w-md overflow-y-auto rounded-[var(--radius-panel)] p-6"
        onClick={(e) => e.stopPropagation()}
      >
        <header
          id={titleId}
          className="mb-2 text-base font-semibold"
        >
          {pending.title}
        </header>
        {pending.message && (
          <p
            id={bodyId}
            className="mb-3 whitespace-pre-wrap text-xs text-[var(--color-muted)]"
          >
            {pending.message}
          </p>
        )}
        <input
          ref={inputRef}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              settle(value);
            }
          }}
          placeholder={pending.placeholder}
          className="input mb-4"
        />
        <div className="flex items-center justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={() => settle(null)}>
            {pending.cancelLabel ?? tr("cancel", undefined, "Cancel")}
          </Button>
          <Button variant="primary" size="sm" onClick={() => settle(value)}>
            {pending.okLabel ?? tr("ok", undefined, "OK")}
          </Button>
        </div>
      </div>
    </div>
  ) : null;

  return { prompt, dialog };
}
