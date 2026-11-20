import { useState } from "react";
import { create } from "zustand";
import { Copy, Sparkles, X } from "lucide-react";

import { useTr } from "../state/lang";
import { getEngineUrl } from "../state/engine";
import { Modal } from "../components/Modal";
import { ReleaseNotesBody } from "./ReleaseNotesBody";
import { Button } from "../components/Button";
import { writeClipboard } from "../lib/clipboard";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { pushNotification } from "../state/notifications";
import { trStatic } from "../lib/trStatic";

/** What the engine says about a newer release (engine_update.rs). */
export interface EngineUpdate {
  current: string;
  latest?: string;
  available: boolean;
  notes?: string;
  docker: boolean;
}

/** What a Docker user runs to update: in the folder of their compose file. Pulling leaves the
 * replaced image behind (~170 MB a release), so the last step removes this app's images that no
 * container uses; the label filter keeps it away from every other image on the machine. */
export const DOCKER_UPDATE_COMMAND =
  "docker compose pull && docker compose up -d && docker image prune -af --filter label=org.opencontainers.image.source=https://github.com/phantomptr/ps5upload";

const DISMISSED_KEY = "ps5upload.web_update_dismissed";
const NOTIFIED_KEY = "ps5upload.web_update_notified";

/** Whether to show the update for this answer: an update, not dismissed for that version. */
export function shouldShowWebUpdate(u: EngineUpdate | null, dismissed: string | null): boolean {
  return !!u?.available && !!u.latest && u.latest !== dismissed;
}

interface WebUpdateState {
  update: EngineUpdate | null;
  dismissed: string | null;
  /** Asks the engine once per page load (the engine itself checks GitHub on its own schedule). */
  load: () => void;
  dismiss: (version: string) => void;
}

let loading = false;

/** The web UI's update (a browser using a self-hosted engine): read once, shared by the Settings
 *  row, the Settings dot and the update banner. */
export const useWebUpdateStore = create<WebUpdateState>((set, get) => ({
  update: null,
  dismissed: safeGetItem(DISMISSED_KEY),
  load() {
    if (loading || get().update) return;
    loading = true;
    fetch(`${getEngineUrl()}/api/engine/update`, { signal: AbortSignal.timeout(20_000) })
      .then((r) => (r.ok ? (r.json() as Promise<EngineUpdate>) : null))
      .then((u) => {
        set({ update: u });
        // The bell gets it once per version, like the desktop app's update notice.
        if (u?.available && u.latest && safeGetItem(NOTIFIED_KEY) !== u.latest) {
          safeSetItem(NOTIFIED_KEY, u.latest);
          pushNotification(
            "info",
            trStatic("update_notif_title", "Update available — v{ver}", { ver: u.latest }),
            { link: "/settings" },
          );
        }
      })
      .catch(() => undefined)
      .finally(() => {
        loading = false;
      });
  },
  dismiss(version) {
    safeSetItem(DISMISSED_KEY, version);
    set({ dismissed: version });
  },
}));

/** The update shown by the web UI now, or null. */
export function webUpdateWaiting(s: Pick<WebUpdateState, "update" | "dismissed">): string | null {
  return shouldShowWebUpdate(s.update, s.dismissed) ? (s.update?.latest ?? null) : null;
}

/** How to update where the engine runs, with the release's notes: Details in the web UI. */
export function WebUpdateHowTo({
  version,
  update,
  onClose,
}: {
  version: string;
  update: EngineUpdate | null;
  onClose: () => void;
}) {
  const tr = useTr();
  const [copied, setCopied] = useState(false);
  return (
    <Modal open onClose={onClose} title={tr("whats_new_in", { version }, `What's new in v${version}`)} size="xl">
      <div className="max-h-[60vh] overflow-auto p-4 text-sm">
        <div className="mb-4 rounded-[var(--radius-card)] border border-[var(--color-border)] p-3">
          {update?.docker ? (
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
        {update?.latest === version && <ReleaseNotesBody version={version} notes={update.notes ?? ""} />}
      </div>
    </Modal>
  );
}

/**
 * The update row at the top of Settings in the web UI (a browser using a self-hosted engine). The
 * desktop app has UpdateToast, which downloads and installs; a page can't, so this one says
 * what's new and how to update where the engine runs: pull the new image, or update the app on
 * that computer. A routine update shows only here, as a dot on Settings and in the bell
 * (lib/updateNotice); the banner across the top is for a PS5 that runs a newer helper.
 */
export function WebUpdateToast() {
  const tr = useTr();
  const update = useWebUpdateStore((s) => s.update);
  const version = useWebUpdateStore(webUpdateWaiting);
  const dismiss = useWebUpdateStore((s) => s.dismiss);
  const [showNotes, setShowNotes] = useState(false);

  if (!version) return null;

  return (
    <div
      data-testid="settings-update-row"
      className="mb-4 flex items-center gap-3 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-accent)_35%,transparent)] bg-[var(--color-accent-soft)] px-4 py-2 text-sm"
    >
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
        onClick={() => dismiss(version)}
        className="shrink-0 rounded-full p-1.5 text-[var(--color-muted)] hover:text-[var(--color-text)]"
      >
        <X size={14} />
      </button>
      {showNotes && <WebUpdateHowTo version={version} update={update} onClose={() => setShowNotes(false)} />}
    </div>
  );
}

export default WebUpdateToast;
