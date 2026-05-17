import { useSearchParams } from "react-router-dom";
import { ScrollText, Terminal } from "lucide-react";

import { PageHeader } from "../../components";
import { useTr } from "../../state/lang";
import AppLogsPanel from "./AppLogsPanel";
import KernelLogPanel from "./KernelLogPanel";

/**
 * Logs shell — hosts two URL-routed tabs:
 *
 *   - **app**: in-memory React/Tauri events (errors, warnings, lifecycle
 *     traces). Persists for the session, never network-backed.
 *   - **kernel**: live /dev/klog stream from the payload. Polls every
 *     second while playing; dies when the payload dies.
 *
 * Pre-2.12.0 these were two separate sidebar entries (/logs and
 * /kernel-log). Merged into one /logs entry with `?tab=kernel` for
 * the kernel-log view to reduce sidebar noise and group the
 * conceptually-related "thing happens, line appears" affordances.
 *
 * Tab state lives in the URL (`useSearchParams`) so deep links and
 * page refresh preserve the active tab.
 */

type TabId = "app" | "kernel";

const TABS: Array<{ id: TabId; icon: typeof ScrollText; key: string; fallback: string }> = [
  { id: "app", icon: ScrollText, key: "logs_tab_app", fallback: "App" },
  { id: "kernel", icon: Terminal, key: "logs_tab_kernel", fallback: "Kernel" },
];

export default function LogsScreen() {
  const tr = useTr();
  const [searchParams, setSearchParams] = useSearchParams();

  const tabParam = searchParams.get("tab");
  const activeTab: TabId = tabParam === "kernel" ? "kernel" : "app";
  const setActiveTab = (next: TabId) => {
    /* Setting an explicit "app" still writes the param — that way
     * back-button history is consistent regardless of which tab you
     * land on first. */
    setSearchParams({ tab: next }, { replace: true });
  };

  const activeMeta = TABS.find((t) => t.id === activeTab)!;
  const description =
    activeTab === "kernel"
      ? tr(
          "logs_description_kernel",
          undefined,
          "Live stream of /dev/klog from the payload. Open Filters to hide Sony's routine subsystem chatter while hunting a specific issue.",
        )
      : tr(
          "logs_description_app",
          undefined,
          "In-app log of errors, warnings, and notable events on the desktop side. Useful for bug reports — click Copy to grab a plain-text dump.",
        );

  return (
    <div className="flex h-full flex-col p-6">
      <PageHeader
        icon={activeMeta.icon}
        title={tr("logs", undefined, "Logs")}
        description={description}
      />

      {/* Tab strip. Underline-style for low visual weight — the page
          header already announces the screen. Active tab gets the
          accent border + bold weight so it reads at a glance.
          A11y: role=tablist + role=tab + aria-selected is the WAI-
          ARIA pattern for tabs (aria-pressed is for toggle buttons,
          which screen readers announce differently). aria-controls
          ties each tab to its rendered panel id. */}
      <div
        role="tablist"
        aria-label={tr("logs", undefined, "Logs")}
        className="mb-4 flex items-center gap-1 border-b border-[var(--color-border)]"
      >
        {TABS.map(({ id, icon: Icon, key, fallback }) => {
          const isActive = id === activeTab;
          return (
            <button
              key={id}
              type="button"
              role="tab"
              id={`logs-tab-${id}`}
              aria-controls={`logs-panel-${id}`}
              aria-selected={isActive}
              tabIndex={isActive ? 0 : -1}
              onClick={() => setActiveTab(id)}
              className={
                "flex items-center gap-1.5 border-b-2 px-3 py-2 text-sm transition-colors " +
                (isActive
                  ? "border-[var(--color-accent)] font-semibold text-[var(--color-accent)]"
                  : "border-transparent text-[var(--color-muted)] hover:text-[var(--color-text)]")
              }
            >
              <Icon size={14} strokeWidth={1.75} />
              {tr(key, undefined, fallback)}
            </button>
          );
        })}
      </div>

      <div
        role="tabpanel"
        id={`logs-panel-${activeTab}`}
        aria-labelledby={`logs-tab-${activeTab}`}
        className="flex min-h-0 flex-1 flex-col"
      >
        {activeTab === "kernel" ? <KernelLogPanel /> : <AppLogsPanel />}
      </div>
    </div>
  );
}
