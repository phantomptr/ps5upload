// Where a game image goes on the PS5 (#673): the drive and folder, wherever the Workshop offers to
// upload one, and the real path before anything runs. With no drive picked it is the drive set
// under Volumes › Use for packages for that console.

import { useEffect, useId, useState } from "react";

import {
  fetchVolumes,
  volumeAllocatableBytes,
  type Volume,
} from "../../api/ps5";
import { consoleAddr, hostOf } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import {
  DEFAULT_IMAGE_SUBPATH,
  imageDrive,
  imageDriveChoices,
} from "../../lib/imageUpload";
import { usePkgStorageStore } from "../../lib/pkgStorage";
import { resolveUploadDest } from "../../lib/uploadDest";
import { useTr } from "../../state/lang";

/** The console's drives; null until known or when they cannot be read. */
export function useConsoleVolumes(host: string | null): Volume[] | null {
  const [volumes, setVolumes] = useState<Volume[] | null>(null);
  const key = host ? hostOf(host) : "";
  useEffect(() => {
    setVolumes(null);
    if (!key) return;
    let live = true;
    fetchVolumes(consoleAddr(key))
      .then((v) => {
        if (live) setVolumes(v);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [key]);
  return volumes;
}

/** The folder an image lands in: `<drive>/<folder>`. */
export function imageFolderPath(volume: string, subpath: string): string {
  return resolveUploadDest(volume, subpath || DEFAULT_IMAGE_SUBPATH, "x")
    .destRoot;
}

export function ImageDestination({
  host,
  volume,
  subpath,
  onChange,
}: {
  /** The console it goes to. */
  host: string;
  /** The drive picked; null: this console's drive for packages. */
  volume: string | null;
  subpath: string;
  onChange: (volume: string | null, subpath: string) => void;
}) {
  const tr = useTr();
  const driveId = useId();
  const folderId = useId();
  const volumes = useConsoleVolumes(host);
  // A change under Volumes shows here at once.
  usePkgStorageStore((s) => s.defaults);
  const auto = imageDrive(host, null, volumes);
  const now = imageDrive(host, volume, volumes);
  const choices = imageDriveChoices(volumes);
  const free = (path: string) => {
    const v = choices.find((c) => c.path === path);
    return v?.free_bytes ? volumeAllocatableBytes(v) : null;
  };
  const label = (path: string) => {
    const bytes = free(path);
    return bytes === null
      ? path
      : `${path} · ${formatBytes(bytes)} ${tr("fs_free", undefined, "free")}`;
  };
  const shown =
    volume && !choices.some((c) => c.path === volume) ? [volume] : [];
  return (
    <div className="flex flex-col gap-2 text-sm" data-testid="image-dest">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <label htmlFor={driveId} className="text-[var(--color-muted)]">
          {tr("collection.send_drive", undefined, "Drive")}
        </label>
        <select
          id={driveId}
          className="select w-auto!"
          data-testid="image-dest-drive"
          value={volume ?? ""}
          onChange={(e) => onChange(e.target.value || null, subpath)}
        >
          <option value="">
            {tr(
              "image_dest_default",
              { path: label(auto.volume) },
              `Default: ${label(auto.volume)}`,
            )}
          </option>
          {[...shown, ...choices.map((c) => c.path)].map((path) => (
            <option key={path} value={path}>
              {label(path)}
            </option>
          ))}
        </select>
        <label htmlFor={folderId} className="text-[var(--color-muted)]">
          {tr("collection.send_folder", undefined, "Folder")}
        </label>
        <input
          id={folderId}
          className="input w-36!"
          data-testid="image-dest-folder"
          value={subpath}
          placeholder={DEFAULT_IMAGE_SUBPATH}
          onChange={(e) => onChange(volume, e.target.value)}
        />
      </div>
      <span
        className="break-all text-xs text-[var(--color-muted)]"
        data-testid="image-dest-path"
      >
        {(() => {
          const path = `${imageFolderPath(now.volume, subpath)}/…`;
          return tr("image_dest_goes_to", { path }, `Goes to ${path}`);
        })()}
      </span>
      {now.fellBack && volumes !== null && (
        <span
          className="text-xs text-[var(--color-warn)]"
          data-testid="image-dest-fell-back"
        >
          {tr(
            "image_dest_fell_back",
            undefined,
            "The drive set for it cannot be used right now, so internal storage is used.",
          )}
        </span>
      )}
    </div>
  );
}
