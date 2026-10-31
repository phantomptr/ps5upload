import { useEffect, useState } from "react";
import { Copy, Sparkles, X } from "lucide-react";

import { useTr } from "../state/lang";
import { getEngineUrl } from "../state/engine";
import { Modal } from "../components/Modal";
import { MarkdownView } from "../components/MarkdownView";
import { Button } from "../components/Button";
import { writeClipboard } from "../lib/clipboard";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/** What the engine says about a newer release (engine_update.rs). */
export interface EngineUpdate {
  current: string;
  latest?: string;
  available: boolean;
  notes?: string;
  docker: boolean;
}

/** What a Docker user runs to update: in the folder of their compose file. */
export const DOCKER_UPDATE_COMMAND = "docker compose pull && docker compose up -d";

const DISMISSED_KEY = "ps5upload.web_update_dismissed";

/** Whether to show the bar for this answer: an update, not dismissed for that version. */
export function shouldShowWebUpdate(u: EngineUpdate | null, dismissed: string | null): boolean {
  return !!u?.available && !!u.latest && u.latest !== dismissed;
}

/**
 * The update bar for the web UI (a browser using a self-hosted engine). The desktop app has
 * UpdateToast, which downloads and installs; a page can't, so this one says what's new and how
 * to update where the engine runs: pull the new image, or update the app on that computer.
 */
export function WebUpdateToast() {
  const tr = useTr();
  const [update, setUpdate] = useState<EngineUpdate | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(() => safeGetItem(DISMISSED_KEY));
  const [showNotes, setShowNotes] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let live = true;
    fetch(`${getEngineUrl()}/api/engine/update`, { signal: AbortSignal.timeout(20_000) })
      .then((r) => (r.ok ? (r.json() as Promise<EngineUpdate>) : null))
      .then((u) => live && setUpdate(u))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, []);

  if (!shouldShowWebUpdate(update, dismissed) || !update?.latest) return null;
  const version = update.latest;

  return (
    <div className="mx-3 mt-2 flex items-center gap-3 rounded-full border border-[color-mix(in_srgb,var(--color-accent)_35%,transparent)] bg-[var(--color-accent-soft)] px-4 py-2 text-sm md:mx-6">
      <Sparkles size={15} className="shrink-0 text-[var(--color-accent)]" />
      <span className="min-w-0 flex-1 truncate">
        <span className="font-medium text-[var(--color-text)]">
          {tr("update_toast_title", undefined, "Update available")}
        </span>
        <span className="text-[var(--color-muted)]"> — v{version}</span>
      </span>
      <button
        type="button"
        onClick={() => setShowNotes(true)}
        className="shrink-0 rounded px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
      >
        {tr("update_toast_details", undefined, "Details")}
      </button>
      <button
        type="button"
        aria-label={tr("update_toast_dismiss", undefined, "Dismiss")}
        onClick={() => {
          safeSetItem(DISMISSED_KEY, version);
          setDismissed(version);
        }}
        className="shrink-0 rounded-full p-1.5 text-[var(--color-muted)] hover:text-[var(--color-text)]"
      >
        <X size={14} />
      </button>
      {showNotes && (
        <Modal
          open
          onClose={() => setShowNotes(false)}
          title={tr("whats_new_in", { version }, `What's new in v${version}`)}
          size="xl"
        >
          <div className="max-h-[60vh] overflow-auto p-4 text-sm">
            <div className="mb-4 rounded-[var(--radius-card)] border border-[var(--color-border)] p-3">
              {update.docker ? (
                <>
                  <p className="mb-2">
                    {tr(
                      "web_update_how_docker",
                      undefined,
                      "To update, run this in the folder with your compose file (Watchtower can also do it for you):",
                    )}
                  </p>
                  <div className="flex items-center gap-2">
                    <code className="min-w-0 flex-1 overflow-x-auto rounded bg-[var(--color-surface)] px-2 py-1 font-mono text-xs">
                      {DOCKER_UPDATE_COMMAND}
                    </code>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => void writeClipboard(DOCKER_UPDATE_COMMAND).then((ok) => setCopied(ok))}
                    >
                      <Copy size={13} />
                      {copied ? "✓" : tr("copy", undefined, "Copy")}
                    </Button>
                  </div>
                </>
              ) : (
                <p>
                  {tr(
                    "web_update_how_app",
                    undefined,
                    "This page comes from PS5Upload on another computer. Update the app there, and this page updates with it.",
                  )}
                </p>
              )}
            </div>
            {update.notes?.trim() ? <MarkdownView source={update.notes} /> : null}
          </div>
        </Modal>
      )}
    </div>
  );
}

export default WebUpdateToast;
