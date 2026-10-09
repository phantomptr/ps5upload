import { CheckCircle2, Loader2, XCircle } from "lucide-react";

import type { CopyActivity } from "../../state/copyActivity";
import { useTr } from "../../state/lang";

/** One line saying what is happening to a copy, with a way to the screen that shows it. */
export function ActivityLine({
  activity,
  onOpen,
}: {
  activity: CopyActivity | null;
  onOpen: () => void;
}) {
  const tr = useTr();
  if (!activity) return null;
  const pct = (n: number | null) => (n === null ? "" : ` ${n}%`);
  const text =
    activity.phase === "building"
      ? tr("collection.act_building", { pct: pct(activity.pct) }, "Building the image{pct}")
      : activity.phase === "queued"
        ? tr("collection.act_queued", undefined, "Waiting in the queue")
        : activity.phase === "sending"
          ? activity.installing
            ? tr("collection.act_installing", { pct: pct(activity.pct) }, "Installing{pct}")
            : tr("collection.act_sending", { pct: pct(activity.pct) }, "Sending{pct}")
          : activity.phase === "done"
            ? activity.installing
              ? tr("collection.act_installed", undefined, "Installed")
              : tr("collection.act_sent", undefined, "On the PS5")
            : activity.message || tr("collection.act_failed", undefined, "Didn't work");
  const tone =
    activity.phase === "failed"
      ? "text-[var(--color-bad)]"
      : activity.phase === "done"
        ? "text-[var(--color-good)]"
        : "text-[var(--color-text)]";
  const Icon =
    activity.phase === "failed" ? XCircle : activity.phase === "done" ? CheckCircle2 : Loader2;
  return (
    <span className={`inline-flex min-w-0 items-center gap-1.5 text-xs ${tone}`}>
      <Icon
        size={13}
        className={`shrink-0 ${activity.phase === "sending" || activity.phase === "building" ? "animate-spin" : ""}`}
      />
      <span className="truncate">{text}</span>
      <button
        type="button"
        onClick={onOpen}
        className="shrink-0 text-[var(--color-accent)] underline-offset-2 hover:underline"
      >
        {tr("collection.act_open", undefined, "Open")}
      </button>
    </span>
  );
}
