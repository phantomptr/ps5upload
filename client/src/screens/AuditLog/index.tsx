import { useMemo } from "react";
import { ScrollText } from "lucide-react";

import { PageHeader } from "../../components";
import { useTr } from "../../state/lang";
import { useAuditLogStore, type AuditEntry } from "../../state/auditLog";

/**
 * Top-level Audit log screen (promoted from a Settings card in 2.12.0).
 *
 * The audit log records every destructive or otherwise-irreversible
 * action the user has taken: deletes (Library, FileSystem, Saves),
 * unmounts, reboots/shutdowns, app unregisters, mount-replacements,
 * etc. It's a permanent local safety record — there's deliberately
 * no "Clear" button.
 *
 * Why promoted: the conceptual-model audit flagged Settings as a
 * "junk drawer" mixing real preferences with one-off operations and
 * read-only views. The audit log is in the latter category —
 * conceptually adjacent to Activity / Logs (the other Diagnostics
 * tabs) rather than to language / theme / keep-awake / etc. Burying
 * it under "Settings" meant users who wanted to answer "what did
 * I delete last week?" had to find a Settings card by scrolling.
 *
 * The screen is a thin wrapper: it composes the same read-only
 * table the old Settings card used (no logic moved or changed),
 * just rehoused under its own route + sidebar entry.
 */
export default function AuditLogScreen() {
  const tr = useTr();
  const rawEntries = useAuditLogStore((s) => s.entries);
  const entries: AuditEntry[] = useMemo(
    () => [...rawEntries].reverse(),
    [rawEntries],
  );

  return (
    <div className="p-6">
      <PageHeader
        icon={ScrollText}
        title={tr("audit_log_title", undefined, "Audit log")}
        description={tr(
          "audit_log_description",
          undefined,
          "Permanent local record of destructive and irreversible actions you've taken. No clear button — that's deliberate. Last 100 entries shown; older entries roll off the ring buffer.",
        )}
      />
      {entries.length === 0 ? (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "audit_empty",
            undefined,
            "No destructive actions recorded yet. Reboots, deletes, unregisters, and similar will appear here as you perform them.",
          )}
        </p>
      ) : (
        <div className="max-h-[calc(100vh-12rem)] overflow-y-auto rounded-md border border-[var(--color-border)] bg-[var(--color-surface)]">
          <div className="overflow-x-auto">
          <table className="w-full min-w-[480px] text-xs">
            <thead className="sticky top-0 bg-[var(--color-surface-2)] text-xs uppercase tracking-wide text-[var(--color-muted)]">
              <tr>
                <th className="px-2 py-1 text-left">
                  {tr("audit_when", undefined, "When")}
                </th>
                <th className="px-2 py-1 text-left">
                  {tr("audit_kind", undefined, "Action")}
                </th>
                <th className="px-2 py-1 text-left">
                  {tr("audit_what", undefined, "Detail")}
                </th>
                <th className="px-2 py-1 text-left">
                  {tr("audit_context", undefined, "Context")}
                </th>
              </tr>
            </thead>
            <tbody>
              {entries.slice(0, 100).map((e) => (
                <tr
                  key={e.id}
                  className={`border-t border-[var(--color-border)] ${
                    e.failed ? "text-[var(--color-bad)]" : ""
                  }`}
                >
                  <td className="px-2 py-1 tabular-nums text-xs">
                    {new Date(e.ts).toLocaleString()}
                  </td>
                  <td className="px-2 py-1 font-mono text-xs">{e.kind}</td>
                  <td className="px-2 py-1">{e.what}</td>
                  <td className="px-2 py-1 truncate text-xs text-[var(--color-muted)]">
                    {e.context ?? ""}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          </div>
        </div>
      )}
    </div>
  );
}
