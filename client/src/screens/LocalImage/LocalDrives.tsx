import { useCallback, useEffect, useState } from "react";
import { Eject, FolderOpen, HardDrive, Info } from "lucide-react";

import { Button, Callout, Card, ErrorCard, Spinner } from "../../components";
import { humanizePs5Error } from "../../lib/humanizeError";
import { openLocalPath } from "../../lib/openLocalPath";
import { pickPath } from "../../lib/pickPath";
import { isMacOrLinuxDesktop } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useTr } from "../../state/lang";
import {
  localImageAttach,
  localImageDetach,
  localImageStatus,
  type AttachedImage,
  type LocalImageStatus,
} from "../../api/ps5";
import { LOCAL_IMAGE_EXTENSIONS } from "./imageTypes";

function basename(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

/** Whether `path` is an image this computer can open as a drive (.exfat, .img, .image, .raw). */
export function isDriveImage(path: string): boolean {
  const lower = path.trim().toLowerCase();
  return LOCAL_IMAGE_EXTENSIONS.some((ext) => lower.endsWith(`.${ext}`));
}

export interface LocalDrives {
  /** Only a macOS or Linux desktop has the tools: the browser build would mount it on the
   *  engine's server, and Windows has no such tool. */
  canMount: boolean;
  status: LocalImageStatus | null;
  busy: boolean;
  error: string | null;
  clearError: () => void;
  /** Open `path` (or one picked now) as a drive, then show its files. */
  open: (path?: string) => Promise<void>;
  eject: (a: AttachedImage) => Promise<void>;
  /** Show a mounted image's files in this computer's file manager. */
  reveal: (mountPoint: string) => Promise<void>;
}

/** Opening a game image as a drive on this computer, and the images open that way now. */
export function useLocalDrives(): LocalDrives {
  const tr = useTr();
  const canMount = isTauriEnv() && isMacOrLinuxDesktop();
  const [status, setStatus] = useState<LocalImageStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!canMount) return;
    try {
      setStatus(await localImageStatus());
    } catch (e) {
      setError(String(e));
    }
  }, [canMount]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const reveal = useCallback(
    async (mountPoint: string) => {
      // Don't leave them staring at a screen that looks idle.
      if (!(await openLocalPath(mountPoint))) {
        setError(
          tr(
            "localimage_open_failed",
            { path: mountPoint },
            `The image is mounted at ${mountPoint}, but this computer would not open it. Open that folder yourself.`,
          ),
        );
      }
    },
    [tr],
  );

  const open = useCallback(
    async (given?: string) => {
      setError(null);
      const path =
        given ??
        (await pickPath({
          mode: "file",
          title: tr("localimage_pick", undefined, "Choose a game image"),
          filters: [
            { name: tr("localimage_filter", undefined, "Disk images"), extensions: [...LOCAL_IMAGE_EXTENSIONS] },
          ],
        }));
      if (!path) return;
      setBusy(true);
      try {
        const info = await localImageAttach(path);
        await refresh();
        // Open the mounted volume straight away: the point is the files, not a device name.
        if (info.mount_point) await reveal(info.mount_point);
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [refresh, reveal, tr],
  );

  const eject = useCallback(
    async (a: AttachedImage) => {
      setError(null);
      setBusy(true);
      try {
        await localImageDetach(a.device);
        await refresh();
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [refresh],
  );

  return { canMount, status, busy, error, clearError: () => setError(null), open, eject, reveal };
}

/**
 * The images open as drives on this computer, each with Show files and Eject, and how it
 * works. Rendered only on a macOS or Linux desktop; with nothing open and `quiet`, it shows
 * nothing at all (Workshop's converting view keeps it out of the way until it matters).
 */
export function LocalDrivesCard({ drives, quiet = false }: { drives: LocalDrives; quiet?: boolean }) {
  const tr = useTr();
  const { canMount, status, busy, error } = drives;
  if (!canMount) return null;
  const attached = status?.attached ?? [];
  const unsupported = status && !status.supported;
  if (quiet && attached.length === 0 && !error) return null;

  return (
    <Card
      title={tr("localimage_title_v2", undefined, "Opened as drives")}
      icon={HardDrive}
      right={
        !unsupported ? (
          <Button
            variant="secondary"
            size="sm"
            leftIcon={busy ? <Spinner size={13} tone="inherit" /> : <FolderOpen size={13} />}
            onClick={() => void drives.open()}
            disabled={busy}
          >
            {tr("localimage_open_v2", undefined, "Open an image as a drive…")}
          </Button>
        ) : undefined
      }
    >
      <div className="flex flex-col gap-3">
        {error && (
          <ErrorCard
            title={tr("localimage_failed", undefined, "Something went wrong")}
            detail={humanizePs5Error(error) || error}
            onDismiss={drives.clearError}
          />
        )}
        {unsupported && (
          <Callout tone="warn" title={tr("localimage_unsupported", undefined, "Not available on this system")}>
            {status?.unsupported_reason}
          </Callout>
        )}
        {!quiet && (
          <p className="flex items-start gap-2 text-sm text-[var(--color-muted)]">
            <Info size={14} className="mt-0.5 shrink-0" aria-hidden />
            <span>
              {tr(
                "localimage_explain",
                undefined,
                "It opens the image as a normal drive on this computer, so you can add, replace and delete files inside it — patch files, for example — using Finder or your file manager, exactly like a USB stick. Changes are saved straight into the image file. ps5upload never writes to the image itself; your operating system does. Eject it here when you are done.",
              )}
            </span>
          </p>
        )}
        {attached.length === 0 && !unsupported && !quiet && (
          <p className="text-sm text-[var(--color-muted)]">
            {tr("localimage_none_hint", undefined, "Choose a .exfat, .img, .image or .raw file to open it as a drive.")}
          </p>
        )}
        {attached.map((a) => (
          <div
            key={a.device}
            className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-[var(--color-border)] p-3"
          >
            <div className="min-w-0 flex-1 basis-[12rem]">
              <div className="truncate text-sm font-medium">{basename(a.image)}</div>
              <div className="mt-0.5 truncate font-mono text-xs text-[var(--color-muted)]">
                {a.mount_point ||
                  tr("localimage_not_mounted", undefined, "attached, but nothing could be mounted from it")}
              </div>
            </div>
            <div className="flex shrink-0 gap-2">
              {a.mount_point && (
                <Button
                  variant="secondary"
                  size="sm"
                  leftIcon={<FolderOpen size={13} />}
                  onClick={() => void drives.reveal(a.mount_point)}
                >
                  {tr("localimage_reveal", undefined, "Show files")}
                </Button>
              )}
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<Eject size={13} />}
                disabled={busy}
                onClick={() => void drives.eject(a)}
              >
                {tr("localimage_eject", undefined, "Eject")}
              </Button>
            </div>
          </div>
        ))}
      </div>
    </Card>
  );
}
