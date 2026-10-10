import { useEffect, useRef, useState } from "react";

import { startLinkDownload, type LinkClass } from "../../api/links";
import { fetchVolumes, jobCancel, jobStatus } from "../../api/ps5";
import { Button, Input, Spinner } from "../../components";
import { consoleAddr, hostOf, transferAddr } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { pkgStorageFor } from "../../lib/pkgStorage";
import { useTr } from "../../state/lang";
import {
  dismissWatchedJob,
  stopWatchedJob,
  useWatchedJobStore,
  watchJob,
  watchedJob,
  type WatchedJobDeps,
} from "../../state/watchedJobs";

const watchDeps: WatchedJobDeps = {
  jobStatus: (id) => jobStatus(id),
  jobCancel,
  sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
};

/** Where a download-only file goes when the user does not choose: next to the package
 *  library on the console's default drive, in its own `downloads` folder. */
export function defaultDownloadDir(host: string): string {
  const storage = pkgStorageFor(host, null);
  return storage.dir.replace(/\/pkg_library$/, "/downloads");
}

/** A link that is a real file but not a package (R4, #368): offer to download it to a console
 *  folder. The engine streams it over AVA1; nothing is stored on this computer. */
export function LinkDownloadCard({
  host,
  url,
  info,
  insecureTls,
  onClose,
}: {
  host: string;
  url: string;
  info: LinkClass;
  insecureTls: boolean;
  onClose: () => void;
}) {
  const tr = useTr();
  const [dir, setDir] = useState(() => defaultDownloadDir(host));
  const [name, setName] = useState(info.filename);
  // The download belongs to this console and link, not to this card (see state/watchedJobs):
  // it keeps running, and shows in Tasks, when the user leaves the screen.
  const jobKey = `linkdl:${hostOf(host)}:${url}`;
  const job = useWatchedJobStore((s) => watchedJob(s, jobKey));
  const phase: "idle" | "running" | "done" | "failed" =
    !job || job.phase === "stopped" ? "idle" : job.phase;
  const sent = job?.sent ?? 0;
  const error = job?.phase === "failed" ? job.error : null;
  const landed =
    job?.phase === "done" ? (job.dest ?? `${dir.trim()}/${name.trim()}`) : null;
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    // The real default drive, once the volume list is known.
    fetchVolumes(transferAddr(host))
      .then((v) => {
        if (!alive.current) return;
        const storage = pkgStorageFor(host, v);
        setDir((cur) =>
          cur === defaultDownloadDir(host)
            ? storage.dir.replace(/\/pkg_library$/, "/downloads")
            : cur,
        );
      })
      .catch(() => {});
    return () => {
      alive.current = false;
    };
  }, [host]);

  const total = info.total_size ?? 0;

  function start() {
    const fileName = name.trim() || null;
    const destDir = dir.trim();
    void watchJob(
      {
        key: jobKey,
        kind: "download",
        origin: "install",
        label: fileName ?? info.filename,
        host,
        detail: destDir,
      },
      () =>
        startLinkDownload({
          url,
          destDir,
          addr: consoleAddr(host),
          fileName,
          insecureTls,
        }),
      watchDeps,
    );
  }

  function cancel() {
    stopWatchedJob(jobKey, watchDeps);
  }

  // Closing the card after a finished download forgets it; a running one stays in Tasks.
  const close = () => {
    dismissWatchedJob(jobKey);
    onClose();
  };

  return (
    <div className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] mb-4 p-3">
      <div className="text-sm font-medium text-[var(--color-text)]">
        {tr("linkdl.title", undefined, "This link is a file, not a package")}
      </div>
      <p className="my-1 text-xs text-[var(--color-muted)]">
        {tr(
          "linkdl.help",
          undefined,
          "It cannot be installed, but it can be downloaded straight to a folder on the PS5. The file goes to the console over your network and is not stored on this computer.",
        )}
      </p>
      <p className="mb-2 text-xs text-[var(--color-muted)]">
        {info.filename}
        {total > 0 ? ` (${formatBytes(total)})` : ""}
      </p>
      <div className="flex flex-wrap gap-2">
        <div className="min-w-52 flex-1">
          <Input
            label={tr("linkdl.folder", undefined, "Folder on the PS5")}
            value={dir}
            onChange={(e) => setDir(e.currentTarget.value)}
            disabled={phase === "running"}
            spellCheck={false}
          />
        </div>
        <div className="min-w-40 flex-1">
          <Input
            label={tr("linkdl.name", undefined, "File name")}
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
            disabled={phase === "running"}
            spellCheck={false}
          />
        </div>
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-2">
        {phase !== "running" && phase !== "done" && (
          <Button
            variant="secondary"
            size="sm"
            onClick={start}
            disabled={!dir.trim().startsWith("/") || !name.trim()}
          >
            {tr("linkdl.start", undefined, "Download to the PS5")}
          </Button>
        )}
        {phase === "running" && (
          <>
            <Spinner size={14} tone="accent" />
            <span className="text-xs">
              {total > 0
                ? tr(
                    "linkdl.progress",
                    { sent: formatBytes(sent), total: formatBytes(total) },
                    "Downloading to the PS5: {sent} of {total}",
                  )
                : tr("linkdl.running", undefined, "Downloading to the PS5…")}
            </span>
            <Button variant="ghost" size="sm" onClick={cancel}>
              {tr("linkdl.cancel", undefined, "Cancel")}
            </Button>
          </>
        )}
        {phase === "done" && (
          <span className="text-xs text-[var(--color-good)]">
            {tr(
              "linkdl.done",
              { path: landed ?? "" },
              "Saved on the PS5 at {path}",
            )}
          </span>
        )}
        {phase !== "running" && (
          <Button variant="ghost" size="sm" onClick={close}>
            {phase === "done"
              ? tr("linkdl.close", undefined, "Close")
              : tr("linkdl.dismiss", undefined, "Not now")}
          </Button>
        )}
      </div>
      {error && (
        <p role="alert" className="mt-2 text-xs text-[var(--color-bad)]">
          {error}
        </p>
      )}
    </div>
  );
}
