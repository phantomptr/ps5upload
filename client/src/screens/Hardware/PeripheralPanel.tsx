import { useState } from "react";
import {
  Disc3,
  Power,
  Loader2,
  AlertTriangle,
  CheckCircle2,
} from "lucide-react";
import {
  peripheralEject,
  peripheralBdOff,
  peripheralBdOn,
  type PeripheralAck,
} from "../../api/ps5";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";

/**
 * Peripheral control: BD drive eject / power.
 *
 * Skipping USB port toggles in the default UI — they're rarely
 * useful (modern USB devices handle their own power) and the
 * `port` argument needs a port-picker. We expose them via the API
 * for advanced workflows but the panel keeps to BD only.
 */
export default function PeripheralPanel({ mgmtAddr }: { mgmtAddr: string }) {
  const tr = useTr();
  const [busy, setBusy] = useState<null | "eject" | "off" | "on">(null);
  const [last, setLast] = useState<PeripheralAck | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function run(
    kind: "eject" | "off" | "on",
    fn: (addr: string) => Promise<PeripheralAck>,
  ) {
    setBusy(kind);
    setError(null);
    try {
      const ack = await fn(mgmtAddr);
      setLast(ack);
      if (!ack.ok) {
        setError(ack.err ?? `code ${ack.code}`);
      } else {
        pushNotification(
          "info",
          tr(
            "peripheral_action_done",
            { action: ack.action ?? kind },
            `BD ${ack.action ?? kind} requested`,
          ),
        );
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center gap-2">
        <Disc3 size={14} />
        <h3 className="text-sm font-semibold">
          {tr("peripheral_title", undefined, "Disc drive")}
        </h3>
      </header>
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="secondary"
          size="sm"
          leftIcon={
            busy === "eject" ? (
              <Loader2 size={11} className="animate-spin" />
            ) : (
              <Disc3 size={11} />
            )
          }
          onClick={() => run("eject", peripheralEject)}
          disabled={busy !== null}
        >
          {tr("peripheral_eject", undefined, "Eject disc")}
        </Button>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={
            busy === "off" ? (
              <Loader2 size={11} className="animate-spin" />
            ) : (
              <Power size={11} />
            )
          }
          onClick={() => run("off", peripheralBdOff)}
          disabled={busy !== null}
        >
          {tr("peripheral_bd_off", undefined, "BD power off")}
        </Button>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={
            busy === "on" ? (
              <Loader2 size={11} className="animate-spin" />
            ) : (
              <Power size={11} />
            )
          }
          onClick={() => run("on", peripheralBdOn)}
          disabled={busy !== null}
        >
          {tr("peripheral_bd_on", undefined, "BD power on")}
        </Button>
      </div>
      {last && last.ok && !error && (
        <div className="mt-2 flex items-center gap-1 text-[11px] text-[var(--color-good)]">
          <CheckCircle2 size={11} />
          {last.action ?? "ok"}
        </div>
      )}
      {error && (
        <div className="mt-2 flex items-start gap-1 text-[11px] text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          <span>{error}</span>
        </div>
      )}
      <p className="mt-3 text-[10px] text-[var(--color-muted)]">
        {tr(
          "peripheral_explainer",
          undefined,
          "BD power-off can save 2-3W when you only play digital titles. Eject works whether the drive has a disc or not.",
        )}
      </p>
    </section>
  );
}
