import { useMemo } from "react";
import { ShieldCheck } from "lucide-react";

import { PageHeader } from "../../components";
import { useTr } from "../../state/lang";
import { useAuditLogStore, type AuditEntry } from "../../state/auditLog";

/**
 * Top-level Audit log screen (promoted from a Settings card in 2.12.0).
 *
 * The audit log records power actions (wake, rest, restart, shut down),
 * deletes from Files and the game library, unregisters, uninstalls and
 * installs. It lives in localStorage as a 256-entry ring; there's
 * deliberately no "Clear" button.
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
    <div className="app-page">
      <PageHeader
        icon={ShieldCheck}
        title={tr("audit_log_title", undefined, "Audit log")}
        description={tr(
          "audit_log_description_v3",
          undefined,
          "A record, kept in this app on this computer, of power actions, deletes, unregisters, uninstalls and installs. The newest 256 are kept and older ones roll off; there is no clear button, but clearing the app's data or browser storage erases it. The last 100 are shown.",
        )}
      />
      {entries.length === 0 ? (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "audit_empty_v2",
            undefined,
            "Nothing recorded yet. Power actions (wake, rest mode, restart, shut down), deletes, unregisters, uninstalls and installs appear here as you do them.",
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
