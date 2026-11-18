import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { Stethoscope, X } from "lucide-react";

import { rate, slowNow, slowTransferHint } from "../../lib/netDoctor";
import type { BottleneckCause } from "../../lib/jobLive";
import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { useTr } from "../../state/lang";
import { speedTestFor, useSpeedTestStore } from "../../state/speedTest";
import { useUploadSettingsStore } from "../../state/uploadSettings";

/** Dismissed until this time (ms), on this computer only. */
const DISMISS_KEY = "ps5upload.netdoctor.hintDismissedUntil";
const DISMISS_FOR_MS = 7 * 24 * 60 * 60_000;

function dismissedNow(): boolean {
  const until = Number(safeGetItem(DISMISS_KEY) ?? 0);
  return Number.isFinite(until) && until > Date.now();
}

/** "8 MiB/s, at 4 MiB/s": a cap is typed with up to one decimal. */
function bandwidthCapFor(cap: number): string {
  return Number.isInteger(cap) ? String(cap) : cap.toFixed(1);
}

/** A running upload far below what the link should do, for half a minute: one quiet line
 *  saying what holds it back (the engine's own measurement: the network, the source on this
 *  computer, the console's drive or workers, or a speed limit), with the network doctor a click
 *  away. Shown only with a measured reason, and dismissed for a week with one click, so it
 *  never nags. */
export function SlowTransferHint({
  host,
  bytesPerSec,
  bottleneck,
}: {
  host: string | null;
  bytesPerSec: number;
  bottleneck: BottleneckCause | null;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const tested = useSpeedTestStore((s) => {
    const st = host ? speedTestFor(s, host) : null;
    return st?.phase === "done" ? st.uploadBps : null;
  });
  const cap = useUploadSettingsStore((s) => s.bandwidthCapMbps);
  const setCap = useUploadSettingsStore((s) => s.setBandwidthCapMbps);
  const [dismissed, setDismissed] = useState(dismissedNow);
  const [since, setSince] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());

  const slow = slowNow({
    bytesPerSec,
    bottleneck,
    speedTestUploadBps: tested,
    bandwidthCapMbps: cap,
  });
  // The progress ticks re-render this every half second; the clock is read there.
  useEffect(() => {
    const t = Date.now();
    setNow(t);
    setSince((s) => (slow ? (s ?? t) : null));
  }, [slow, bytesPerSec]);

  if (!host) return null;
  const hint = slowTransferHint({
    bytesPerSec,
    bottleneck,
    sustainedMs: since === null ? 0 : now - since,
    speedTestUploadBps: tested,
    bandwidthCapMbps: cap,
  });
  // Dismissing quiets the network advice; a speed limit holding the upload back is always
  // said, with the way to remove it (one set by accident went unseen otherwise).
  if (!hint || (dismissed && hint.cause !== "cap")) return null;

  const speed = rate(bytesPerSec);
  const expected = hint.expectedBps ? rate(hint.expectedBps) : null;
  const lead =
    hint.cause === "network"
      ? null
      : expected
        ? tr(
            "netdoctor.hint.below_test",
            { speed, expected },
            "This upload runs at {speed}, well below the {expected} the speed test measured.",
          )
        : tr(
            "netdoctor.hint.runs_at",
            { speed },
            "This upload runs at {speed}.",
          );
  const because =
    hint.cause === "network"
      ? expected
        ? tr(
            "netdoctor.hint.tested",
            { speed, expected },
            "This upload runs at {speed}, well below the {expected} the speed test measured, and the network is what holds it back.",
          )
        : tr(
            "netdoctor.hint.untested",
            { speed },
            "This upload runs at {speed}, and the network is what holds it back.",
          )
      : hint.cause === "source"
        ? tr(
            "netdoctor.hint.cause_source",
            undefined,
            "Reading the files on this computer is what holds it back, not the network: a USB or hard drive, a network share (NAS) or unpacking an archive is slower than the link. Copying the game to an internal SSD first, or uploading an archive's files once unpacked, gives full speed.",
          )
        : hint.cause === "disk"
          ? tr(
              "netdoctor.hint.cause_disk",
              undefined,
              "The PS5's drive is what holds it back, not the network: an external USB drive, a nearly full drive or thousands of small files (each costs time of its own) write slower than the link delivers. A game image (.ffpkg or .exfat) goes as one file.",
            )
          : hint.cause === "workers"
            ? tr(
                "netdoctor.hint.cause_workers",
                undefined,
                "The PS5 is still writing what it received, not waiting on the network. With many small files each one costs time of its own; a game image (.ffpkg or .exfat) goes as one file.",
              )
            : tr(
                "netdoctor.hint.cause_cap_v2",
                { cap: bandwidthCapFor(cap) },
                "The upload speed limit (about {cap} MiB/s, set in Settings › Uploads › Speed) is what holds it back.",
              );
  return (
    <div
      className="mb-2 flex flex-wrap items-center gap-2 rounded-[var(--radius-field)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-xs"
      data-testid="slow-transfer-hint"
      data-cause={hint.cause}
      role="status"
    >
      <Stethoscope
        size={12}
        aria-hidden
        className="shrink-0 text-[var(--color-muted)]"
      />
      <span className="min-w-0 flex-[1_1_14rem] text-[var(--color-muted)]">
        {lead ? `${lead} ${because}` : because}
      </span>
      {hint.cause === "cap" && (
        <button
          type="button"
          className="font-medium text-[var(--color-accent)] hover:underline"
          onClick={() => setCap(0)}
          data-testid="slow-transfer-hint-remove-cap"
        >
          {tr("bandwidth_cap_clear", undefined, "remove limit")}
        </button>
      )}
      <button
        type="button"
        className="font-medium text-[var(--color-accent)] hover:underline"
        onClick={() => navigate("/connection?tab=health")}
        data-testid="slow-transfer-hint-open"
      >
        {tr("netdoctor.hint.open", undefined, "Diagnose")}
      </button>
      <button
        type="button"
        className="rounded p-0.5 text-[var(--color-muted)] hover:text-[var(--color-text)]"
        aria-label={tr("netdoctor.hint.dismiss", undefined, "Dismiss")}
        title={tr("netdoctor.hint.dismiss", undefined, "Dismiss")}
        onClick={() => {
          safeSetItem(DISMISS_KEY, String(Date.now() + DISMISS_FOR_MS));
          setDismissed(true);
        }}
      >
        <X size={12} aria-hidden />
      </button>
    </div>
  );
}
