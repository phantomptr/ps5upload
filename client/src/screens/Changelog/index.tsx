import { useEffect, useState } from "react";
import { loadBundledDoc } from "../../lib/bundledDoc";
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
 * Changelog screen ("What's new"). Renders the whole CHANGELOG.md the app
 * was built with, plus a link to the GitHub releases page (release
 * downloads, and notes for versions newer than this build).
 */
export default function ChangelogScreen() {
  const tr = useTr();
  const [raw, setRaw] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

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

  return (
    <div className="app-page">
      <PageHeader
        icon={Sparkles}
        title={tr("whats_new", undefined, "What's new")}
        description={tr(
          "changelog_description_v2",
          undefined,
          "Release notes for every ps5upload version up to this one, bundled with the app. Newer releases and their downloads are on GitHub.",
        )}
        right={
          <Button
            variant="secondary"
            size="sm"
            rightIcon={<ExternalLink size={12} />}
            onClick={() => openExternal(GITHUB_RELEASES_URL)}
          >
            {tr("changelog_full_history", undefined, "Full history")}
          </Button>
        }
      />

      <div className="mx-auto max-w-3xl">
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

        {raw !== null && <MarkdownView source={raw} />}
      </div>
    </div>
  );
}
