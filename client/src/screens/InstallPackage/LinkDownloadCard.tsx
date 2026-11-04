import { useEffect, useRef, useState } from "react";

import { startLinkDownload, type LinkClass } from "../../api/links";
import { fetchVolumes, jobCancel, jobStatus } from "../../api/ps5";
import { Button, Checkbox, Input, Spinner } from "../../components";
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

/** Where a game image goes: the default drive's `homebrew` folder, which ShadowMount+ scans
 *  (internal storage: `/data/homebrew`). */
export function imageDirFor(volume: string | null): string {
  return volume ? `${volume.replace(/\/+$/, "")}/homebrew` : "/data/homebrew";
}

const IMAGE_LABEL: Record<string, string> = { exfat: "exFAT", ufs: "UFS (.ffpkg)", pfs: "PFS" };

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
  const lz4 = info.lz4 ?? null;
  const canUnpack = !!lz4 && !lz4.unsupported && (lz4.unpacked_size ?? 0) > 0;
  const [unpack, setUnpack] = useState(canUnpack);
  const unpacking = canUnpack && unpack;
  // What ShadowMount+ will be handed: the image inside an .lz4 being unpacked, or a plain image.
  const image = unpacking ? lz4?.image ?? null : lz4 ? null : (info.image ?? null);
  // The default folder depends on the console's drives, known once their list arrives; both the
  // first guess and the switch below compare against the default for the drives known now.
  const [volumes, setVolumes] = useState<Parameters<typeof pkgStorageFor>[1]>(null);
  const defaultDirFor = (img: string | null, v: Parameters<typeof pkgStorageFor>[1]) => {
    const storage = pkgStorageFor(host, v);
    return img ? imageDirFor(storage.volume) : storage.dir.replace(/\/pkg_library$/, "/downloads");
  };
  const [dir, setDir] = useState(() => defaultDirFor(image, null));
  const linkName = (u: boolean) => (u && lz4 ? lz4.unpacked_name : info.filename);
  const [name, setName] = useState(() => linkName(unpacking));
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
        setVolumes(v);
        setDir((cur) => (cur === defaultDirFor(image, null) ? defaultDirFor(image, v) : cur));
      })
      .catch(() => {});
    return () => {
      alive.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host]);

  const packed = info.total_size ?? 0;
  const total = unpacking ? (lz4?.unpacked_size ?? 0) : packed;

  /** Turning unpacking on or off changes the name and folder, unless the user edited them. */
  function toggleUnpack(on: boolean) {
    setName((cur) => (cur === linkName(unpack) ? linkName(on) : cur));
    const wasDir = defaultDirFor(image, volumes);
    const nextImage = on ? lz4?.image ?? null : null;
    const nextDir = defaultDirFor(nextImage, volumes);
    setDir((cur) => (cur === wasDir ? nextDir : cur));
    setUnpack(on);
  }

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
          unpack: unpacking,
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
        {lz4
          ? tr("linkdl.lz4_title", undefined, "This link is LZ4-compressed")
          : image
            ? tr("linkdl.image_title", undefined, "This link is a game image")
            : tr("linkdl.title", undefined, "This link is a file, not a package")}
      </div>
      <p className="my-1 text-xs text-[var(--color-muted)]">
        {lz4
          ? tr(
              "linkdl.lz4_help",
              undefined,
              "ps5upload can unpack it while it downloads, so the PS5 receives the unpacked file. Nothing is stored on this computer, and the PS5 only needs room for the unpacked file.",
            )
          : tr(
              "linkdl.help",
              undefined,
              "It cannot be installed, but it can be downloaded straight to a folder on the PS5. The file goes to the console over your network and is not stored on this computer.",
            )}
      </p>
      <p className="mb-2 text-xs text-[var(--color-muted)]">
        {info.filename}
        {packed > 0 ? ` (${formatBytes(packed)})` : ""}
        {unpacking && lz4?.unpacked_size
          ? ` → ${name.trim() || lz4.unpacked_name} (${formatBytes(lz4.unpacked_size)})`
          : ""}
      </p>
      {lz4 && (
        <div className="mb-2">
          <Checkbox
            checked={unpacking}
            disabled={!canUnpack || phase === "running"}
            onChange={toggleUnpack}
            label={tr("linkdl.lz4_unpack", undefined, "Unpack on the way")}
            hint={
              lz4.unsupported
                ? lz4.unsupported
                : tr(
                    "linkdl.lz4_unpack_hint",
                    undefined,
                    "Off: the .lz4 file is saved on the PS5 as it is.",
                  )
            }
          />
        </div>
      )}
      {image && (
        <p className="mb-2 text-xs text-[var(--color-muted)]">
          {tr(
            "linkdl.image_hint",
            { kind: IMAGE_LABEL[image] ?? image },
            "Game image ({kind}). ShadowMount+ mounts it as it is from a folder it scans, such as homebrew on a drive. The file appears there only once it is complete.",
          )}
        </p>
      )}
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
