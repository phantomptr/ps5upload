import { FolderArchive, Gamepad2 } from "lucide-react";
import { useSearchParams } from "react-router";

import { PageHeader, Tabs } from "../../components";
import { useTr } from "../../state/lang";
import InstalledAppsScreen from "../InstalledApps";
import LibraryScreen from "../Library";

/**
 * One Games workspace with two deliberately user-facing views:
 *
 * - Ready to play: titles registered with the PS5 and actionable now.
 * - Game files: folders and disk images stored on console-attached storage.
 *
 * The old "Library" / "Installed Apps" pair exposed implementation details
 * as sibling destinations and made users guess which inventory they needed.
 * Keeping both inventories is useful; naming and locating them by intent is
 * what removes the ambiguity.
 *
 * The views are chips (the shared Tabs), selected by `?tab=` so a link, a
 * refresh or Back lands on the same one.
 */
type GamesTab = "ready" | "files";

export default function GamesScreen() {
  const tr = useTr();
  const [searchParams, setSearchParams] = useSearchParams();
  const tab: GamesTab = searchParams.get("tab") === "files" ? "files" : "ready";
  const setTab = (next: string) => {
    if (next === tab) return;
    // A full view, not a transient filter: push it so Back returns to the
    // previous one. Other query state the panels own is kept.
    const params = new URLSearchParams(searchParams);
    params.set("tab", next);
    setSearchParams(params);
  };

  const label = tr("games_title", undefined, "Games");
  return (
    <div className="app-page flex h-full flex-col">
      <PageHeader
        icon={Gamepad2}
        title={label}
        description={
          tab === "files"
            ? tr(
                "games_files_description",
                undefined,
                "Game folders and disk images found in console storage. Mount, register, move, or inspect source files here.",
              )
            : tr(
                "games_ready_description",
                undefined,
                "Games registered on this PS5. Launch, stop, inspect, or uninstall them here.",
              )
        }
      />
      <Tabs
        ariaLabel={label}
        value={tab}
        onChange={setTab}
        className="mb-6"
        tabs={[
          { id: "ready", icon: Gamepad2, label: tr("games_tab_ready", undefined, "Ready to play") },
          { id: "files", icon: FolderArchive, label: tr("games_tab_files", undefined, "Game files") },
        ]}
      />
      <div role="tabpanel" aria-label={tab === "files" ? tr("games_tab_files", undefined, "Game files") : tr("games_tab_ready", undefined, "Ready to play")} className="min-h-0 flex-1">
        {tab === "files" ? <LibraryScreen /> : <InstalledAppsScreen />}
      </div>
    </div>
  );
}
