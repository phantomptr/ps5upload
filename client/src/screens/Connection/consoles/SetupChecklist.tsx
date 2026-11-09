// The setup checklist inside a console's card: reach it, run the helper, ready. Shown only while
// that console is not ready; the elfldr warning and the payloads tip live here, not on the page.

import { useEffect, useRef, useState } from "react";
import { CheckCircle2, Circle, Moon, Send, XCircle } from "lucide-react";

import { Button } from "../../../components";
import { useTr } from "../../../state/lang";
import { checkConsole, closedPortText, payloadManagerReadyText, type ConsoleCheck } from "../../../lib/consoleCheck";
import { useConnectionStore } from "../../../state/connection";
import { helperSendFor, useHelperSendStore } from "../../../state/helperSend";
import { sendHelperTo } from "../../../state/helperSendRuntime";
import { clearHelperStopped } from "../../../state/helperStopped";
import { usePairingStore } from "../../../state/pairing";
import { isTauriEnv } from "../../../lib/tauriEnv";
import { hostOf } from "../../../lib/addr";
import { useHelperReplaceGuard } from "../useHelperReplaceGuard";
import PathCheck from "../PathCheck";
import { BringUpPanel } from "../BringUpPanel";
import type { CardStatus, SetupStep } from "./model";
import {
  BundledPayloadBanner,
  ElfldrFirstCallout,
  FirstRunNudge,
  SendPayloadPointer,
  sendButtonLabel,
} from "./setupBits";

type Mark = "done" | "now" | "todo" | "fail" | "rest";

function StepMark({ mark }: { mark: Mark }) {
  if (mark === "done") return <CheckCircle2 size={18} className="text-[var(--color-good)]" aria-hidden />;
  if (mark === "fail") return <XCircle size={18} className="text-[var(--color-bad)]" aria-hidden />;
  if (mark === "rest") return <Moon size={18} className="text-[var(--color-accent-bright)]" aria-hidden />;
  return (
    <Circle
      size={18}
      className={mark === "now" ? "text-[var(--color-accent)]" : "text-[var(--color-muted)]"}
      aria-hidden
    />
  );
}

function Step({
  n,
  mark,
  title,
  children,
  testId,
}: {
  n: number;
  mark: Mark;
  title: string;
  children?: React.ReactNode;
  testId: string;
}) {
  return (
    <li className="flex gap-3" data-testid={testId} data-mark={mark}>
      <div className="flex flex-col items-center">
        <StepMark mark={mark} />
        <span className="mt-1 w-px flex-1 bg-[var(--color-border)]" aria-hidden />
      </div>
      <div className="min-w-0 flex-1 pb-4">
        <div className={`text-sm font-medium ${mark === "todo" ? "text-[var(--color-muted)]" : ""}`}>
          <span className="sr-only">{n}. </span>
          {title}
        </div>
        {children ? <div className="mt-2 flex flex-col gap-2">{children}</div> : null}
      </div>
    </li>
  );
}

/** The check's answer in words, and whether it says "load elfldr first". */
function checkText(
  tr: ReturnType<typeof useTr>,
  target: string,
  c: ConsoleCheck,
): { ok: boolean; rest: boolean; text: string; elfldr: boolean } {
  switch (c.kind) {
    case "loader":
      return {
        ok: true,
        rest: false,
        elfldr: false,
        text: c.helperRunning
          ? tr("connection_payload_running", { host: target }, `Helper is running on ${target}`)
          : tr("connection_port_open", { port: 9021, host: target }, `Port 9021 is open on ${target}`),
      };
    case "helper":
      return { ok: true, rest: false, elfldr: false, text: tr("connection_payload_running", { host: target }, `Helper is running on ${target}`) };
    case "payload-manager":
      return { ok: true, rest: false, elfldr: false, text: payloadManagerReadyText(tr) };
    case "rest":
      return {
        ok: false,
        rest: true,
        elfldr: false,
        text: tr("connection_rest_mode", undefined, "Your PS5 is in rest mode. Wake it, then check again."),
      };
    case "no-loader":
      return { ok: false, rest: false, elfldr: true, text: closedPortText(tr, target, c) };
    case "unreachable":
      return { ok: false, rest: false, elfldr: false, text: closedPortText(tr, target, c) };
  }
}

export function SetupChecklist({
  host,
  status,
  active = false,
}: {
  host: string;
  status: CardStatus;
  /** The selected console: Quick bring-up acts on that one. */
  active?: boolean;
}) {
  const tr = useTr();
  const target = host.trim();
  const { guard, dialog } = useHelperReplaceGuard();
  const send = useHelperSendStore((st) => helperSendFor(st, target));
  const openPairing = usePairingStore((s) => s.openFor);
  const [checking, setChecking] = useState(false);
  const [check, setCheck] = useState<ReturnType<typeof checkText> | null>(null);
  /** Bumped by each Check: the check by path (home, then Tailscale) runs once for it. */
  const [pathNonce, setPathNonce] = useState(0);
  const [elapsed, setElapsed] = useState(0);
  const started = useRef<number | null>(null);
  const busy = send?.state === "busy";

  useEffect(() => {
    if (!busy) {
      started.current = null;
      setElapsed(0);
      return;
    }
    started.current ??= send?.startedAtMs ?? Date.now();
    const id = window.setInterval(() => setElapsed(Date.now() - (started.current ?? Date.now())), 250);
    return () => window.clearInterval(id);
  }, [busy, send?.startedAtMs]);

  const runCheck = async () => {
    if (!target) return;
    clearHelperStopped(target);
    setChecking(true);
    const c = await checkConsole(target);
    if (c.kind === "rest") useConnectionStore.getState().setHostStatus(target, { power: "rest" });
    setCheck(checkText(tr, target, c));
    setPathNonce((n) => n + 1);
    setChecking(false);
  };

  const runSend = async () => {
    if (!target || busy) return;
    if (!(await guard(target))) return;
    const conn = useConnectionStore.getState();
    const active = hostOf(conn.host) === hostOf(target);
    const replacing = active ? conn.payloadVersion : null;
    if (active) conn.setStatus({ payloadVersion: null, ps5Kernel: null });
    await sendHelperTo(target, tr, replacing);
  };

  const step: SetupStep = status.step;
  const reached = step !== "address" || !!check?.ok;
  const mark1: Mark = reached ? "done" : check?.rest ? "rest" : check && !check.ok ? "fail" : "now";
  const mark2: Mark =
    status.helper === "pairing" || step === "ready"
      ? "done"
      : send?.state === "fail"
        ? "fail"
        : reached
          ? "now"
          : "todo";
  const mark3: Mark = step === "ready" ? "done" : status.helper === "pairing" ? "now" : "todo";

  return (
    <section
      className="rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-accent)_30%,transparent)] bg-[var(--color-surface)] p-3 sm:p-4"
      aria-label={tr("cs_setup_title", undefined, "Finish setting up")}
      data-testid="consoles-checklist"
    >
      {dialog}
      <h3 className="mb-3 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {tr("cs_setup_title", undefined, "Finish setting up")}
      </h3>
      <ol className="flex flex-col">
        <Step n={1} mark={mark1} title={tr("cs_setup_reach", undefined, "Reach the PS5")} testId="consoles-step-reach">
          {!reached ? (
            <>
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "cs_setup_reach_hint",
                  undefined,
                  "Turn the PS5 on, on the same network as this device, and run your jailbreak's loader. Its address is in the PS5's network settings or your router's device list.",
                )}
              </p>
              <div className="flex flex-wrap items-center gap-2">
                <Button size="sm" variant="secondary" loading={checking} onClick={() => void runCheck()} data-testid="consoles-check">
                  {tr("connection_check", undefined, "Check")}
                </Button>
              </div>
            </>
          ) : null}
          {check ? (
            <p
              role={check.ok ? "status" : "alert"}
              className={`whitespace-pre-line break-words text-xs ${
                check.ok ? "text-[var(--color-good)]" : check.rest ? "" : "text-[var(--color-bad)]"
              }`}
            >
              {check.text}
            </p>
          ) : null}
          {/* Step 2 shows the same note while it waits: once is enough. */}
          {check?.elfldr && !(mark2 === "now" || mark2 === "fail") ? <ElfldrFirstCallout /> : null}
          {check && !check.ok && pathNonce > 0 ? <PathCheck host={target} nonce={pathNonce} /> : null}
        </Step>
        <Step n={2} mark={mark2} title={tr("cs_setup_helper", undefined, "Send the PS5Upload helper")} testId="consoles-step-helper">
          {mark2 === "now" || mark2 === "fail" ? (
            <>
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "connection_step2_hint_short",
                  undefined,
                  "A small program the PS5 runs to accept uploads. It starts in a few seconds.",
                )}
              </p>
              <ElfldrFirstCallout />
              {isTauriEnv() ? <BundledPayloadBanner /> : null}
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  size="sm"
                  variant="primary"
                  leftIcon={<Send size={13} />}
                  loading={busy}
                  disabled={busy}
                  onClick={() => void runSend()}
                  data-testid="consoles-send-helper"
                >
                  {sendButtonLabel(busy, send?.phase ?? null, elapsed, false, tr)}
                </Button>
              </div>
              {send?.state === "fail" && send.msg ? (
                <p role="alert" className="whitespace-pre-line break-words text-xs text-[var(--color-bad)]">
                  {send.msg}
                </p>
              ) : null}
              {isTauriEnv() ? <FirstRunNudge /> : null}
              {/* The one-tap path (bring-up playlist, helper, auto-loader); it sends from this
                  machine, so not in the browser, and it acts on the selected console. */}
              {isTauriEnv() && active ? <BringUpPanel /> : null}
              <SendPayloadPointer />
            </>
          ) : null}
        </Step>
        <Step n={3} mark={mark3} title={tr("cs_setup_ready", undefined, "Ready for uploads")} testId="consoles-step-ready">
          {status.helper === "pairing" ? (
            <>
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "cs_setup_pair_hint",
                  undefined,
                  "The helper runs, but this PS5 has not let this device in yet. Pair with the code on the TV, or approve this device on a device that already uses this PS5.",
                )}
              </p>
              <div>
                <Button size="sm" variant="primary" onClick={() => void openPairing(target)} data-testid="consoles-pair">
                  {tr("session_pair_button", undefined, "Pair…")}
                </Button>
              </div>
            </>
          ) : null}
        </Step>
      </ol>
    </section>
  );
}
