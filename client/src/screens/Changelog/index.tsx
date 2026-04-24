import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sparkles, ExternalLink } from "lucide-react";
import { open as openExternal } from "@tauri-apps/plugin-shell";

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
 * Changelog screen — the default landing page, so users see "what's
 * new" every time they launch. Renders CHANGELOG.md with a header
 * pointing at the GitHub releases page for full history beyond what
 * ships bundled.
 */
export default function ChangelogScreen() {
  const tr = useTr();
  const [raw, setRaw] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const content = await invoke<string>("changelog_load");
        setRaw(content);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        log.error("changelog", "failed to load CHANGELOG.md", msg);
        setError(msg);
      }
    })();
  }, []);

  return (
    <div className="p-6">
      <PageHeader
        icon={Sparkles}
        title={tr("whats_new", undefined, "What's new")}
        description={tr(
          "changelog_description",
          undefined,
          "Release notes for ps5upload. Old versions live on GitHub; the most recent entries are bundled with the app.",
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
