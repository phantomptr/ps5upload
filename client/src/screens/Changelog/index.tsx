import { useEffect, useMemo, useState } from "react";
import { loadBundledDoc } from "../../lib/bundledDoc";
import { recentChangelog } from "../../lib/changelogDoc";
import { Sparkles, ExternalLink } from "lucide-react";
import { openExternalUrl as openExternal } from "../../lib/openExternalUrl";

import {
  PageHeader,
  EmptyState,
  ErrorCard,
  MarkdownView,
  Button,
} from "../../components";
import { log } from "../../state/logs";
import { useTr } from "../../state/lang";

const GITHUB_RELEASES_URL =
  "https://github.com/phantomptr/ps5upload/releases";

/**
 * Changelog screen ("What's new"). Renders the CHANGELOG.md the app was built
 * with, newest versions first and the rest on request, plus a link to the GitHub
 * releases page (release downloads, and notes for versions newer than this build).
 *
 * `embedded`: shown as the What's new tab of Help, under that screen's own
 * header; the GitHub link then sits above the notes.
 */
export default function ChangelogScreen({ embedded = false }: { embedded?: boolean } = {}) {
  const tr = useTr();
  const [raw, setRaw] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showAll, setShowAll] = useState(false);
  const recent = useMemo(() => (raw === null ? null : recentChangelog(raw)), [raw]);

  useEffect(() => {
    (async () => {
      try {
        const content = await loadBundledDoc("changelog");
        setRaw(content);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        log.error("changelog", "failed to load CHANGELOG.md", msg);
        setError(msg);
      }
    })();
  }, []);

  const fullHistory = (
    <Button
      variant="secondary"
      size="sm"
      rightIcon={<ExternalLink size={12} />}
      onClick={() => openExternal(GITHUB_RELEASES_URL)}
    >
      {tr("changelog_full_history", undefined, "Full history")}
    </Button>
  );

  return (
    <div className={embedded ? "" : "app-page"}>
      {embedded ? (
        <div className="mb-4">{fullHistory}</div>
      ) : (
        <PageHeader
          icon={Sparkles}
          title={tr("whats_new", undefined, "What's new")}
          description={tr(
            "changelog_description_v2",
            undefined,
            "Release notes for every ps5upload version up to this one, bundled with the app. Newer releases and their downloads are on GitHub.",
          )}
          right={fullHistory}
        />
      )}

      <div className="max-w-4xl">
        {error && (
          <div className="mb-4">
            <ErrorCard
              title={tr("changelog_load_error", undefined, "Couldn't load CHANGELOG.md")}
              detail={error}
            />
          </div>
        )}

        {raw === null && !error && (
          <EmptyState message={tr("loading", undefined, "Loading…")} />
        )}

        {raw !== null && recent && <MarkdownView source={showAll ? raw : recent.text} />}
        {recent?.more && !showAll && (
          <Button variant="secondary" size="sm" onClick={() => setShowAll(true)}>
            {tr("changelog_show_older", undefined, "Show older versions")}
          </Button>
        )}
      </div>
    </div>
  );
}
