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
import { ddpStatus, powerWake, type DdpStatus } from "../../api/ps5";
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

  /* Whether the console is awake, asleep, or not answering.
   *
   * Asked over the discovery protocol, which needs neither the payload nor a
   * credential — so it can tell "the console is in standby" apart from "the
   * helper is not running", which nothing else here can. */
  const profiles = useRosterStore((st) => st.profiles);
  const activeId = useRosterStore((st) => st.active_id);
  const setWakeCredential = useRosterStore((st) => st.setWakeCredential);
  const profile = profiles.find((p) => p.id === activeId) ?? null;
  const credential = profile?.wake_credential ?? "";
  const [ddp, setDdp] = useState<DdpStatus | null>(null);
  const [credentialDraft, setCredentialDraft] = useState("");

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

  const inStandby = ddp?.code === 620;

  async function runWake() {
    if (!credential) return;
    setBusy("wake");
    setError(null);
    try {
      await powerWake(host, credential);
      // Not "waking up": the console never acknowledges, and ignores the
      // request entirely unless Remote Play is enabled.
      pushNotification(
        "info",
        tr("power_wake_sent", undefined, "Wake signal sent"),
        {
          body: tr("power_wake_sent_body", undefined,
            "The console does not confirm a wake. If it stays asleep, check Settings → System → Remote Play → Enable Remote Play."),
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
        {/* Wake is offered only once we have recorded a MAC, which happens
            the first time the console is reachable. Showing a dead button
            before then would promise something that cannot work. */}
        {inStandby && credential ? (
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void runWake()}
            disabled={busy !== null}
            leftIcon={
              busy === "wake" ? <Spinner size={12} tone="inherit" /> : <Power size={12} />
            }
            title={tr("power_wake_hint", undefined,
              "Wakes the console over Sony's discovery protocol. Requires Remote Play enabled on the console.")}
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

      {/* Only when it can change the outcome: the console is asleep and we
          have no way to wake it. A credential cannot be derived — it has to
          be read out of the Remote Play app's own traffic. */}
      {inStandby && !credential ? (
        <div className="mt-2 flex flex-wrap items-center gap-2 text-xs">
          <span className="text-[var(--color-muted)]">
            {tr("power_wake_needs_credential", undefined,
              "To wake this console, paste its Remote Play user-credential:")}
          </span>
          <input
            className="input py-1 text-xs"
            style={{ width: "auto", minWidth: "12rem" }}
            placeholder={tr("power_wake_credential_placeholder", undefined, "user-credential")}
            value={credentialDraft}
            onChange={(e) => setCredentialDraft(e.target.value)}
          />
          <Button
            variant="secondary"
            size="sm"
            disabled={!credentialDraft.trim() || !profile}
            onClick={() => {
              if (profile) setWakeCredential(profile.id, credentialDraft);
              setCredentialDraft("");
            }}
          >
            {tr("power_wake_save_credential", undefined, "Save")}
          </Button>
        </div>
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
