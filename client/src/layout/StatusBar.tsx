import { useState } from "react";
import { Camera, Check, Loader2, AlertTriangle } from "lucide-react";

import { useConnectionStore } from "../state/connection";
import { parsePS5Firmware } from "../lib/ps5Firmware";
import { useTr } from "../state/lang";
import { captureAppScreenshot } from "../lib/captureScreenshot";

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
  const engineError = useConnectionStore((s) => s.engineError);
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
    <div className="flex items-center gap-5 border-t border-[var(--color-border)] bg-[var(--color-surface-2)] px-4 pt-1.5 pb-[calc(env(safe-area-inset-bottom)_+_0.375rem)] pl-[calc(env(safe-area-inset-left)_+_1rem)] pr-[calc(env(safe-area-inset-right)_+_1rem)] text-xs text-[var(--color-muted)]">
      <div
        className="flex items-center gap-2"
        title={
          engineError ??
          tr("status_engine_tooltip", undefined, "ps5upload-engine sidecar on localhost:19113")
        }
      >
        {dot(engineStatus)} {tr("status_engine", undefined, "engine")}
      </div>
      <div className="flex items-center gap-2" title={tr("status_payload_tooltip", undefined, "PS5Upload helper on :9113")}>
        {dot(payloadStatus)} {tr("status_payload", undefined, "helper")}
        {payloadVersion && (
          <span className="rounded bg-[var(--color-surface-3)] px-1 font-mono text-xs">
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
          <span className="rounded bg-[var(--color-surface-3)] px-1 font-mono text-xs">
            {ps5Firmware
              ? tr("status_fw", { ver: ps5Firmware }, `FW ${ps5Firmware}`)
              : tr("status_kernel_ok", undefined, "kernel OK")}
          </span>
        </div>
      )}
      <div className="ml-auto">{tr("status_no_active_transfers", undefined, "no active transfers")}</div>
      <CaptureButton />
    </div>
  );
}

/**
 * Global screenshot capture. Lives in the always-visible footer so a user can
 * grab WHATEVER screen shows the problem (not just the Bug Report form), with
 * the capture landing in the gallery the Bug Report page selects from.
 */
function CaptureButton() {
  const tr = useTr();
  const [state, setState] = useState<"idle" | "busy" | "done" | "fail">("idle");

  const capture = async () => {
    if (state === "busy") return;
    setState("busy");
    try {
      // Let the button's own spinner paint before the (synchronous-ish)
      // DOM serialization briefly blocks the main thread.
      await new Promise((r) => setTimeout(r, 16));
      await captureAppScreenshot();
      setState("done");
    } catch {
      setState("fail");
    }
    setTimeout(() => setState("idle"), 1800);
  };

  const icon =
    state === "busy" ? (
      <Loader2 size={13} className="animate-spin" />
    ) : state === "done" ? (
      <Check size={13} className="text-[var(--color-good)]" />
    ) : state === "fail" ? (
      <AlertTriangle size={13} className="text-[var(--color-bad)]" />
    ) : (
      <Camera size={13} />
    );

  return (
    <button
      type="button"
      onClick={capture}
      disabled={state === "busy"}
      title={tr(
        "status_capture_tooltip",
        undefined,
        "Capture a screenshot of this screen for a bug report",
      )}
      aria-label={tr("status_capture", undefined, "Capture screenshot")}
      className="flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-[var(--color-surface-3)]"
    >
      {icon}
      <span className="hidden sm:inline">
        {state === "done"
          ? tr("status_capture_done", undefined, "Saved")
          : state === "fail"
            ? tr("status_capture_fail", undefined, "Failed")
            : tr("status_capture", undefined, "Capture")}
      </span>
    </button>
  );
}
