import { usePoll } from "../../lib/usePoll";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  Power,
  RotateCw,
  Moon,
  AlertTriangle,
  CheckCircle2,
} from "lucide-react";
import {
  powerReboot,
  powerShutdown,
  powerStandby,
  type PowerControlAck,
} from "../../api/ps5";
import { mgmtAddr } from "../../lib/addr";
import { Button, Spinner } from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { ddpStatus, type DdpStatus } from "../../api/ps5";
import { useConsoleWake } from "../../lib/useConsoleWake";
import { useHostRuntime } from "../../state/connection";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { audit } from "../../state/auditLog";
import { powerStateFromDdp, wakeUi, type PowerState } from "../../lib/wakeState";
import WakeSetup from "./WakeSetup";
import { consoleOnTailnet, useTailnet } from "../../state/useTailnet";
import { busyText } from "../../lib/helperGuard";
import { meshFor, othersBusy, useMeshStore } from "../../state/mesh";
import { trStatic } from "../../lib/trStatic";
import { humanizeCaught, humanizePs5Error } from "../../lib/humanizeError";

/**
 * Compact power-control panel — one row of buttons (reboot, standby,
 * shutdown) with confirmation before any destructive action.
 *
 * Lives inside the Connection screen so it's only available when a
 * PS5 is at least nominally reachable. Sends to the management port
 * (the console host). On the destructive actions we log a notification +
 * surface the connection-drop note from core (which is success).
 */
type PowerKind = "reboot" | "shutdown" | "standby";

function powerRequested(kind: PowerKind): string {
  if (kind === "reboot") return trStatic("power_reboot_requested", "PS5 restart requested");
  if (kind === "shutdown") return trStatic("power_shutdown_requested", "PS5 shutdown requested");
  return trStatic("power_standby_requested", "PS5 rest mode requested");
}

function powerFailed(kind: PowerKind): string {
  if (kind === "reboot") return trStatic("power_reboot_failed", "PS5 restart failed");
  if (kind === "shutdown") return trStatic("power_shutdown_failed", "PS5 shutdown failed");
  return trStatic("power_standby_failed", "PS5 rest mode failed");
}

export default function PowerControl({ host }: { host: string }) {
  const tr = useTr();
  const [runBusy, setBusy] = useState<null | "reboot" | "shutdown" | "standby">(null);
  const [last, setLast] = useState<PowerControlAck | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();

  const addr = mgmtAddr(host);

  /* Whether the console is awake, asleep, or not answering.
   *
   * Asked over the discovery protocol, which needs neither the payload nor a
   * credential — so it can tell "the console is in standby" apart from "the
   * helper is not running", which nothing else here can. */
  // THIS console's stored wake credentials (by address), not the selected profile's: the
  // card can show a console that is not the active tab.
  const wakeCtl = useConsoleWake(host);
  // A PS5 on Tailscale: a resting one is off Tailscale, so a wake works only at home. Said
  // plainly next to Wake instead of a wake that silently does nothing from away.
  const { status: tailnet } = useTailnet();
  const onTailnet = !!consoleOnTailnet(tailnet, host);
  const { profile, credential, hasSessionKeys } = wakeCtl;
  const rt = useHostRuntime(host);
  const [ddp, setDdp] = useState<DdpStatus | null>(null);

  // Shared poller: no overlap, nothing while hidden or while a transfer runs to this console.
  const tickDdp = useCallback(async () => {
    if (!host) return;
    const asked = host;
    try {
      const s = await ddpStatus(asked);
      if (asked === hostRef.current) setDdp(s);
    } catch {
      if (asked === hostRef.current) setDdp(null);
    }
  }, [host]);
  const hostRef = useRef(host);
  useEffect(() => {
    hostRef.current = host;
    setDdp(null);
  }, [host]);
  usePoll(tickDdp, 15000, { host, enabled: !!host });

  /* The console state and the stored credential together decide what the UI
     may offer — kept in one testable place rather than as scattered JSX
     conditionals. */
  // Until this card's own DDP poll answers, start from what the status poller already knows
  // (lib/consolePower): a console it saw asleep shows Wake at once instead of after 15 s.
  const power: PowerState = ddp
    ? powerStateFromDdp(ddp.code)
    : rt.power === "rest"
      ? "standby"
      : rt.payloadStatus === "up" || rt.power === "awake"
        ? "awake"
        : "offline";
  const ui = wakeUi(power, !!credential, hasSessionKeys);
  const busy = wakeCtl.busy ? "wake" : runBusy;
  // Rest mode, reboot and shut down go through the helper, which a sleeping console does not
  // run: offered only while it is not known to be asleep.
  const asleep = power === "standby";
  const helperActionTitle = asleep
    ? tr("status_ps5_rest_mode", undefined, "In rest mode")
    : undefined;
  const runWake = () => wakeCtl.wake();

  async function run(
    kind: "reboot" | "shutdown" | "standby",
    confirmText: string,
    fn: (addr: string) => Promise<PowerControlAck>,
  ) {
    if (!host?.trim()) return;
    const actionLabel =
      kind === "reboot"
        ? tr("power_action_reboot", undefined, "Reboot")
        : kind === "shutdown"
          ? tr("power_action_shutdown", undefined, "Shut down")
          : tr("power_action_standby", undefined, "Rest mode");
    // Other devices working on this console lose that work too (8.0.0): say who.
    const busy = othersBusy(meshFor(useMeshStore.getState(), host).state);
    const busyNote = busy.length
      ? `\n\n${tr(
          "mesh_power_busy",
          { work: busyText(busy) },
          `Other devices are using this console: ${busyText(busy)}. Their work stops too.`,
        )}`
      : "";
    const ok = await confirmDialog({
      title: tr("power_confirm_title", { kind: actionLabel }, `Confirm ${actionLabel}`),
      message: confirmText + busyNote,
      destructive: true,
      confirmLabel:
        kind === "reboot"
          ? tr("power_reboot", undefined, "Reboot")
          : kind === "shutdown"
            ? tr("power_shutdown", undefined, "Shutdown")
            : tr("power_standby", undefined, "Standby"),
    });
    if (!ok) return;
    setBusy(kind);
    setError(null);
    setLast(null);
    try {
      const ack = await fn(addr);
      // Record a "last action" only on success — setting it unconditionally
      // showed the green success line AND the red error line together when
      // the payload rejected the action (ack.ok === false).
      if (ack.ok) {
        setLast(ack);
      }
      if (ack.ok) {
        pushNotification(
          "info",
          withConsolePrefix(host, powerRequested(kind)),
          {
            body: ack.err
              ? ack.err
              : tr(
                  "power_ack_dispatched",
                  { kind: actionLabel, host },
                  `${actionLabel} dispatched to ${host}.`,
                ),
          },
        );
        audit(
          kind === "reboot"
            ? "system_reboot"
            : kind === "shutdown"
              ? "system_shutdown"
              : "system_standby",
          `PS5 ${kind}`,
          { context: host },
        );
      } else {
        const msg = ack.err
          ? humanizePs5Error(ack.err)
          : tr("error_unknown", undefined, "unknown error");
        setError(msg);
        pushNotification(
          "error",
          withConsolePrefix(host, powerFailed(kind)),
          {
            body: msg,
          },
        );
        audit(
          kind === "reboot"
            ? "system_reboot"
            : kind === "shutdown"
              ? "system_shutdown"
              : "system_standby",
          `PS5 ${kind} failed`,
          { context: host, failed: true },
        );
      }
    } catch (e) {
      const msg = humanizeCaught(e);
      setError(msg);
      pushNotification("error", withConsolePrefix(host, powerFailed(kind)), {
        body: msg,
      });
      audit(
        kind === "reboot"
          ? "system_reboot"
          : kind === "shutdown"
            ? "system_shutdown"
            : "system_standby",
        `PS5 ${kind} failed`,
        { context: host, failed: true },
      );
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="surface-panel px-5 py-4">
      {confirmDialogNode}
      <div className="mb-3 flex items-center gap-2 text-sm font-semibold">
        <Power size={15} className="text-[var(--color-accent-bright)]" />
        {tr("power_control_title", undefined, "PS5 power")}
        {power !== "awake" ? (
          <span className="ms-auto text-xs font-normal text-[var(--color-muted)]">
            {asleep
              ? tr("status_ps5_rest_mode", undefined, "In rest mode")
              : tr("status_ps5_not_answering", undefined, "Not answering")}
          </span>
        ) : null}
      </div>
      {power !== "awake" && onTailnet ? (
        <p className="mb-2 text-xs text-[var(--color-muted)]" data-testid="power-wake-tailnet-note">
          {tr(
            "power_wake_tailnet_note",
            undefined,
            "Away from home? A PS5 in rest mode is off Tailscale, so it can only be woken from its home network.",
          )}
        </p>
      ) : null}
      <div className="flex flex-wrap items-center gap-2">
        {/* Only when it can actually work: the console is asleep and a wake
            credential is stored. Every other state hides it rather than
            offer a button that does nothing. */}
        {ui.showWakeButton ? (
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void runWake()}
            disabled={busy !== null}
            leftIcon={
              busy === "wake" ? <Spinner size={12} tone="inherit" /> : <Power size={12} />
            }
            title={
              ui.canSignIn
                ? tr("power_wake_signin_hint", undefined,
                    "Wakes the console and signs in to your user, so it comes up on the home screen instead of user-select.")
                : tr("power_wake_hint", undefined,
                    "Wakes the console over Sony's discovery protocol. Requires Remote Play enabled on the console.")
            }
          >
            {busy === "wake" && ui.canSignIn
              ? tr("power_action_wake_signin_busy", undefined, "Waking & signing in…")
              : ui.canSignIn
                ? tr("power_action_wake_signin", undefined, "Wake & sign in")
                : tr("power_action_wake", undefined, "Wake")}
          </Button>
        ) : null}
        <Button
          variant="secondary"
          size="sm"
          leftIcon={
            busy === "standby" ? (
              <Spinner size={12} tone="inherit" />
            ) : (
              <Moon size={11} />
            )
          }
          onClick={() =>
            run(
              "standby",
              tr(
                "power_confirm_standby",
                undefined,
                "Enter rest mode on the PS5? Active downloads / uploads will be interrupted.",
              ),
              powerStandby,
            )
          }
          disabled={busy !== null || asleep}
          title={helperActionTitle}
        >
          {tr("power_action_standby", undefined, "Rest mode")}
        </Button>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={
            busy === "reboot" ? (
              <Spinner size={12} tone="inherit" />
            ) : (
              <RotateCw size={11} />
            )
          }
          onClick={() =>
            run(
              "reboot",
              tr(
                "power_confirm_reboot",
                undefined,
                "Reboot the PS5 now? You'll need to resend ps5upload + companions after it boots.",
              ),
              powerReboot,
            )
          }
          disabled={busy !== null || asleep}
          title={helperActionTitle}
        >
          {tr("power_action_reboot", undefined, "Reboot")}
        </Button>
        <Button
          variant="danger"
          size="sm"
          leftIcon={
            busy === "shutdown" ? (
              <Spinner size={12} tone="inherit" />
            ) : (
              <Power size={11} />
            )
          }
          onClick={() =>
            run(
              "shutdown",
              tr(
                "power_confirm_shutdown",
                undefined,
                "Shut down the PS5? You'll need to power it on manually before reconnecting.",
              ),
              powerShutdown,
            )
          }
          disabled={busy !== null || asleep}
          title={helperActionTitle}
        >
          {tr("power_action_shutdown", undefined, "Shut down")}
        </Button>
      </div>

      {/* Wake setup: the console-side settings and this console's wake code.
          Shown whenever there is no credential yet, and (collapsed) once
          there is, so the settings stay discoverable if wake stops working.
          Offline consoles are skipped — nothing here can be done or checked
          when we cannot see the console at all. */}
      {power !== "offline" && (ui.showSetup || credential) ? (
        <WakeSetup
          host={host}
          addr={addr}
          profileId={profile?.id ?? null}
          credential={credential}
          power={power}
          ui={ui}
        />
      ) : null}
      {last && (
        <div className="mt-2 flex items-start gap-1.5 text-xs">
          <CheckCircle2
            size={11}
            className="mt-0.5 shrink-0 text-[var(--color-good)]"
          />
          <span className="text-[var(--color-muted)]">
            {last.action ?? "ok"}
            {last.err && ` — ${last.err}`}
          </span>
        </div>
      )}
      {(error || wakeCtl.error) && (
        <div className="mt-2 flex items-start gap-1.5 text-xs text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          <span>{error || wakeCtl.error}</span>
        </div>
      )}
    </section>
  );
}
