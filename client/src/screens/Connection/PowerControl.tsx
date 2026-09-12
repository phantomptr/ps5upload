import { useState } from "react";
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
import { useEffect } from "react";
import { ddpStatus, powerWake, wakeAndSignIn, type DdpStatus } from "../../api/ps5";
import { useRosterStore } from "../../state/roster";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { audit } from "../../state/auditLog";
import { powerStateFromDdp, wakeUi } from "../../lib/wakeState";
import WakeSetup from "./WakeSetup";

/**
 * Compact power-control panel — one row of buttons (reboot, standby,
 * shutdown) with confirmation before any destructive action.
 *
 * Lives inside the Connection screen so it's only available when a
 * PS5 is at least nominally reachable. Sends to the management port
 * (host:9114). On the destructive actions we log a notification +
 * surface the connection-drop note from core (which is success).
 */
export default function PowerControl({ host }: { host: string }) {
  const tr = useTr();
  const [busy, setBusy] = useState<null | "reboot" | "shutdown" | "standby" | "wake">(
    null,
  );
  const [last, setLast] = useState<PowerControlAck | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();

  const addr = mgmtAddr(host);

  /* Whether the console is awake, asleep, or not answering.
   *
   * Asked over the discovery protocol, which needs neither the payload nor a
   * credential — so it can tell "the console is in standby" apart from "the
   * helper is not running", which nothing else here can. */
  const profiles = useRosterStore((st) => st.profiles);
  const activeId = useRosterStore((st) => st.active_id);
  const profile = profiles.find((p) => p.id === activeId) ?? null;
  const credential = profile?.wake_credential ?? "";
  const registKey = profile?.wake_regist_key ?? "";
  const rpKey = profile?.wake_rp_key ?? "";
  const hasSessionKeys = !!registKey && !!rpKey;
  const [ddp, setDdp] = useState<DdpStatus | null>(null);

  useEffect(() => {
    if (!host) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const s = await ddpStatus(host);
        if (!cancelled) setDdp(s);
      } catch {
        if (!cancelled) setDdp(null);
      }
    };
    void tick();
    const id = setInterval(() => void tick(), 15000);
    return () => { cancelled = true; clearInterval(id); };
  }, [host]);

  /* The console state and the stored credential together decide what the UI
     may offer — kept in one testable place rather than as scattered JSX
     conditionals. */
  const power = powerStateFromDdp(ddp?.code);
  const ui = wakeUi(power, !!credential, hasSessionKeys);

  async function runWake() {
    setBusy("wake");
    setError(null);
    try {
      if (ui.canSignIn) {
        // Wake AND sign the user in — a longer, confirmed operation: it waits
        // for the console to boot, then establishes the control session.
        await wakeAndSignIn(host, credential, registKey, rpKey);
        pushNotification(
          "success",
          tr("power_wake_signedin", undefined, "Woken and signed in"),
          {
            body: tr("power_wake_signedin_body", undefined,
              "The console is on and signed in to your user."),
          },
        );
        audit("system_wake", withConsolePrefix(host, "woken and signed in"));
        return;
      }
      if (!credential) return;
      await powerWake(host, credential);
      // Not "waking up": the console never acknowledges, and ignores the
      // request entirely unless Remote Play is enabled.
      pushNotification(
        "info",
        tr("power_wake_sent", undefined, "Wake signal sent"),
        {
          body: tr("power_wake_sent_body", undefined,
            "The console doesn't confirm a wake. If it stays asleep, check that Remote Play, Stay Connected to the Internet, and Turning On PS5 from Network are all enabled on the console."),
        },
      );
      audit("system_wake", withConsolePrefix(host, "wake request sent"));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  async function run(
    kind: "reboot" | "shutdown" | "standby",
    confirmText: string,
    fn: (addr: string) => Promise<PowerControlAck>,
  ) {
    if (!host?.trim()) return;
    const ok = await confirmDialog({
      title: tr("power_confirm_title", { kind }, `Confirm ${kind}`),
      message: confirmText,
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
          withConsolePrefix(host, `PS5 ${kind} requested`),
          {
            body: ack.err
              ? ack.err
              : tr(
                  "power_ack_dispatched",
                  { kind },
                  `${kind} dispatched to ${host}.`,
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
        const msg = ack.err ?? "unknown error";
        setError(msg);
        pushNotification(
          "error",
          withConsolePrefix(host, `PS5 ${kind} failed`),
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
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification("error", withConsolePrefix(host, `PS5 ${kind} failed`), {
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
    <section className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      {confirmDialogNode}
      <div className="mb-2 flex items-center gap-2 text-xs font-semibold">
        <Power size={12} />
        {tr("power_control_title", undefined, "PS5 power")}
      </div>
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
          disabled={busy !== null}
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
          disabled={busy !== null}
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
          disabled={busy !== null}
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
      {error && (
        <div className="mt-2 flex items-start gap-1.5 text-xs text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          <span>{error}</span>
        </div>
      )}
    </section>
  );
}
