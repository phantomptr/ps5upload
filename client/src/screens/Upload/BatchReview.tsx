// Upload with several sources: the review list. Each row shows what it is, where it will land
// and what (if anything) is wrong; shared settings sit below; nothing is queued until Add.

import { useState } from "react";
import { AlertTriangle, CheckCircle2, Loader2, X, XCircle } from "lucide-react";

import { Button, Toggle } from "../../components";
import { formatBytes } from "../../lib/format";
import type { BatchCheck } from "../../lib/uploadBatch";
import type { BatchRow } from "../../state/uploadBatch";
import { useTr } from "../../state/lang";

export interface BatchReviewProps {
  rows: BatchRow[];
  check: BatchCheck;
  /** Where a ready row lands; null until inspected. */
  destFor: (row: BatchRow) => string | null;
  sizeFor: (row: BatchRow) => number | null;
  kindLabel: (row: BatchRow) => string;
  /** The destination picker (the single-source one). */
  destination: React.ReactNode;
  strategy: "overwrite" | "resume";
  onStrategy: (s: "overwrite" | "resume") => void;
  /** Kind-specific options, shown only when the list holds that kind. */
  options: React.ReactNode;
  /** Rows that will be added. */
  addCount: number;
  canAdd: boolean;
  onAdd: (startNow: boolean) => void;
  onRemove: (id: string) => void;
  onInclude: (id: string, include: boolean) => void;
  onPassword: (id: string, password: string) => void;
  onClear: () => void;
}

function PasswordField({ onSubmit }: { onSubmit: (pw: string) => void }) {
  const tr = useTr();
  const [pw, setPw] = useState("");
  return (
    <form
      className="flex items-center gap-1"
      onSubmit={(e) => {
        e.preventDefault();
        if (pw) onSubmit(pw);
      }}
    >
      <input
        type="password"
        autoComplete="off"
        value={pw}
        onChange={(e) => setPw(e.target.value)}
        placeholder={tr("fpkg.archivePassword", undefined, "Archive password")}
        className="input input-sm w-36! text-xs"
      />
      <Button size="sm" type="submit" disabled={!pw}>
        {tr("batch_unlock", undefined, "Unlock")}
      </Button>
    </form>
  );
}

export function BatchReview(p: BatchReviewProps) {
  const tr = useTr();
  const inspecting = p.rows.filter((r) => r.status === "inspecting").length;
  return (
    <section className="mb-6 grid gap-4">
      <div className="rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)]">
        <header className="flex items-center justify-between gap-2 border-b border-[var(--color-border)] px-4 py-2.5">
          <div className="text-sm font-semibold">
            {tr("batch_title", { count: p.rows.length }, "{count} sources")}
            {inspecting > 0 && (
              <span className="ml-2 font-normal text-[var(--color-muted)]">
                {tr("batch_inspecting", { count: inspecting }, "checking {count}…")}
              </span>
            )}
          </div>
          <Button size="sm" variant="ghost" onClick={p.onClear}>
            {tr("batch_clear", undefined, "Clear list")}
          </Button>
        </header>
        <ul className="divide-y divide-[var(--color-border)]">
          {p.rows.map((r) => {
            const issues = p.check.issues.get(r.id) ?? [];
            const skipped = p.check.skip.has(r.id);
            const dest = p.destFor(r);
            const size = p.sizeFor(r);
            const name = r.path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? r.path;
            return (
              <li key={r.id} className="flex items-start gap-3 px-4 py-2.5 text-sm">
                <input
                  type="checkbox"
                  className="mt-1"
                  checked={r.include}
                  disabled={r.status === "error"}
                  onChange={(e) => p.onInclude(r.id, e.target.checked)}
                  aria-label={tr("batch_include", { name }, "Include {name}")}
                />
                <span className="mt-0.5 shrink-0">
                  {r.status === "inspecting" ? (
                    <Loader2 size={14} className="animate-spin text-[var(--color-muted)]" />
                  ) : r.status === "error" ? (
                    <XCircle size={14} className="text-[var(--color-bad)]" />
                  ) : issues.length || r.status === "needs-password" ? (
                    <AlertTriangle size={14} className="text-[var(--color-warn)]" />
                  ) : (
                    <CheckCircle2 size={14} className="text-[var(--color-good)]" />
                  )}
                </span>
                <div className={`min-w-0 flex-1 ${r.include ? "" : "opacity-60"}`}>
                  <div className="flex flex-wrap items-baseline gap-x-2">
                    <span className="truncate font-medium" title={r.path}>
                      {name}
                    </span>
                    <span className="text-xs text-[var(--color-muted)]">{p.kindLabel(r)}</span>
                    {size != null && (
                      <span className="text-xs tabular-nums text-[var(--color-muted)]">{formatBytes(size)}</span>
                    )}
                  </div>
                  {dest && r.include && (
                    <div className="truncate font-mono text-xs text-[var(--color-muted)]" title={dest}>
                      → {dest}
                    </div>
                  )}
                  {r.error && <div className="text-xs text-[var(--color-bad)]">{r.error}</div>}
                  {skipped && (
                    <div className="text-xs text-[var(--color-muted)]">
                      {tr("batch_already_queued", undefined, "Already in the queue — skipped")}
                    </div>
                  )}
                  {issues.map((i) => (
                    <div key={i.text} className="text-xs text-[var(--color-warn)]">
                      {tr(i.key, i.vars, i.text)}
                    </div>
                  ))}
                  {r.status === "needs-password" && (
                    <div className="mt-1">
                      <PasswordField onSubmit={(pw) => p.onPassword(r.id, pw)} />
                    </div>
                  )}
                </div>
                <button
                  type="button"
                  onClick={() => p.onRemove(r.id)}
                  aria-label={tr("queue_remove", undefined, "Remove from queue")}
                  className="rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
                >
                  <X size={14} />
                </button>
              </li>
            );
          })}
        </ul>
      </div>

      {p.destination}

      <div className="rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] grid gap-3 p-4">
        <Toggle
          checked={p.strategy === "resume"}
          onChange={(on) => p.onStrategy(on ? "resume" : "overwrite")}
          label={tr("batch_resume", undefined, "Keep what's already on the PS5 (resume)")}
          hint={tr(
            "batch_resume_hint",
            undefined,
            "Off: every destination that already exists is replaced.",
          )}
        />
        {p.options}
      </div>

      {p.check.space.length > 0 && (
        <div className="rounded-[var(--radius-field)] border border-[color-mix(in_srgb,var(--color-warn)_40%,transparent)] bg-[var(--color-warn)]/10 px-3 py-2 text-sm text-[var(--color-warn)]">
          {p.check.space.map((s) => (
            <div key={s.text}>{tr(s.key, s.vars, s.text)}</div>
          ))}
        </div>
      )}

      <div className="flex flex-wrap gap-2">
        <Button variant="primary" disabled={!p.canAdd} onClick={() => p.onAdd(true)}>
          {tr("batch_upload_now", { count: p.addCount }, "Upload {count} now")}
        </Button>
        <Button disabled={!p.canAdd} onClick={() => p.onAdd(false)}>
          {tr("batch_add", { count: p.addCount }, "Add {count} to queue")}
        </Button>
      </div>
    </section>
  );
}
