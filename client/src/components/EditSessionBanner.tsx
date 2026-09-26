import { useEffect } from "react";
import { useNavigate } from "react-router";
import { FilePenLine, FolderOpen, X } from "lucide-react";

import { Button } from "./Button";
import { useConnectionStore } from "../state/connection";
import { editSessionForHost, useEditSessionStore } from "../state/editSession";
import { useTr } from "../state/lang";
import { pushNotification } from "../state/notifications";
import { withConsolePrefix } from "../state/roster";
import { saveFsLastPath } from "../lib/fsLastPath";

/**
 * Standing reminder that an image is checked out of ShadowMount+ for editing.
 *
 * This is not decoration. While the session is open the image lives outside
 * ShadowMount+'s scan folders, which means the game is GONE from the PS5 home
 * screen until the session is finished. A user who forgets — or who closes the
 * app mid-edit and comes back later — would otherwise have no way to discover
 * why their game vanished, so the banner is rendered on every screen the edit
 * flow passes through and the session is re-probed from the console on mount.
 */
export default function EditSessionBanner({
  onFinished,
}: {
  /** Called after a session is successfully checked back in. The Games and
   *  Files screens pass their own refresh: the image reappears in its
   *  original folder and the mount disappears, so a stale listing would show
   *  neither until the user refreshed by hand. */
  onFinished?: () => void;
} = {}) {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const checkout = useEditSessionStore(
    (s) => editSessionForHost(s, host).checkout,
  );
  const imageRw = useEditSessionStore(
    (s) => editSessionForHost(s, host).imageRw,
  );
  const busy = useEditSessionStore((s) => editSessionForHost(s, host).busy);
  const error = useEditSessionStore((s) => editSessionForHost(s, host).error);
  const refresh = useEditSessionStore((s) => s.refresh);
  const finish = useEditSessionStore((s) => s.finish);

  // Probe on mount and whenever the console changes. Cheap (one small file
  // read) and it's the only way a session started elsewhere — or before a
  // crash — becomes visible again.
  useEffect(() => {
    if (payloadStatus !== "up") return;
    void refresh(host);
  }, [host, payloadStatus, refresh]);

  const session = imageRw ?? checkout;
  if (!session) return null;

  const imagePath = imageRw?.image_path ?? checkout?.original_path ?? "";
  const name = imagePath.split("/").pop() ?? imagePath;

  const runFinish = async () => {
    try {
      const done = await finish(host);
      if (done) {
        onFinished?.();
        pushNotification(
          "info",
          withConsolePrefix(
            host,
            tr("edit_session_done", undefined, "Edit session finished"),
          ),
          {
            body: tr(
              "edit_session_done_body",
              { name },
              `${name} is back in its original folder. ShadowMount+ will mount and register it again within about a minute.`,
            ),
          },
        );
      }
    } catch {
      // The store already captured the message; it renders below.
    }
  };

  return (
    <div className="flex flex-col gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-accent-soft)] p-3">
      <div className="flex flex-wrap items-center gap-3">
        <FilePenLine
          size={18}
          className="shrink-0 text-[var(--color-accent)]"
        />
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm font-semibold">
            {tr("edit_session_title", { name }, `Editing ${name}`)}
          </div>
          <div className="mt-0.5 truncate font-mono text-xs text-[var(--color-muted)]">
            {session.mount_point}
          </div>
          <div className="mt-1 text-xs text-[var(--color-muted)]">
            {tr(
              "edit_session_body",
              undefined,
              imageRw
                ? "This image is temporarily writable. Finish editing to remount it read-only and reduce the risk of accidental image corruption."
                : "While this is open the game is hidden from the PS5 home screen — ShadowMount+ can't see the image where it is now. Finish editing to put it back.",
            )}
          </div>
        </div>
        <div className="flex shrink-0 flex-wrap gap-2">
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<FolderOpen size={12} />}
            onClick={() => {
              // `?path=` rather than just navigate("/files"): the browser
              // seeds its folder from storage only on mount, so a plain
              // navigate does nothing when the user is already on Files —
              // which is where this banner also renders. Seed storage too so
              // the folder sticks for the next visit.
              saveFsLastPath(host, session.mount_point);
              navigate(`/files?path=${encodeURIComponent(session.mount_point)}`);
            }}
            disabled={busy}
          >
            {tr("edit_session_open", undefined, "Open files")}
          </Button>
          <Button
            variant="primary"
            size="sm"
            leftIcon={<X size={12} />}
            onClick={() => void runFinish()}
            disabled={busy}
            loading={busy}
          >
            {tr("edit_session_finish", undefined, "Finish editing")}
          </Button>
        </div>
      </div>
      {error && (
        <div className="rounded border border-[var(--color-bad)] bg-[var(--color-surface-2)] px-2 py-1 text-xs text-[var(--color-bad)]">
          {error}
        </div>
      )}
    </div>
  );
}
