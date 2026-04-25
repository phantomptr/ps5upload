import { useConnectionStore } from "../state/connection";
import { parsePS5Firmware } from "../lib/ps5Firmware";
import { useTr } from "../state/lang";

/**
 * App-footer status strip: engine + payload liveness, plus the versions
 * reported by the PS5 side when it's reachable.
 *
 * PS5 firmware display: we parse the user-visible firmware number (e.g.
 * "9.60") out of the kernel string for the primary display. The full
 * kernel build string ("FreeBSD 11.0 r218215/releases/09.60 …") stays
 * available as a tooltip. Before this change the footer truncated the
 * kernel string mid-word, hiding the firmware value users actually
 * want to see.
 */
export default function StatusBar() {
  const tr = useTr();
  const engineStatus = useConnectionStore((s) => s.engineStatus);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const payloadVersion = useConnectionStore((s) => s.payloadVersion);
  const ps5Kernel = useConnectionStore((s) => s.ps5Kernel);
  const ps5Firmware = parsePS5Firmware(ps5Kernel);

  const dot = (status: "up" | "down" | "unknown") => {
    const color =
      status === "up"
        ? "bg-[var(--color-good)]"
        : status === "down"
        ? "bg-[var(--color-bad)]"
        : "bg-[var(--color-muted)]";
    return <span className={`inline-block h-2 w-2 rounded-full ${color}`} aria-hidden />;
  };

  return (
    <div className="flex items-center gap-5 border-t border-[var(--color-border)] bg-[var(--color-surface-2)] px-4 py-1.5 text-xs text-[var(--color-muted)]">
      <div className="flex items-center gap-2" title={tr("status_engine_tooltip", undefined, "ps5upload-engine sidecar on localhost:19113")}>
        {dot(engineStatus)} {tr("status_engine", undefined, "engine")}
      </div>
      <div className="flex items-center gap-2" title={tr("status_payload_tooltip", undefined, "PS5 payload on :9113")}>
        {dot(payloadStatus)} {tr("status_payload", undefined, "payload")}
        {payloadVersion && (
          <span className="rounded bg-[var(--color-surface-3)] px-1 font-mono text-[10px]">
            v{payloadVersion}
          </span>
        )}
      </div>
      {ps5Kernel && (
        <div
          className="flex items-center gap-2"
          title={ps5Kernel}
        >
          <span>PS5</span>
          <span className="rounded bg-[var(--color-surface-3)] px-1 font-mono text-[10px]">
            {ps5Firmware ? `FW ${ps5Firmware}` : tr("status_kernel_ok", undefined, "kernel OK")}
          </span>
        </div>
      )}
      <div className="ml-auto">{tr("status_no_active_transfers", undefined, "no active transfers")}</div>
    </div>
  );
}
