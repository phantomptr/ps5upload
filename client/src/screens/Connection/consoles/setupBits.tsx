// Pieces of the setup checklist that were the old three-step Connection wizard's: the elfldr
// warning, the payloads tip, the bundled helper line and the setup wizard nudge. They show inside a
// console's card only while that console is not ready, never as page furniture.

import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { AlertTriangle, ArrowRight, Send, Sparkles } from "lucide-react";

import { Button, ErrorCard } from "../../../components";
import { bundledPayloadInfo, type BundledPayloadInfo } from "../../../api/ps5";
import { formatBytes } from "../../../lib/format";
import { formatDate } from "../../../lib/formatDate";
import { humanizeCaught } from "../../../lib/humanizeError";
import { useTr, type Translator } from "../../../state/lang";
import type { HelperSendPhase } from "../../../state/helperSend";

/** The send button's words for where a send is: the user sees progress in what they pressed. */
export function sendButtonLabel(
  busy: boolean,
  phase: HelperSendPhase | null,
  elapsedMs: number,
  resend: boolean,
  tr: Translator,
): string {
  if (!busy) {
    return resend
      ? tr("connection_send_resend", undefined, "Resend helper")
      : tr("connection_send_send", undefined, "Send helper");
  }
  const sec = Math.max(1, Math.round(elapsedMs / 1000));
  if (phase === "locating") return tr("connection_send_locating", { sec }, `Locating ELF… (${sec}s)`);
  if (phase === "sending") return tr("connection_send_sending", { sec }, `Sending to PS5… (${sec}s)`);
  if (phase === "waiting") return tr("connection_send_waiting", { sec }, `Waiting for helper… (${sec}s)`);
  return tr("connection_send_working", { sec }, `Working… (${sec}s)`);
}

/** "Load elfldr first", where it can't be missed: the step users miss most (an autoloader
 *  without elfldr: the helper connects, then drops). */
export function ElfldrFirstCallout() {
  const tr = useTr();
  return (
    <div
      className="flex items-start gap-2 rounded-[var(--radius-field)] border border-[color-mix(in_srgb,var(--color-warn)_40%,transparent)] bg-[var(--color-warn-soft)] p-3 text-sm"
      data-testid="consoles-elfldr"
    >
      <AlertTriangle size={16} className="mt-0.5 shrink-0 text-[var(--color-warn)]" />
      <p className="min-w-0">
        <span className="font-semibold">
          {tr("connection_elfldr_first_title", undefined, "Load elfldr first.")}
        </span>{" "}
        {tr(
          "connection_elfldr_first_short",
          undefined,
          "With an autoloader or PLDMGR, put elfldr before ps5upload.elf, or the helper drops after a few seconds.",
        )}
      </p>
    </div>
  );
}

/** Where "Send payload" went: other payloads are sent from Payloads, not here. Users still look
 *  for that button where the helper is sent, so the checklist points at it. */
export function SendPayloadPointer() {
  const tr = useTr();
  const navigate = useNavigate();
  return (
    <div
      className="flex flex-wrap items-center gap-2 rounded-[var(--radius-field)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-3 text-xs"
      data-testid="connection-send-payload-pointer"
    >
      <Send size={14} className="shrink-0 text-[var(--color-accent)]" aria-hidden />
      <span className="min-w-[12rem] flex-1 text-[var(--color-muted)]">
        {tr(
          "connection_send_payload_moved",
          undefined,
          "Sending another payload (kstuff, etaHEN, any .elf)? That is in Payloads → Send payload. This screen sends only the PS5Upload helper.",
        )}
      </span>
      <Button
        variant="secondary"
        size="sm"
        rightIcon={<ArrowRight size={11} className="rtl:rotate-180" />}
        onClick={() => navigate("/payloads?tab=send")}
      >
        {tr("connection_send_payload_open", undefined, "Open Send payload")}
      </Button>
    </div>
  );
}

/** First time on a console: the setup wizard sends kstuff, ShadowMount+ and the helper in order. */
export function FirstRunNudge() {
  const tr = useTr();
  const navigate = useNavigate();
  return (
    <div className="flex flex-wrap items-start gap-3 rounded-[var(--radius-field)] border border-[color-mix(in_srgb,var(--color-accent)_35%,transparent)] bg-[var(--color-accent-soft)] p-3 text-xs">
      <Sparkles size={14} className="mt-0.5 shrink-0 text-[var(--color-accent)]" />
      <div className="min-w-[12rem] flex-1">
        <div className="font-semibold text-[var(--color-text)]">
          {tr("connection_first_run_nudge_title", undefined, "First time setting up?")}
        </div>
        <div className="text-[var(--color-muted)]">
          {tr(
            "connection_first_run_nudge_body_v2",
            undefined,
            "The setup wizard downloads kstuff and ShadowMount+ and sends them, then the helper, in the right order. If your console already loads kstuff itself (an autoloader, etaHEN), step 2 below sends just the helper.",
          )}
        </div>
      </div>
      <Button
        variant="primary"
        size="sm"
        rightIcon={<ArrowRight size={11} className="rtl:rotate-180" />}
        onClick={() => navigate("/first-run")}
      >
        {tr("connection_first_run_nudge_action", undefined, "Open setup wizard")}
      </Button>
    </div>
  );
}

/** Which ps5upload.elf a send uses: its size and build date. If the date is older than your last
 *  `make payload`, the app holds a stale bundled copy and needs a rebuild. */
export function BundledPayloadBanner() {
  const tr = useTr();
  const [info, setInfo] = useState<BundledPayloadInfo | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    bundledPayloadInfo()
      .then(setInfo)
      .catch((e) => setErr(humanizeCaught(e)));
  }, []);
  if (err) {
    return (
      <ErrorCard
        title={tr("connection_bundled_payload_unavailable", "Bundled helper not available")}
        detail={<div className="mt-0.5 break-words font-mono text-xs opacity-90">{err}</div>}
      />
    );
  }
  if (!info) return null;
  const built = info.mtime > 0 ? formatDate(new Date(info.mtime * 1000)) : "unknown";
  const basename = info.path.split(/[\\/]/).pop() ?? info.path;
  return (
    <div
      className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-[var(--color-muted)]"
      title={info.path}
    >
      <span>
        <span className="font-semibold text-[var(--color-text)]">{basename}</span> · {formatBytes(info.size)}
      </span>
      <span>{tr("connection_built", { date: built }, `built: ${built}`)}</span>
    </div>
  );
}
