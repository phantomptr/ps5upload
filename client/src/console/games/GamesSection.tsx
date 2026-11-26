import { useState } from "react";
import { Gamepad2, LibraryBig } from "lucide-react";

import { Library } from "../library/Library";
import type { SectionContext } from "../sections";
import { t } from "../strings";
import { Games } from "./Games";

export type GamesView = "installed" | "library";

/** The view a Games opening asks for: the Library when opened with view=library or to scan
 *  (Home's "USB drive connected"), else Installed. */
export function gamesViewOf(params: Record<string, string> | null): GamesView {
  return params?.view === "library" || params?.scan === "1" ? "library" : "installed";
}

/**
 * Games: the console's installed games, and the games on its drives and shares (the Library,
 * #498), one Rail entry with a switch under the title. They were two Rail entries that both
 * listed games, the second already marking which were installed; the switch keeps each view
 * as it was, one press apart.
 */
export function GamesSection({ ctx }: { ctx: SectionContext }) {
  const [view, setView] = useState<GamesView>(() => gamesViewOf(ctx.params));
  // Home's "USB drive connected" asks for one scan: switching to Installed and back does not scan again.
  const [scan, setScan] = useState(ctx.params?.scan === "1");
  const switcher = (
    <div className="cw-toolbar" role="group" aria-label={t("cw_games_title")}>
      <button
        type="button"
        className="cw-focusable cw-button"
        aria-pressed={view === "installed"}
        data-action="games-view-installed"
        onClick={() => {
          setScan(false);
          setView("installed");
        }}
      >
        <Gamepad2 aria-hidden />
        <span>{t("cw_games_view_installed")}</span>
      </button>
      <button
        type="button"
        className="cw-focusable cw-button"
        aria-pressed={view === "library"}
        data-action="games-view-library"
        onClick={() => setView("library")}
      >
        <LibraryBig aria-hidden />
        <span>{t("cw_games_view_library")}</span>
      </button>
    </div>
  );
  return view === "library" ? (
    <Library scan={scan} ctx={ctx} switcher={switcher} />
  ) : (
    <Games ctx={ctx} switcher={switcher} />
  );
}
