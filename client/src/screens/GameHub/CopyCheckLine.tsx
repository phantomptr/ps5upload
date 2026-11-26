import { X } from "lucide-react";

import type { CopyCheck } from "../../api/collection";
import { formatCollectionBytes } from "../../lib/collectionView";
import { copyVerdict } from "../../lib/copyCheckVerdict";
import { formatNumber } from "../../lib/formatDate";
import {
  cancelCopyChecks,
  checkPercent,
  useCopyCheck,
} from "../../state/copyChecks";
import { useTr, type Translator } from "../../state/lang";

/** A copy's check, under its row: progress while it runs, then what was found. */
export function CopyCheckLine({ path }: { path: string }) {
  const tr = useTr();
  const c = useCopyCheck(path);
  if (!c) return null;
  const how =
    c.method === "package"
      ? tr(
          "copycheck.how_package",
          undefined,
          "verified against its own digests",
        )
      : c.method === "archive"
        ? tr(
            "copycheck.how_archive",
            undefined,
            "every file's checksum checked",
          )
        : c.method === "files"
          ? tr("copycheck.how_files", undefined, "every file read back")
          : tr(
              "copycheck.how_bytes",
              undefined,
              "read start to end (finds a copy cut short or unreadable, not one with wrong bytes)",
            );
  if (c.state === "queued" || c.state === "running") {
    const pct = c.state === "running" ? checkPercent(c) : null;
    return (
      <div
        className="mt-1 flex items-center gap-2 text-[0.6875rem] text-[var(--color-muted)]"
        data-testid="copy-check"
      >
        <span>
          {c.state === "queued"
            ? tr("copycheck.queued", undefined, "Waiting to be checked")
            : pct === null
              ? tr("copycheck.running", undefined, "Checking…")
              : runningText(tr, c, pct)}
        </span>
        <button
          type="button"
          className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 hover:bg-[var(--color-surface-raised)]"
          onClick={() => void cancelCopyChecks(path)}
        >
          <X size={11} aria-hidden />
          {tr("cancel", undefined, "Cancel")}
        </button>
      </div>
    );
  }
  if (c.state === "ok") {
    return (
      <div
        className="mt-1 text-[0.6875rem] text-[var(--color-good)]"
        data-testid="copy-check"
      >
        {tr("copycheck.ok", { how }, "Checked, no problems: {how}.")}
      </div>
    );
  }
  if (c.state === "cancelled") {
    return (
      <div
        className="mt-1 text-[0.6875rem] text-[var(--color-muted)]"
        data-testid="copy-check"
      >
        {tr("copycheck.cancelled", undefined, "Check stopped.")}
      </div>
    );
  }
  if (c.state === "failed") {
    return (
      <div
        className="mt-1 text-[0.6875rem] text-[var(--color-warn)]"
        data-testid="copy-check"
      >
        {tr(
          "copycheck.failed",
          { why: c.error ?? "" },
          "Could not be checked: {why}",
        )}
      </div>
    );
  }
  return <CopyProblems check={c} how={how} />;
}

/** A running check's line: how far through the whole check it is and, for a package, which of
 *  its two reads it is on (#951: its blocks, then the PlayGo chunk CRCs, a second full read). */
export function runningText(
  tr: Translator,
  c: Pick<CopyCheck, "bytes_done" | "bytes_total" | "phase">,
  pct: number,
): string {
  const vars = { pct, done: formatCollectionBytes(c.bytes_done), total: formatCollectionBytes(c.bytes_total) };
  if (c.phase === "blocks")
    return tr("copycheck.running_blocks_pct", vars, "Checking the data blocks: {pct}% ({done} of {total})");
  if (c.phase === "playgo")
    return tr("copycheck.running_playgo_pct", vars, "Checking the PlayGo chunks: {pct}% ({done} of {total})");
  return tr("copycheck.running_pct", vars, "Checking {pct}% ({done} of {total})");
}

/** A check that found problems: what is wrong in plain words, what to do about it, and the
 *  check's own lines folded away for a bug report. */
export function CopyProblems({ check: c, how }: { check: CopyCheck; how: string }) {
  const tr = useTr();
  const v = copyVerdict(c);
  const again = tr(
    "copycheck.fix_again",
    undefined,
    "This usually means a copy or download was interrupted or went wrong. Copy or download it again.",
  );
  const [verdict, advice] =
    v.kind === "data"
      ? [
          tr(
            "copycheck.v_data",
            {
              bad: formatNumber(v.bad),
              total: formatNumber(v.total),
              pct: v.percent,
              size: formatCollectionBytes(v.bytes),
            },
            "This file's data is damaged: {bad} of {total} blocks don't match ({pct}%, about {size}).",
          ),
          again,
        ]
      : v.kind === "truncated"
        ? [
            v.need > 0
              ? tr(
                  "copycheck.v_truncated",
                  { have: formatCollectionBytes(v.have), need: formatCollectionBytes(v.need) },
                  "This file is cut short: it is {have} and should be at least {need}.",
                )
              : tr("copycheck.v_truncated_end", undefined, "This file is cut short: its end is missing."),
            tr(
              "copycheck.fix_truncated",
              undefined,
              "The copy or download stopped before the end. Copy or download it again.",
            ),
          ]
        : v.kind === "structure"
          ? [
              tr(
                "copycheck.v_structure",
                { count: formatNumber(v.count) },
                "This file's own tables are damaged, so the game can't be read out of it. Checks that failed: {count}.",
              ),
              again,
            ]
          : v.kind === "metadata"
            ? [
                tr(
                  "copycheck.v_metadata",
                  { count: formatNumber(v.count) },
                  "The game's data is intact, but the package's own header information doesn't match. Checks that failed: {count}.",
                ),
                tr(
                  "copycheck.fix_metadata",
                  undefined,
                  "This can happen when a package was edited after it was made. It may still install. If the PS5 refuses it, copy or download it again.",
                ),
              ]
            : [
                tr("copycheck.v_other", { count: formatNumber(v.count) }, "Problems found: {count}."),
                tr(
                  "copycheck.fix_other",
                  undefined,
                  "This copy may not install or play. Copy or download it again.",
                ),
              ];
  // A header-only mismatch is a caution, not a broken file.
  const tone = v.kind === "metadata" ? "text-[var(--color-warn)]" : "text-[var(--color-bad)]";
  return (
    <div className={`mt-1 text-[0.6875rem] ${tone}`} data-testid="copy-check" data-verdict={v.kind}>
      <div className="font-medium" data-testid="copy-check-verdict">
        {verdict}
      </div>
      <div data-testid="copy-check-advice">{advice}</div>
      <details className="mt-0.5 text-[var(--color-muted)]" data-testid="copy-check-details">
        <summary className="cursor-pointer">{tr("copycheck.details", undefined, "Details")}</summary>
        <div className="mt-0.5">{tr("copycheck.details_how", { how }, "How it was checked: {how}.")}</div>
        <ul className="mt-0.5 list-disc ps-4 font-mono">
          {c.problems.slice(0, 5).map((p, i) => (
            <li key={i} className="break-all">
              {p.file ? `${p.file}: ` : ""}
              {p.error}
            </li>
          ))}
          {c.problems_total > 5 && (
            <li>
              {tr("copycheck.more", { n: c.problems_total - 5 }, "and {n} more")}
            </li>
          )}
        </ul>
      </details>
    </div>
  );
}
