import { useEffect, useRef } from "react";
import { onNavAction, type NavAction } from "../nav";
import { t } from "../strings";
import { useBackLevel } from "../backHistory";
import { useModalFocus } from "./restoreFocus";

/**
 * The dialog's controller handling: ○ is "no". Every other action is swallowed
 * while it is open (the page behind never sees them anyway: nav.ts scopes
 * handlers to the open modal).
 */
export function dialogNavHandler(onCancel: () => void): (action: NavAction) => boolean {
  return (action) => {
    if (action === "back") onCancel();
    return true;
  };
}

/**
 * "Are you sure?" for anything that cannot be undone: close a game, cancel a
 * task, delete. A nav modal: the D-pad stays inside it,
 * focus starts on the safe choice (listed first), ○ closes it, and focus goes
 * back where it was when it closes (restoreFocus: a neighbour if that is gone).
 */
export function ConfirmDialog({
  title,
  body,
  confirmLabel,
  cancelLabel,
  danger,
  busy,
  error,
  altLabel,
  onAlt,
  onConfirm,
  onCancel,
}: {
  title: string;
  body?: string;
  confirmLabel: string;
  /** Default: "Cancel". */
  cancelLabel?: string;
  /** The confirm button in the warning colour. */
  danger?: boolean;
  /** The action is running: a second ✕ does nothing. */
  busy?: boolean;
  /** Why the last try did not work, shown inside the dialog (it stays open to try again). */
  error?: string | null;
  /** A second way to say yes (Uninstall and delete), in the warning colour, after the confirm button. */
  altLabel?: string;
  onAlt?: () => void;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const safe = useRef<HTMLButtonElement>(null);

  // Focus starts on the safe choice; on close it goes back to the opener, or to
  // the row now in its place when the opener is gone (a deleted row).
  useModalFocus(safe);
  // Open: the browser's Back (○) is "no".
  useBackLevel(true);

  useEffect(() => onNavAction(dialogNavHandler(onCancel), () => root.current), [onCancel]);

  return (
    <div className="cw-scrim">
      <div
        ref={root}
        className="cw-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="cw-dialog-title"
        aria-describedby={body ? "cw-dialog-body" : undefined}
        aria-busy={busy ? true : undefined}
        data-nav-modal
      >
        <h2 id="cw-dialog-title">{title}</h2>
        {body ? <p id="cw-dialog-body">{body}</p> : null}
        {error ? (
          <p className="cw-field-error" role="alert">
            {error}
          </p>
        ) : null}
        <div className="cw-dialog-buttons">
          <button ref={safe} type="button" className="cw-focusable cw-button" data-nav-default onClick={onCancel}>
            <span>{cancelLabel ?? t("cw_dialog_cancel")}</span>
          </button>
          <button
            type="button"
            className={`cw-focusable cw-button ${danger ? "is-danger" : "is-primary"}`}
            onClick={() => {
              if (!busy) onConfirm();
            }}
          >
            <span>{confirmLabel}</span>
          </button>
          {altLabel && onAlt ? (
            <button
              type="button"
              className="cw-focusable cw-button is-danger"
              onClick={() => {
                if (!busy) onAlt();
              }}
            >
              <span>{altLabel}</span>
            </button>
          ) : null}
        </div>
      </div>
    </div>
  );
}
