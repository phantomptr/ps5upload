import { FolderArchive, Gamepad2 } from "lucide-react";

import TabbedShell, { type TabbedShellTab } from "../../layout/TabbedShell";
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
 */
type GamesTab = "ready" | "files";

export default function GamesScreen() {
  const tr = useTr();
  const tabs: ReadonlyArray<TabbedShellTab<GamesTab>> = [
    {
      id: "ready",
      icon: Gamepad2,
      key: "games_tab_ready",
      fallback: "Ready to play",
      description: tr(
        "games_ready_description",
        undefined,
        "Games registered on this PS5. Launch, stop, inspect, or uninstall them here.",
      ),
    },
    {
      id: "files",
      icon: FolderArchive,
      key: "games_tab_files",
      fallback: "Game files",
      description: tr(
        "games_files_description",
        undefined,
        "Game folders and disk images found in console storage. Mount, register, move, or inspect source files here.",
      ),
    },
  ];

  return (
    <TabbedShell
      idPrefix="games"
      titleIcon={Gamepad2}
      titleKey="games_title"
      titleFallback="Games"
      tabs={tabs}
      renderPanel={(tab) =>
        tab === "files" ? (
          <LibraryScreen embedded />
        ) : (
          <InstalledAppsScreen embedded />
        )
      }
    />
  );
}
