import { useEffect } from "react";
import { RefreshCw, X } from "lucide-react";

import { Button } from "../components";
import { hostOf } from "../lib/addr";
import type { UpdateHold } from "../lib/helperUpdatePrompt";
import { useHelperReplaceGuard } from "../screens/Connection/useHelperReplaceGuard";
import { helperSendFor, useHelperSendStore, type HelperSendState } from "../state/helperSend";
import {
  dismissHelperPrompt,
  holdText,
  requestHelperUpdate,
  runHelperUpdate,
  startHelperUpdatePrompt,
  takeHelperUpdateRequest,
  useHelperUpdatePromptStore,
  type HelperPromptView,
} from "../state/helperUpdatePrompt";
import { useTr } from "../state/lang";

/**
 * The part of "the PS5 helper is out of date" that has to live in the shell (#941): it keeps the
 * notices in step with the status poller, and it owns the questions the replace guard asks, so
 * the Update helper button works from a toast, from Home and from anywhere else that asks.
 * Renders nothing but those dialogs.
 */
export default function HelperUpdatePromptHost() {
  const tr = useTr();
  const { guard, dialog } = useHelperReplaceGuard();
  const request = useHelperUpdatePromptStore((s) => s.request);
  useEffect(() => startHelperUpdatePrompt(), []);
  useEffect(() => {
    if (!request) return;
    // Once, whichever mounted shell sees it first.
    const host = takeHelperUpdateRequest();
    if (host) void runHelperUpdate(host, tr, guard);
  }, [request, tr, guard]);
  return <>{dialog}</>;
}

export interface HelperUpdateNoticeViewProps {
  view: HelperPromptView | null;
  /** Why the last press did not send. */
  hold: UpdateHold | null;
  /** The send the button started, while it runs or after it failed. */
  send: HelperSendState | null;
  onUpdate: () => void;
  onDismiss: () => void;
}

export function HelperUpdateNoticeView({ view, hold, send, onUpdate, onDismiss }: HelperUpdateNoticeViewProps) {
  const tr = useTr();
  if (!view) return null;
  const sending = send?.state === "busy";
  return (
    <div
      role="status"
      data-testid="helper-update-notice"
      className="mt-4 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-warn)_35%,transparent)] bg-[var(--color-warn-soft)] p-3 text-[var(--color-text)]"
    >
      <div className="flex items-start gap-2.5">
        <RefreshCw size={16} aria-hidden className="mt-0.5 shrink-0 text-[var(--color-warn)]" />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium">{tr("helper_update_title", undefined, "The PS5 helper is out of date")}</p>
          <p className="mt-0.5 text-xs leading-relaxed text-[var(--color-muted)]">
            {tr(
              "helper_update_body",
              { helper: view.from, app: view.to },
              "This PS5 runs helper v{helper}. This app is v{app}. Update the helper so new fixes and features work.",
            )}
          </p>
        </div>
        <button
          type="button"
          onClick={onDismiss}
          className="-me-1 -mt-1 shrink-0 rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] max-md:p-2.5"
          aria-label={tr("dismiss", undefined, "Dismiss")}
          title={tr("dismiss", undefined, "Dismiss")}
          data-testid="helper-update-dismiss"
        >
          <X size={14} />
        </button>
      </div>
      <Button
        className="mt-3 w-full"
        size="sm"
        variant="primary"
        leftIcon={<RefreshCw size={13} />}
        loading={sending}
        disabled={sending}
        onClick={onUpdate}
        data-testid="helper-update-now"
      >
        {tr("helper_update_action", undefined, "Update helper")}
      </Button>
      {hold && !sending ? (
        <p className="mt-2 text-xs leading-relaxed text-[var(--color-muted)]" data-testid="helper-update-hold">
          {holdText(tr, hold)}
        </p>
      ) : send?.msg ? (
        <p
          className={`mt-2 whitespace-pre-line text-xs leading-relaxed ${
            send.state === "fail" ? "text-[var(--color-bad)]" : "text-[var(--color-muted)]"
          }`}
          data-testid="helper-update-step"
        >
          {send.msg}
        </p>
      ) : null}
    </div>
  );
}

/** Home's notice for `host`: its helper is older than this app, with the button that updates it. */
export function HelperUpdateNotice({ host }: { host: string }) {
  const key = hostOf(host) || host;
  const view = useHelperUpdatePromptStore((s) => s.byHost[key] ?? null);
  const hold = useHelperUpdatePromptStore((s) => s.holds[key] ?? null);
  const send = useHelperSendStore((s) => helperSendFor(s, host));
  return (
    <HelperUpdateNoticeView
      view={view}
      hold={hold}
      send={send}
      onUpdate={() => requestHelperUpdate(key)}
      onDismiss={() => dismissHelperPrompt(key)}
    />
  );
}
