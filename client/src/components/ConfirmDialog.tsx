import { useEffect, useId, useRef, useState } from "react";
import { AlertTriangle } from "lucide-react";

import { Button } from ".";
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
  /** Label of the affirmative button. Defaults to "Confirm". */
  confirmLabel?: string;
  /** Label of the cancel button. Defaults to "Cancel". */
  cancelLabel?: string;
  /** When true, the confirm button uses the destructive (red)
   *  variant — appropriate for delete/unmount/discard actions. */
  destructive?: boolean;
}

interface PendingState extends ConfirmOptions {
  resolve: (ok: boolean) => void;
}

export function useConfirm(): {
  confirm: (opts: ConfirmOptions) => Promise<boolean>;
  dialog: React.ReactNode;
} {
  const tr = useTr();
  const [pending, setPending] = useState<PendingState | null>(null);
  // Stable id pair so aria-labelledby/-describedby on the dialog wire
  // up to the right elements without colliding with other dialogs.
  const titleId = useId();
  const bodyId = useId();
  // Element that had focus when confirm() was called — restored on
  // close so keyboard users return to the trigger button instead of
  // dropping back to <body>. Captured imperatively in confirm().
  const previousFocusRef = useRef<HTMLElement | null>(null);

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
      if (e.key === "Escape") {
        e.preventDefault();
        settle(false);
      } else if (e.key === "Enter") {
        e.preventDefault();
        settle(true);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending]);

  const dialog = pending ? (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      onClick={() => settle(false)}
    >
      <div
        // alertdialog (rather than plain dialog) tells assistive tech
        // this is a destructive/important confirmation needing
        // immediate attention. aria-modal + aria-labelledby +
        // aria-describedby give screen readers the title and body
        // copy as soon as the modal opens.
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={pending.message ? bodyId : undefined}
        className="w-full max-w-md rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <header
          id={titleId}
          className="mb-2 flex items-center gap-2 text-sm font-semibold"
        >
          {pending.destructive && (
            <AlertTriangle size={14} className="text-[var(--color-bad)]" />
          )}
          <span>{pending.title}</span>
        </header>
        {pending.message && (
          <p
            id={bodyId}
            className="mb-4 text-xs text-[var(--color-muted)]"
          >
            {pending.message}
          </p>
        )}
        <div className="flex items-center justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={() => settle(false)}>
            {pending.cancelLabel ?? tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant={pending.destructive ? "danger" : "primary"}
            size="sm"
            onClick={() => settle(true)}
            // Auto-focus so the user can confirm with Enter without
            // tabbing first.
            autoFocus
          >
            {pending.confirmLabel ?? tr("confirm", undefined, "Confirm")}
          </Button>
        </div>
      </div>
    </div>
  ) : null;

  return { confirm, dialog };
}
