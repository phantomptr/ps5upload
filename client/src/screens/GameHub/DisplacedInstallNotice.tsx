import { useEffect, useState } from "react";

import { fsPathPresent } from "../../api/ps5";
import { WarningCard } from "../../components";
import { transferAddr } from "../../lib/addr";
import { findDisplacedInstall, installWhereLabel, type DisplacedInstall } from "../../lib/consoleInstall";
import { useTr } from "../../state/lang";

/**
 * #884: an earlier PS5Upload added a copy of this game to the home screen while the PS5 had it
 * installed on M.2 or extended storage, and the PS5 now lists it under Console Storage and can't
 * start it. Shown only when that layout is on the console (the package still on M.2/extended
 * storage, PS5Upload's staged files and no package in /user/app/<id> on the internal drive).
 *
 * Read-only: it says what happened and the safest way back. It changes nothing on the console,
 * because the PS5's own record of the game is what changed, PS5Upload kept no copy of it, and
 * editing it blind could lose the game for good.
 */
export function DisplacedInstallNotice({
  host,
  titleId,
  name,
}: {
  host: string;
  titleId: string;
  name: string;
}) {
  const tr = useTr();
  const [found, setFound] = useState<DisplacedInstall | null>(null);

  useEffect(() => {
    let cancelled = false;
    setFound(null);
    if (!host.trim() || !titleId) return;
    findDisplacedInstall({ exists: (p) => fsPathPresent(transferAddr(host), p) }, titleId)
      .then((r) => {
        if (!cancelled) setFound(r);
      })
      // The console could not say: nothing to show.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [host, titleId]);

  if (!found) return null;
  const where = installWhereLabel(found.where);
  const steps = [
    tr(
      "displaced_install_step_saves",
      undefined,
      "Back up this game's saves first (Saves on this page, or Settings › Saved Data on the PS5).",
    ),
    tr(
      "displaced_install_step_delete",
      { name },
      "On the PS5, go to Settings › Storage › Console Storage › Games and Apps, select {name} and delete it. Saves are kept.",
    ),
    tr(
      "displaced_install_step_download",
      { name, where },
      "Download {name} again from the Game Library. To have it on {where} again, choose that first in Settings › Storage › Installation Location.",
    ),
    tr(
      "displaced_install_step_no_move",
      { where },
      "Don't use Move Games and Apps for this game: the copy the PS5 lists on Console Storage has no game data, and moving it onto {where} could overwrite the real game there.",
    ),
    tr(
      "displaced_install_step_rebuild",
      undefined,
      "Only if it still won't start: Rebuild Database in Safe Mode. It keeps games and saves, but resets the home screen's order, and games added by PS5Upload or other homebrew have to be added again.",
    ),
  ];
  return (
    <div className="mb-3" data-testid="displaced-install">
      <WarningCard
        title={tr("displaced_install_title", { name }, "{name} needs to be installed again")}
        detail={
          <div className="flex flex-col gap-2">
            <p>
              {tr(
                "displaced_install_body",
                { name, where, pkg: found.pkg },
                "{name} is installed on {where} ({pkg}), but an earlier version of PS5Upload added a copy of it to the home screen, and the PS5 now lists the game under Console Storage, where it has no game data, so it can't start. PS5Upload no longer does this. Your game files and saves are still there.",
              )}
            </p>
            <ol className="list-decimal ps-5">
              {steps.map((s) => (
                <li key={s}>{s}</li>
              ))}
            </ol>
          </div>
        }
      />
    </div>
  );
}
