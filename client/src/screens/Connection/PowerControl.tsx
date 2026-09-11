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
import { netInterfacesGet, powerWake } from "../../api/ps5";
import { useRosterStore } from "../../state/roster";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { audit } from "../../state/auditLog";

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

  /* Learn the console's MAC while it is awake, so it can be woken later.
   *
   * Wake-on-LAN is the one power action the payload cannot perform — it is
   * not running once the console suspends. The address therefore has to be
   * captured in advance, and here is the natural place: this panel is on
   * screen exactly when the console is reachable. Recorded once per profile;
   * a failure is silent because nothing the user did has gone wrong. */
  const profiles = useRosterStore((st) => st.profiles);
  const activeId = useRosterStore((st) => st.active_id);
  const setMac = useRosterStore((st) => st.setMac);
  const profile = profiles.find((p) => p.id === activeId) ?? null;
  const knownMac = profile?.mac ?? "";

  useEffect(() => {
    if (!host || !profile || knownMac) return;
    void (async () => {
      try {
        const r = await netInterfacesGet(mgmtAddr(host));
        const wired = (r.interfaces ?? []).find(
          (i) => i.mac && i.mac !== "00:00:00:00:00:00" && i.ipv4 && i.ipv4 !== "0.0.0.0",
        );
        if (wired?.mac) setMac(profile.id, wired.mac);
      } catch {
        // Console asleep or payload down — nothing to record, and nothing
        // the user needs told about.
      }
    })();
  }, [host, profile, knownMac, setMac]);

  async function runWake() {
    if (!knownMac) return;
    setBusy("wake");
    setError(null);
    try {
      const r = await powerWake(knownMac, host);
      // Deliberately not "waking up": the packet is fire-and-forget UDP and
      // the console ignores it unless the user enabled waking from network.
      pushNotification(
        "info",
        tr("power_wake_sent", undefined, "Wake signal sent"),
        {
          body: tr("power_wake_sent_body", undefined,
            "If the console does not come up, turn on Settings → System → Power Saving → Features Available in Rest Mode → Enable Turning On PS5 from Network."),
        },
      );
      audit("system_wake", withConsolePrefix(host, `wake packet sent (${r.packets_sent ?? 0})`));
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
        {/* Wake is offered only once we have recorded a MAC, which happens
            the first time the console is reachable. Showing a dead button
            before then would promise something that cannot work. */}
        {knownMac ? (
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void runWake()}
            disabled={busy !== null}
            leftIcon={
              busy === "wake" ? <Spinner size={12} tone="inherit" /> : <Power size={12} />
            }
            title={tr("power_wake_hint", { mac: knownMac },
              `Sends a Wake-on-LAN packet to ${knownMac}. Requires "Enable Turning On PS5 from Network" on the console.`)}
          >
            {tr("power_action_wake", undefined, "Wake")}
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
