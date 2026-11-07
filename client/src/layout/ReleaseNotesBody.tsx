import { useEffect, useState } from "react";
import { ExternalLink } from "lucide-react";

import { Button } from "../components/Button";
import { MarkdownView } from "../components/MarkdownView";
import { openExternalUrl } from "../lib/openExternalUrl";
import { Spinner } from "../components/Spinner";
import { fetchReleaseNotes } from "../lib/releaseNotes";
import { useTr } from "../state/lang";

/** A release's notes: the ones the update check brought, else GitHub's for that release. A check
 *  that came without notes left the update notice's Details empty. `load` is for tests. */
export function ReleaseNotesBody({
  version,
  notes,
  load = fetchReleaseNotes,
}: {
  version: string;
  notes: string;
  load?: (version: string) => Promise<string>;
}) {
  const tr = useTr();
  const [fetched, setFetched] = useState<{ version: string; notes: string | null } | null>(null);
  const have = notes.trim() !== "";
  useEffect(() => {
    if (have) return;
    let live = true;
    load(version)
      .then((n) => live && setFetched({ version, notes: n }))
      .catch(() => live && setFetched({ version, notes: null }));
    return () => {
      live = false;
    };
  }, [have, version, load]);
  if (have) return <MarkdownView source={notes} />;
  const got = fetched?.version === version ? fetched : null;
  if (!got)
    return (
      <p className="flex items-center gap-2 text-[var(--color-muted)]" data-testid="release-notes-loading">
        <Spinner size={12} />
        {tr("loading", undefined, "Loading…")}
      </p>
    );
  if (got.notes) return <MarkdownView source={got.notes} />;
  return (
    <div className="space-y-2" data-testid="release-notes-unavailable">
      <p className="text-[var(--color-muted)]">
        {tr("update_notes_unavailable", { version }, "The notes of v{version} could not be read from GitHub.")}
      </p>
      <Button
        size="sm"
        variant="ghost"
        onClick={() =>
          void openExternalUrl(
            `https://github.com/phantomptr/ps5upload/releases/tag/v${version.replace(/^v/, "")}`,
          )
        }
      >
        <ExternalLink size={13} />
        {tr("library_fpkg_kstuff_tip_repo", undefined, "View on GitHub")}
      </Button>
    </div>
  );
}
