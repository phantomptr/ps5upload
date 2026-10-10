import { ArrowLeftRight, Laptop, Loader2 } from "lucide-react";

import { useTr } from "../../state/lang";
import { formatBytes } from "../../lib/format";
import { formatEtaSeconds } from "../../lib/uploadEta";
import { useRateEta } from "../../lib/useRateEta";
import type { BottleneckCause, JobLive } from "../../lib/jobLive";

/** Catalog key and English fallback for each cause (written out so the i18n scripts see them). */
const CAUSE: Record<BottleneckCause, { key: string; text: string }> = {
  network: { key: "bottleneck_network", text: "network" },
  source: { key: "bottleneck_source_v2", text: "reading the source" },
  disk: { key: "bottleneck_disk", text: "console disk" },
  workers: { key: "bottleneck_workers", text: "console workers" },
  memory: { key: "bottleneck_memory", text: "console memory" },
};

/** "Limited by: network", one line. Renders nothing without a cause. */
export function BottleneckLine({ cause }: { cause: BottleneckCause | null }) {
  const tr = useTr();
  if (!cause) return null;
  const name = tr(CAUSE[cause].key, undefined, CAUSE[cause].text);
  return (
    <div className="text-xs text-[var(--color-muted)]" data-testid="bottleneck-line">
      {tr("upload_bottleneck_label", { cause: name }, `Limited by: ${name}`)}
    </div>
  );
}

/** The skipping phase of a 7z/RAR resume: the decoder is reading past data the console
 *  already holds. Without this line the wait looks like a stall. */
export function SkippingLine({ live }: { live: JobLive }) {
  const tr = useTr();
  if (!live.skipping) return null;
  const done = formatBytes(live.skipDoneBytes);
  const total = formatBytes(live.skipTotalBytes);
  return (
    <div
      className="flex items-center gap-1.5 text-xs text-[var(--color-warn)]"
      data-testid="skipping-line"
    >
      <Loader2 size={12} className="animate-spin" aria-hidden />
      <span>
        {tr(
          "upload_phase_skipping",
          { done, total },
          `Skipping data the console already has: ${done} of ${total}`,
        )}
      </span>
    </div>
  );
}

/** Files are still settling on the console after the job finished. When the engine sends the
 *  counts, says how many are left and, once the settle has a measurable pace, how long. */
export function SettlingLine({ live }: { live: JobLive }) {
  const tr = useTr();
  const total = live.settleTotal ?? 0;
  const left = live.settleLeft ?? 0;
  const { rate, etaSeconds } = useRateEta("settle", Math.max(0, total - left), total);
  if (!live.settling) return null;
  const counted = total > 0;
  return (
    <div
      className="flex flex-wrap items-center gap-x-1.5 text-xs text-[var(--color-warn)]"
      data-testid="settling-line"
    >
      <Loader2 size={12} className="animate-spin" aria-hidden />
      <span>{tr("upload_phase_settling", undefined, "Finishing on the console…")}</span>
      {counted && (
        <span className="font-mono" data-testid="settling-count">
          {tr(
            "upload_phase_settling_count",
            { left: left.toLocaleString(), total: total.toLocaleString() },
            `${left.toLocaleString()} of ${total.toLocaleString()} files left`,
          )}
          {rate > 0 && ` · ${rate < 10 ? rate.toFixed(1) : Math.round(rate)} ${tr("upload_phase_settling_rate", undefined, "files/s")}`}
          {etaSeconds !== null &&
            ` · ${tr("upload_phase_settling_eta", { time: formatEtaSeconds(etaSeconds) }, `about ${formatEtaSeconds(etaSeconds)} left`)}`}
        </span>
      )}
    </div>
  );
}

/** A finished upload whose files the console has not confirmed saved (the engine's commit_ack warning):
 *  a warning in the result, never a clean success. */
export function UnsettledLine({ live }: { live: JobLive | undefined }) {
  const tr = useTr();
  if (!live?.unsettled) return null;
  return (
    <div
      className="mt-1 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-warn)_40%,transparent)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-warn)]"
      data-testid="unsettled-warning"
      role="alert"
    >
      ⚠{" "}
      {tr(
        "upload_warn_unsettled",
        undefined,
        "Every byte reached the console, but it has not confirmed saving all files yet. They finish on their own; if the console loses power first, send the folder again.",
      )}
    </div>
  );
}

/** Everything a running job's live notes can say, in one block. Renders nothing when the
 *  engine sent no live fields. */
export function JobLiveNotes({ live }: { live: JobLive | undefined }) {
  if (!live) return null;
  return (
    <div className="mb-2 flex flex-col gap-0.5">
      <SkippingLine live={live} />
      <BottleneckLine cause={live.bottleneck} />
      <SettlingLine live={live} />
    </div>
  );
}

/** A console-to-console copy's route (#433): straight between the consoles (Beta), or through
 *  this computer and why. Nothing until the engine knows the route. */
export function RouteLine({ live }: { live: JobLive | undefined }) {
  const tr = useTr();
  if (!live?.route) return null;
  if (live.route === "direct") {
    return (
      <div className="mt-1 flex items-center gap-1.5 text-xs text-[var(--color-muted)]" data-testid="queue-route">
        <ArrowLeftRight size={12} className="shrink-0 text-[var(--color-accent)]" />
        <span>{tr("queue_route_direct", undefined, "Straight between the consoles")}</span>
        <span className="rounded bg-[var(--color-surface-3)] px-1 py-px text-[10px] font-medium uppercase tracking-wide">
          {tr("queue_route_beta", undefined, "Beta")}
        </span>
      </div>
    );
  }
  return (
    <div className="mt-1 flex min-w-0 items-start gap-1.5 text-xs text-[var(--color-muted)]" data-testid="queue-route">
      <Laptop size={12} className="mt-0.5 shrink-0" />
      <span className="min-w-0">
        {tr("queue_route_relay", undefined, "Through this computer")}
        {live.routeReason && (
          <span className="break-words"> · {live.routeReason}</span>
        )}
      </span>
    </div>
  );
}
