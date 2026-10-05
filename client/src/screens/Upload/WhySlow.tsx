import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";

import { useTr } from "../../state/lang";
import { formatBytes, formatDuration } from "../../lib/format";
import { fetchJobSummary, whyEntry, type JobSummary } from "../../lib/jobSummary";

type Loaded = { state: "loading" } | { state: "none" } | { state: "ready"; summary: JobSummary };

/** "Why was this slow?" for a finished transfer (review 009 #4): the engine's one-sentence
 *  reading of where the job's time went, with the numbers behind it. The summary lives on this
 *  computer only and holds no address or path. Closed until opened, and it fetches only then,
 *  so a long queue of finished rows costs nothing. */
export function WhySlowPanel({ jobId }: { jobId: string | undefined }) {
  const tr = useTr();
  const [open, setOpen] = useState(false);
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });

  useEffect(() => {
    if (!open || !jobId) return;
    let live = true;
    setLoaded({ state: "loading" });
    void fetchJobSummary(jobId).then((summary) => {
      if (live) setLoaded(summary ? { state: "ready", summary } : { state: "none" });
    });
    return () => {
      live = false;
    };
  }, [open, jobId]);

  if (!jobId) return null;
  const Chevron = open ? ChevronDown : ChevronRight;
  return (
    <div className="mt-2 text-xs" data-testid="why-slow">
      <button
        type="button"
        className="inline-flex items-center gap-1 text-[var(--color-muted)] hover:text-[var(--color-text)]"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        data-testid="why-slow-toggle"
      >
        <Chevron size={12} aria-hidden />
        {tr("job_why_title", undefined, "Why was this slow?")}
      </button>
      {open && (
        <div
          className="mt-1 space-y-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2"
          data-testid="why-slow-body"
        >
          {loaded.state === "loading" && (
            <div className="text-[var(--color-muted)]">{tr("job_why_loading", undefined, "Loading…")}</div>
          )}
          {loaded.state === "none" && (
            <div className="text-[var(--color-muted)]" data-testid="why-slow-none">
              {tr("job_why_none", undefined, "No summary was recorded for this job.")}
            </div>
          )}
          {loaded.state === "ready" && <WhySlowBody s={loaded.summary} />}
        </div>
      )}
    </div>
  );
}

export function WhySlowBody({ s }: { s: JobSummary }) {
  const tr = useTr();
  const why = whyEntry(s);
  const sh = s.shares;
  const pct = (n: number | undefined) => `${Math.round(n ?? 0)} %`;
  return (
    <>
      <div className="text-[var(--color-text)]" data-testid="why-slow-text">
        {tr(why.key, { pct: why.pct }, why.text.replace("{pct}", String(why.pct)))}
      </div>
      {sh && (sh.ticks ?? 0) > 0 && (
        <div className="text-[var(--color-muted)]" data-testid="why-slow-shares">
          {tr(
            "job_why_shares",
            {
              console: pct(sh.receiver_bound_pct),
              source: pct(sh.source_starved_pct),
              credit: pct(sh.credit_starved_pct),
            },
            `Time held back by: the console ${pct(sh.receiver_bound_pct)}, the source ${pct(sh.source_starved_pct)}, a full receive window ${pct(sh.credit_starved_pct)}`,
          )}
        </div>
      )}
      <div className="text-[var(--color-muted)]">
        {s.lanes_avg
          ? tr(
              "job_why_lanes",
              { avg: s.lanes_avg, max: s.lanes_max ?? 0, chunk: formatBytes((s.chunk_avg_kib ?? 0) * 1024) },
              `Connections: ${s.lanes_avg} on average, ${s.lanes_max ?? 0} at most; chunk ${formatBytes((s.chunk_avg_kib ?? 0) * 1024)}`,
            )
          : null}
      </div>
      {s.resumed && (
        <div className="text-[var(--color-muted)]">
          {tr("job_why_resumed", undefined, "The transfer reconnected and resumed.")}
        </div>
      )}
      {s.slow_drive_switch && (
        <div className="text-[var(--color-muted)]">
          {tr("job_why_slow_drive", undefined, "The transfer switched to sequential writes for a slow drive.")}
        </div>
      )}
      {(s.settle_ms ?? 0) > 1000 && (
        <div className="text-[var(--color-muted)]">
          {tr(
            "job_why_settle",
            { time: formatDuration((s.settle_ms ?? 0) / 1000) },
            `Finishing on the console took ${formatDuration((s.settle_ms ?? 0) / 1000)}.`,
          )}
        </div>
      )}
      {s.console_line && (
        <div className="break-words font-mono text-[var(--color-muted)]" data-testid="why-slow-console-line">
          {s.console_line}
        </div>
      )}
      <div className="text-[var(--color-muted)]">
        {tr(
          "job_why_local_note",
          undefined,
          "Saved on this computer only. It holds no address or file path, and nothing is sent anywhere.",
        )}
      </div>
    </>
  );
}
