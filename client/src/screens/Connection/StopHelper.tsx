import { useState } from "react";
import { Power } from "lucide-react";

import { Button } from "../../components";
import { useConfirm, type ConfirmOptions } from "../../components/ConfirmDialog";
import { helperStop, type HelperStopResult } from "../../api/ps5";
import { hostOf } from "../../lib/addr";
import { transferScreenBusy } from "../../lib/ps5Transfers";
import { useConnectionStore } from "../../state/connection";
import { markHelperStopped } from "../../state/helperStopped";
import { clearHelperSend } from "../../state/helperSend";
import { useUploadQueueStore } from "../../state/uploadQueue";
import { useTr, type Translator } from "../../state/lang";
import { log } from "../../state/logs";

/**
 * "Stop helper" (#471): turn the console's helper off when you are done with it.
 *
 * Asks first, refuses while this app is moving something to or from the console (the engine
 * refuses too, for what it runs itself), and on success marks the console "stopped by you" so
 * the reconnect loop and the other unattended senders leave it off (state/helperStopped).
 */

export type StopHelperOutcome =
  | { kind: "cancelled" }
  | { kind: "stopped" }
  | { kind: "refused"; message: string };

export interface StopHelperDeps {
  confirm: (opts: ConfirmOptions) => Promise<boolean>;
  /** Something of this app's is using the console (an upload, a queue run). */
  busy: (host: string) => boolean;
  stop: (host: string) => Promise<HelperStopResult>;
  /** The helper is off: mark the console and show it down. */
  onStopped: (host: string) => void;
}

/** The whole action, its parts passed in so it can be tested without a console. */
export async function stopHelperFlow(
  host: string,
  deps: StopHelperDeps,
  tr: Translator,
): Promise<StopHelperOutcome> {
  const target = host.trim();
  if (!target) return { kind: "cancelled" };
  const ok = await deps.confirm({
    title: tr("helper_stop_confirm_title", undefined, "Stop the helper?"),
    message: tr(
      "helper_stop_confirm_body",
      undefined,
      "Uploads, installs and the console pages need the helper; send it again from here when you need it.",
    ),
    confirmLabel: tr("helper_stop_button", undefined, "Stop helper"),
    destructive: true,
  });
  if (!ok) return { kind: "cancelled" };
  const busyMessage = tr(
    "helper_stop_busy",
    undefined,
    "The helper is in use. Wait for the transfers and installs on this console to finish, then stop it.",
  );
  if (deps.busy(target)) return { kind: "refused", message: busyMessage };
  let r: HelperStopResult;
  try {
    r = await deps.stop(target);
  } catch (e) {
    const error = e instanceof Error ? e.message : String(e);
    return {
      kind: "refused",
      message: tr("helper_stop_failed", { error }, "Couldn't stop the helper: {error}"),
    };
  }
  if (r.state === "refused") {
    return {
      kind: "refused",
      message:
        r.reason === "busy"
          ? busyMessage
          : tr("helper_stop_failed", { error: r.error }, "Couldn't stop the helper: {error}"),
    };
  }
  deps.onStopped(target);
  return { kind: "stopped" };
}

/** The real dependencies. */
function liveDeps(confirm: StopHelperDeps["confirm"]): StopHelperDeps {
  return {
    confirm,
    busy: (h) =>
      transferScreenBusy(h) || !!useUploadQueueStore.getState().runningHosts[hostOf(h) || h],
    stop: helperStop,
    onStopped: (h) => {
      markHelperStopped(h);
      clearHelperSend(h);
      // Down now, not after the poller's miss debounce: the screens read "Stopped" at once.
      useConnectionStore
        .getState()
        .setHostStatus(h, { payloadStatus: "down", payloadVersion: null });
      log.info("connection", `helper stopped on ${h} by the user; it stays off until sent again`);
    },
  };
}

/** The button and what the last press ended with. */
export function StopHelperView({
  label,
  working,
  message,
  onClick,
  size = "md",
}: {
  label: string;
  working: boolean;
  message: string | null;
  onClick: () => void;
  size?: "sm" | "md";
}) {
  return (
    <div data-testid="stop-helper">
      <Button
        variant="secondary"
        size={size}
        leftIcon={<Power size={14} />}
        onClick={onClick}
        disabled={working}
        loading={working}
      >
        {label}
      </Button>
      {message && (
        <p role="alert" className="mt-2 text-xs text-[var(--color-warn)]">
          {message}
        </p>
      )}
    </div>
  );
}

export function StopHelperButton({ host, size }: { host: string; size?: "sm" | "md" }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [working, setWorking] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  async function onClick() {
    setMessage(null);
    // "Stopping…" once the user said yes, not while the question is open.
    const confirmThenWork = async (o: ConfirmOptions) => {
      const ok = await confirm(o);
      if (ok) setWorking(true);
      return ok;
    };
    try {
      const out = await stopHelperFlow(host, liveDeps(confirmThenWork), tr);
      if (out.kind === "refused") setMessage(out.message);
    } finally {
      setWorking(false);
    }
  }

  return (
    <>
      {dialog}
      <StopHelperView
        label={
          working
            ? tr("helper_stopping", undefined, "Stopping…")
            : tr("helper_stop_button", undefined, "Stop helper")
        }
        working={working}
        message={message}
        onClick={() => void onClick()}
        size={size}
      />
    </>
  );
}
