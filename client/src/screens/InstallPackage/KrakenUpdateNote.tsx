import { ExternalLink } from "lucide-react";
import type { KrakenStatus } from "../../api/ps5";
import { KRAKEN_MIN_VERSION, krakenOutdated } from "../../lib/mounter";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { useTr } from "../../state/lang";

const KRAKEN_RELEASES = "https://github.com/quer3q/kraken-mount/releases";

/** "Update Kraken Mount": shown while the console runs a Kraken Mount older than
 *  KRAKEN_MIN_VERSION, which leaves a mounted package's save data and trophies out. */
export function KrakenUpdateNote({ kraken }: { kraken: KrakenStatus | null | undefined }) {
  const tr = useTr();
  const old = krakenOutdated(kraken);
  if (!old) return null;
  return (
    <p className="mt-2 text-xs text-[var(--color-warn)]" data-testid="kraken-update-note">
      {tr(
        "kraken_update_note",
        { version: old, min: KRAKEN_MIN_VERSION },
        "Kraken Mount {version} can leave a mounted package's save data and trophies out. Update it to {min} or later from its project page.",
      )}{" "}
      <button
        type="button"
        className="inline-flex items-center gap-1 underline underline-offset-2"
        onClick={() => void openExternalUrl(KRAKEN_RELEASES)}
      >
        {tr("kraken_update_open", undefined, "Project page")}
        <ExternalLink size={11} aria-hidden />
      </button>
    </p>
  );
}
