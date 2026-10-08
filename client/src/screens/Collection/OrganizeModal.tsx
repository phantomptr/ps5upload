import { useEffect, useMemo, useState } from "react";
import { ArrowRight, Undo2 } from "lucide-react";

import {
  collection,
  type OrganizeMove,
  type OrganizePlan,
  type OrganizeResult,
  type OrganizeRun,
} from "../../api/collection";
import { Badge, Button, Checkbox, Modal, Spinner } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { formatCollectionBytes } from "../../lib/collectionView";
import { loadCollection } from "../../state/collection";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";

const moveKey = (m: OrganizeMove) => `${m.container}|${m.from}`;

/** PS Game Library's package organizer: preview every move, untick any, apply, undo later. */
export function OrganizeModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [plan, setPlan] = useState<OrganizePlan | null>(null);
  const [runs, setRuns] = useState<OrganizeRun[]>([]);
  const [unticked, setUnticked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<OrganizeResult | null>(null);

  const refresh = async () => {
    setError(null);
    setPlan(null);
    setUnticked(new Set());
    try {
      const [p, r] = await Promise.all([
        collection.organizePlan(),
        collection.organizeRuns(),
      ]);
      setPlan(p);
      setRuns(r);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  useEffect(() => {
    if (!open) return;
    setResult(null);
    void refresh();
  }, [open]);

  const chosen = useMemo(
    () => (plan?.moves ?? []).filter((m) => !unticked.has(moveKey(m))),
    [plan, unticked],
  );
  const chosenBytes = chosen.reduce((n, m) => n + m.size, 0);
  const notInPlace = (plan?.skipped ?? []).filter(
    (s) => s.reason !== "already correct",
  );
  const inPlace = (plan?.skipped.length ?? 0) - notInPlace.length;

  const apply = async () => {
    if (!plan || chosen.length === 0) return;
    setBusy(true);
    try {
      const r = await collection.organizeApply(
        plan.token,
        chosen.map((m) => ({ container: m.container, from: m.from, to: m.to })),
      );
      setResult(r);
      pushNotification(
        r.failed.length > 0 ? "error" : "info",
        tr(
          "collection.org_done",
          { moved: r.moved.length, failed: r.failed.length },
          "Organized: {moved} moved, {failed} failed",
        ),
      );
      await loadCollection();
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const undo = async (run: OrganizeRun) => {
    const ok = await confirm({
      title: tr("collection.org_undo_title", undefined, "Undo this run?"),
      message: tr(
        "collection.org_undo_body",
        { n: run.moved },
        "The {n} packages it moved go back to their old names and folders. A package moved or renamed since then is left where it is.",
      ),
      confirmLabel: tr("collection.org_undo", undefined, "Undo"),
    });
    if (!ok) return;
    setBusy(true);
    try {
      const r = await collection.organizeRevert(run.id);
      pushNotification(
        "info",
        tr(
          "collection.org_undone",
          { n: r.reverted.length, problems: r.problems.length },
          "Undone: {n} restored, {problems} left where they are",
        ),
      );
      await loadCollection();
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const toggle = (m: OrganizeMove, on: boolean) =>
    setUnticked((prev) => {
      const next = new Set(prev);
      if (on) next.delete(moveKey(m));
      else next.add(moveKey(m));
      return next;
    });

  return (
    <Modal
      open={open}
      onClose={onClose}
      size="xl"
      title={tr("collection.org_title", undefined, "Organize packages")}
      bodyClassName="p-4 sm:p-5"
    >
      <p className="text-sm text-[var(--color-muted)]">
        {tr(
          "collection.org_body",
          undefined,
          "Renames every package “Title - Game ID - vVersion - Region - KIND” and files it under Platform › Game ID - Title, inside the folder it is already in. Nothing is overwritten or deleted, files are only renamed (never copied), and every run can be undone.",
        )}
      </p>
      {error && <p className="mt-3 text-sm text-[var(--color-bad)]">{error}</p>}
      {!plan && !error && (
        <div className="mt-4 flex items-center gap-2 text-sm text-[var(--color-muted)]">
          <Spinner size={14} />
          {tr("collection.org_planning", undefined, "Reading the packages…")}
        </div>
      )}
      {result && (
        <div className="mt-3 rounded-lg border border-[var(--color-border)] p-3 text-xs">
          <div className="text-sm text-[var(--color-text)]">
            {tr(
              "collection.org_done",
              { moved: result.moved.length, failed: result.failed.length },
              "Organized: {moved} moved, {failed} failed",
            )}
          </div>
          {result.dropped.length > 0 && (
            <div className="mt-1 text-[var(--color-warn)]">
              {tr(
                "collection.org_dropped",
                { n: result.dropped.length },
                "{n} left alone: they changed since the preview",
              )}
            </div>
          )}
          {result.failed.map((f) => (
            <div
              key={f.from}
              className="mt-1 break-all text-[var(--color-bad)]"
            >
              {f.from}: {f.error}
            </div>
          ))}
        </div>
      )}
      {plan && (
        <>
          <div className="mt-4 flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
            <span className="font-medium text-[var(--color-text)]">
              {plan.moves.length === 0
                ? tr(
                    "collection.org_nothing",
                    undefined,
                    "Every package is already where it belongs.",
                  )
                : tr(
                    "collection.org_summary",
                    {
                      n: plan.moves.length,
                      size: formatCollectionBytes(plan.total_bytes),
                    },
                    "{n} moves · {size}",
                  )}
            </span>
            {plan.unsure_count > 0 && (
              <Badge tone="warn" size="sm">
                {tr(
                  "collection.org_unsure",
                  { n: plan.unsure_count },
                  "{n} uncertain",
                )}
              </Badge>
            )}
            {inPlace > 0 && (
              <span className="text-xs text-[var(--color-muted)]">
                {tr(
                  "collection.org_in_place",
                  { n: inPlace },
                  "{n} already in place",
                )}
              </span>
            )}
          </div>
          {plan.moves.length > 0 && (
            <>
              <div className="mt-2 flex gap-2 text-xs">
                <button
                  type="button"
                  className="text-[var(--color-accent)] hover:underline"
                  onClick={() => setUnticked(new Set())}
                >
                  {tr("collection.org_all", undefined, "Select all")}
                </button>
                <button
                  type="button"
                  className="text-[var(--color-accent)] hover:underline"
                  onClick={() =>
                    setUnticked(new Set(plan.moves.map((m) => moveKey(m))))
                  }
                >
                  {tr("collection.org_none", undefined, "Select none")}
                </button>
              </div>
              <ul className="mt-2 flex max-h-[45vh] flex-col gap-1.5 overflow-y-auto pr-1">
                {plan.moves.map((m) => (
                  <li
                    key={moveKey(m)}
                    className="rounded-lg border border-[var(--color-border)] p-2 text-xs"
                  >
                    <div className="flex items-start gap-2">
                      <Checkbox
                        checked={!unticked.has(moveKey(m))}
                        onChange={(on) => toggle(m, on)}
                        label={
                          <span className="sr-only">
                            {tr("collection.org_pick", undefined, "Include")}
                          </span>
                        }
                      />
                      <div className="min-w-0 flex-1">
                        <div className="break-all font-mono text-[0.6875rem] text-[var(--color-muted)]">
                          {m.from}
                        </div>
                        <div className="mt-0.5 flex items-start gap-1 break-all font-mono text-[0.6875rem] text-[var(--color-text)]">
                          <ArrowRight
                            size={11}
                            className="mt-0.5 shrink-0 text-[var(--color-accent)]"
                          />
                          <span>{m.to}</span>
                        </div>
                        <div className="mt-1 flex flex-wrap gap-1">
                          {m.duplicate && (
                            <Badge
                              tone="warn"
                              size="sm"
                              title={tr(
                                "collection.org_dup_hint",
                                { of: m.duplicate_of ?? "" },
                                "The same release as {of}: filed beside it and marked DUPLICATE. Nothing is deleted.",
                              )}
                            >
                              {tr(
                                "collection.duplicate",
                                undefined,
                                "Duplicate",
                              )}
                            </Badge>
                          )}
                          {!m.confident && (
                            <Badge tone="warn" size="sm" title={m.reason}>
                              {tr(
                                "collection.org_unsure_one",
                                undefined,
                                "Kind inferred",
                              )}
                            </Badge>
                          )}
                          {m.part && (
                            <Badge tone="neutral" size="sm">
                              {tr(
                                "collection.org_part",
                                { part: m.part },
                                "Part {part}",
                              )}
                            </Badge>
                          )}
                        </div>
                      </div>
                    </div>
                  </li>
                ))}
              </ul>
            </>
          )}
          {notInPlace.length > 0 && (
            <details className="mt-3 text-xs">
              <summary className="cursor-pointer text-[var(--color-muted)]">
                {tr(
                  "collection.org_left_alone",
                  { n: notInPlace.length },
                  "Left alone ({n})",
                )}
              </summary>
              <ul className="mt-1 flex flex-col gap-1">
                {notInPlace.map((s) => (
                  <li key={`${s.container}|${s.path}`} className="break-all">
                    <span className="text-[var(--color-warn)]">{s.reason}</span>
                    {": "}
                    <span className="font-mono">{s.path}</span>
                    {s.detail ? ` (${s.detail})` : ""}
                  </li>
                ))}
              </ul>
            </details>
          )}
          {plan.moves.length > 0 && (
            <div className="mt-4 flex flex-wrap items-center justify-end gap-2">
              <span className="text-xs text-[var(--color-muted)]">
                {tr(
                  "collection.org_chosen",
                  {
                    n: chosen.length,
                    size: formatCollectionBytes(chosenBytes),
                  },
                  "{n} chosen · {size}",
                )}
              </span>
              <Button
                variant="primary"
                size="sm"
                loading={busy}
                disabled={chosen.length === 0}
                onClick={() => void apply()}
              >
                {tr(
                  "collection.org_go",
                  { n: chosen.length },
                  "Organize {n} packages",
                )}
              </Button>
            </div>
          )}
        </>
      )}
      {runs.length > 0 && (
        <>
          <h3 className="mb-2 mt-5 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            {tr("collection.org_runs", undefined, "Earlier runs")}
          </h3>
          <ul className="flex flex-col gap-1.5">
            {runs.slice(0, 10).map((r) => (
              <li
                key={r.id}
                className="flex flex-wrap items-center gap-2 rounded-lg border border-[var(--color-border)] p-2 text-xs"
              >
                <span className="text-[var(--color-text)]">
                  {new Date(r.created_at).toLocaleString()}
                </span>
                <span className="text-[var(--color-muted)]">
                  {tr("collection.org_run_moved", { n: r.moved }, "{n} moved")}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-[0.6875rem] text-[var(--color-muted)]">
                  {r.container_root}
                </span>
                {r.reverted_at ? (
                  <span className="text-[var(--color-muted)]">
                    {tr("collection.org_run_undone", undefined, "Undone")}
                  </span>
                ) : (
                  <Button
                    size="sm"
                    variant="secondary"
                    leftIcon={<Undo2 size={12} />}
                    disabled={busy || r.moved === 0}
                    onClick={() => void undo(r)}
                  >
                    {tr("collection.org_undo", undefined, "Undo")}
                  </Button>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
      {dialog}
    </Modal>
  );
}
