import { useEffect, useState } from "react";
import {
  GitCompare,
  Loader2,
  ChevronDown,
  ChevronRight,
  AlertTriangle,
} from "lucide-react";
import { dirDiffPreview, type DirDiffPreview } from "../../api/ps5";
import { useTr } from "../../state/lang";
import { formatBytes } from "../../lib/format";
import { useUploadQueueStore } from "../../state/uploadQueue";
import { useTransferStore, phaseForHost } from "../../state/transfer";
import { hostOf } from "../../lib/addr";

/**
 * Pre-flight folder diff preview.
 *
 * Calls the engine's `transfer_dir_diff_preview` (which runs
 * reconcile() in Fast mode without uploading), shows a one-line
 * summary + an optional details expander.
 *
 * Auto-runs whenever src/dest/excludes change, debounced 500 ms so
 * the user typing in the destination field doesn't fire a probe per
 * keystroke. Failures are non-fatal — the panel just hides itself
 * and the user can still upload normally.
 */
export default function FolderDiffPanel({
  srcDir,
  destRoot,
  transferAddr,
  excludes,
}: {
  srcDir: string;
  destRoot: string;
  transferAddr: string;
  excludes: string[];
}) {
  const tr = useTr();
  const [data, setData] = useState<DirDiffPreview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);

  // Mirror transferScreenBusy() reactively so the panel re-renders when a
  // transfer starts/stops. A diff-preview does a full local walk of the
  // source; on a slow network share (SMB/UNC) that's minutes long, and
  // running it WHILE an upload is in flight makes the two walks fight over
  // the same share + mgmt port — which took the payload down on a large
  // game served from a network drive. So we pause the preview during any
  // active transfer (one-shot or queue) and resume once it settles. The
  // engine also serializes reconciles, but not previewing at all here avoids
  // the wasted multi-minute walk in the first place.
  // Gate on THIS preview's target console only — a transfer to another console
  // must not suppress this one's diff preview (consoles run in parallel). Both
  // the queue-running flag and the one-shot phase are read per-host.
  const targetHost = transferAddr ? hostOf(transferAddr) : null;
  const queueRunning = useUploadQueueStore((s) =>
    targetHost ? !!s.runningHosts[targetHost] : false,
  );
  const phaseKind = useTransferStore((s) => phaseForHost(s, targetHost).kind);
  const transferBusy =
    queueRunning || phaseKind === "starting" || phaseKind === "running";

  useEffect(() => {
    let cancelled = false;
    setData(null);
    setError(null);
    if (!srcDir || !destRoot || !transferAddr) return;
    // Don't walk the source while a transfer holds it; resume when busy clears
    // (transferBusy is a dep, so this effect re-runs and previews then).
    if (transferBusy) return;
    const id = window.setTimeout(async () => {
      if (cancelled) return;
      setLoading(true);
      try {
        const r = await dirDiffPreview(srcDir, destRoot, transferAddr, excludes);
        if (!cancelled) {
          setData(r);
        }
      } catch (e) {
        if (!cancelled) {
          // Don't surface "destination doesn't exist yet" as an error —
          // that's the most common case and the upload itself handles
          // it. Only show real RPC errors.
          const msg = e instanceof Error ? e.message : String(e);
          if (msg.includes("dest_root") && msg.includes("does not exist")) {
            setData({
              to_send_count: 0,
              to_send_bytes: 0,
              already_present_count: 0,
              already_present_bytes: 0,
              sample_to_send: [],
            });
          } else {
            setError(msg);
          }
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    }, 500);
    return () => {
      cancelled = true;
      window.clearTimeout(id);
    };
  }, [srcDir, destRoot, transferAddr, excludes, transferBusy]);

  // While a transfer is active, surface a quiet "paused" note (only once the
  // user has picked a source/dest, so we don't show it on an empty form).
  if (transferBusy && srcDir && destRoot && transferAddr) {
    return (
      <section className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-xs">
        <header className="flex items-center gap-2 text-[var(--color-muted)]">
          <GitCompare size={12} />
          <span>
            {tr(
              "folder_diff_paused",
              undefined,
              "Diff preview paused while a transfer is running.",
            )}
          </span>
        </header>
      </section>
    );
  }

  if (!data && !loading && !error) return null;

  return (
    <section className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-xs">
      <header className="flex items-center gap-2">
        <GitCompare size={12} />
        <h4 className="flex-1 text-xs font-semibold">
          {tr("folder_diff_title", undefined, "Diff vs PS5")}
        </h4>
        {loading && (
          <Loader2 size={11} className="animate-spin text-[var(--color-muted)]" />
        )}
        {data && data.sample_to_send.length > 0 && (
          <button
            type="button"
            onClick={() => setExpanded((v) => !v)}
            className="text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
          >
            {expanded ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
          </button>
        )}
      </header>
      {error && (
        <div className="mt-2 flex items-start gap-1 text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          <span>{error}</span>
        </div>
      )}
      {data && (
        <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-[var(--color-muted)]">
          <span>
            <span className="font-semibold text-[var(--color-accent)]">
              {data.to_send_count}
            </span>{" "}
            {tr("folder_diff_to_send", undefined, "to send")} (
            {formatBytes(data.to_send_bytes)})
          </span>
          {data.already_present_count > 0 && (
            <span>
              <span className="font-semibold text-[var(--color-good)]">
                {data.already_present_count}
              </span>{" "}
              {tr(
                "folder_diff_already_present",
                undefined,
                "already on PS5",
              )}{" "}
              ({formatBytes(data.already_present_bytes)})
            </span>
          )}
          {data.to_send_count === 0 && data.already_present_count > 0 && (
            <span className="text-[var(--color-good)]">
              {tr(
                "folder_diff_no_changes",
                undefined,
                "Nothing to upload — already in sync.",
              )}
            </span>
          )}
        </div>
      )}
      {expanded && data && data.sample_to_send.length > 0 && (
        <div className="mt-2 max-h-40 overflow-auto rounded border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
          <div className="mb-1 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            {tr(
              "folder_diff_sample",
              { count: data.sample_to_send.length, total: data.to_send_count },
              `Sample of files to send (${data.sample_to_send.length} of ${data.to_send_count})`,
            )}
          </div>
          <ul className="space-y-0.5 font-mono text-xs">
            {data.sample_to_send.map((p) => (
              <li key={p} className="break-all">
                {p}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

// formatBytes moved to lib/format.ts (and corrected to IEC binary).
