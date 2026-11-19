import { X } from "lucide-react";

import { Spinner } from "../../../components";
import { useTr } from "../../../state/lang";
import { useStreamLaunch } from "./streamLaunch";

/** A "play on the stream" in progress, over the top of the picture: each step as it happens,
 *  a way to call it off, and why it stopped. */
export function LaunchStatus() {
  const tr = useTr();
  const p = useStreamLaunch((s) => s.pending);
  const clear = useStreamLaunch((s) => s.clear);
  if (!p) return null;
  const name = p.titleName || p.titleId;
  const text =
    p.stage === "stream"
      ? tr("rp_launch_stage_stream", { name }, `Starting the stream for ${name}…`)
      : p.stage === "helper"
        ? tr("rp_launch_stage_helper", { name }, `Waiting for the PS5's helper to start ${name}…`)
        : p.stage === "launch"
          ? tr("rp_launch_stage_launch", { name }, `Starting ${name}…`)
          : (p.error ?? "");
  const failed = p.stage === "failed";
  return (
    <div
      className="pointer-events-auto absolute left-1/2 top-16 z-30 flex w-max max-w-[min(32rem,calc(100%-2rem))] -translate-x-1/2 items-center gap-2 rounded-2xl bg-black/85 px-4 py-2 text-sm text-white shadow-lg ring-1 ring-white/10"
      role={failed ? "alert" : "status"}
      data-testid="rp-launch-status"
      data-stage={p.stage}
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onPointerDown={(e) => e.stopPropagation()}
    >
      {!failed && <Spinner size={14} />}
      <span className={`min-w-0 flex-1 ${failed ? "text-amber-300" : ""}`}>{text}</span>
      <button
        type="button"
        onClick={clear}
        aria-label={
          failed ? tr("rp_launch_dismiss", undefined, "Dismiss") : tr("rp_launch_cancel", undefined, "Cancel")
        }
        className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full hover:bg-white/15"
        data-testid="rp-launch-cancel"
      >
        <X size={14} />
      </button>
    </div>
  );
}
