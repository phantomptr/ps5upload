import { useSearchParams } from "react-router-dom";
import { Boxes, Rocket } from "lucide-react";

import { PageHeader } from "../../components";
import { useTr } from "../../state/lang";
import { makeTabKeyHandler } from "../../lib/tabKeyboardNav";
import CatalogPanel from "./CatalogPanel";
import SendPanel from "./SendPanel";

/**
 * Payloads shell — hosts two URL-routed tabs:
 *
 *   - **catalog**: curated GitHub-released third-party PS5 homebrew
 *     payloads (kstuff, etaHEN, ezremote, etc.) with one-click
 *     check / download / send.
 *   - **send**: arbitrary file picker for anything else you'd
 *     normally `nc` at :9021 (custom ELFs, BIN, JS, LUA, JAR).
 *     Includes playlists and recent-sends history.
 *
 * Pre-2.12.0 these were two separate sidebar entries (/payloads and
 * /send-payload). Merged into one /payloads entry with `?tab=send`
 * for the arbitrary-file flow. Tab state lives in the URL so deep
 * links and page refresh preserve the active tab.
 *
 * The catalog tab is the default — most users want curated payloads
 * with version tracking; the arbitrary-send tab is the escape hatch
 * for power users with custom or experimental binaries.
 */

type TabId = "catalog" | "send";

const TABS: Array<{ id: TabId; icon: typeof Boxes; key: string; fallback: string }> = [
  { id: "catalog", icon: Boxes, key: "payloads_tab_catalog", fallback: "Catalog" },
  { id: "send", icon: Rocket, key: "payloads_tab_send", fallback: "Send file" },
];

export default function PayloadsScreen() {
  const tr = useTr();
  const [searchParams, setSearchParams] = useSearchParams();

  const tabParam = searchParams.get("tab");
  const activeTab: TabId = tabParam === "send" ? "send" : "catalog";
  const setActiveTab = (next: TabId) => {
    setSearchParams({ tab: next }, { replace: true });
  };
  const onTabKey = makeTabKeyHandler<TabId>(
    TABS.map((t) => t.id),
    setActiveTab,
    (id) => `payloads-tab-${id}`,
  );

  const description =
    activeTab === "send"
      ? tr(
          "payloads_description_send",
          undefined,
          "Send any PS5 payload file — .elf, .bin, .js, .lua, or .jar (kstuff, custom homebrew loaders, browser-stage exploits, plugin scripts, BD-JB JARs) — to your PS5. Same flow as the Connection tab, just pointed at a file you choose. Note: BD-JB-style .jar payloads need a JAR-aware loader on a non-9021 port — set the port to whatever your loader listens on.",
        )
      : tr(
          "payloads_description_catalog",
          undefined,
          "Curated third-party PS5 homebrew payloads. Check for the latest release, download once, then send to your PS5 with one click. Versions cache locally so you can also bundle a USB autoloader stick.",
        );

  return (
    <div className="flex h-full flex-col p-6">
      <PageHeader
        icon={Boxes}
        title={tr("payloads", undefined, "Payloads")}
        description={description}
      />

      {/* A11y: role=tablist + role=tab + aria-selected is the WAI-
          ARIA pattern for tabs (aria-pressed is for toggle buttons,
          which screen readers announce differently). aria-controls
          ties each tab to its rendered panel id. */}
      <div
        role="tablist"
        aria-label={tr("payloads", undefined, "Payloads")}
        className="mb-4 flex items-center gap-1 border-b border-[var(--color-border)]"
      >
        {TABS.map(({ id, icon: Icon, key, fallback }) => {
          const isActive = id === activeTab;
          return (
            <button
              key={id}
              type="button"
              role="tab"
              id={`payloads-tab-${id}`}
              aria-controls={`payloads-panel-${id}`}
              aria-selected={isActive}
              tabIndex={isActive ? 0 : -1}
              onClick={() => setActiveTab(id)}
              onKeyDown={(e) => onTabKey(e, id)}
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
        id={`payloads-panel-${activeTab}`}
        aria-labelledby={`payloads-tab-${activeTab}`}
        className="min-h-0 flex-1 overflow-auto"
      >
        {activeTab === "send" ? <SendPanel /> : <CatalogPanel />}
      </div>
    </div>
  );
}
