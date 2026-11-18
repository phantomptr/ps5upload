import { useEffect, useId, useMemo, useRef, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router";
import { Layers, Upload } from "lucide-react";

import type { CollectionGame, CollectionLocation } from "../../api/collection";
import {
  fetchVolumes,
  volumeAllocatableBytes,
  volumeLikelyFitsBytes,
  type Volume,
} from "../../api/ps5";
import { Button, Checkbox, Select } from "../../components";
import { consoleAddr } from "../../lib/addr";
import {
  DEFAULT_SEND_SUBPATH,
  collectionSendItem,
  packageSendItems,
  sendDestination,
  sendKind,
  type SendPlan,
} from "../../lib/collectionSend";
import { krakenRuns, useMounterFor } from "../../lib/mounter";
import { krakenCopyDestination } from "../../lib/pkgUploadChoice";
import { pushNotification } from "../../state/notifications";
import { afterKrakenSend } from "../../state/pkgMount";
import { useSmpState } from "./SmpNeeded";
import { formatCollectionBytes, locationKind } from "../../lib/collectionView";
import { isRemotePath } from "../../lib/remotePath";
import { useFpkgConversion } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";
import { useUploadQueueStore } from "../../state/uploadQueue";
import { imageDrive, imageDriveChoices } from "../../lib/imageUpload";
import { destinationVolumeFor } from "../../state/upload";
import { ActivityLine } from "./ActivityLine";
import { useCopyActivity } from "../../state/copyActivity";
import { useConnectionStore } from "../../state/connection";
import { queueLinkFor } from "../../lib/gamePage";
import { spacePlanLink } from "../../lib/spacePlan";

type How = "as-is" | "image";

/** Send a game folder, image or archive from the collection to the PS5 in one click: it goes
 *  straight into the Upload queue and starts (only it: other queued uploads keep waiting). A
 *  package goes as a plain file (nothing installs it), by default into the folder Kraken Mount
 *  scans when it runs on the console. */
export function SendToPs5({
  game,
  host,
  copies,
  initial,
}: {
  game: Pick<CollectionGame, "title">;
  host: string;
  /** The copies that can be sent (folders, images, archives, and a package picked to send). */
  copies: CollectionLocation[];
  initial: CollectionLocation;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const location = useLocation();
  const [path, setPath] = useState(initial.absolute_path);
  const loc = copies.find((c) => c.absolute_path === path) ?? initial;
  const kind = sendKind(loc);
  // A local game folder can go as one compressed image: far fewer files to write, smaller on
  // the PS5, and ShadowMount+ mounts it. It is built on this computer first.
  const canImage = kind === "folder" && !isRemotePath(loc.absolute_path);
  const [how, setHow] = useState<How>("as-is");
  const radioName = useId();
  const [volumes, setVolumes] = useState<Volume[] | null>(null);
  // Picked here or on the Upload screen; none: this console's drive for packages (#673).
  const [picked, setVolume] = useState<string | null>(() => destinationVolumeFor(host));
  const [subpath, setSubpath] = useState(DEFAULT_SEND_SUBPATH);
  const [register, setRegister] = useState(true);
  const pipelineBusy = useFpkgConversion((s) => s.pipeline.phase === "running");
  const [activity] = useCopyActivity([loc.absolute_path], host);
  const connectedHost = useConnectionStore((s) => s.host ?? "");
  const isPkg = loc.type === "pkg";
  // Which mounter runs, read once a package is picked (the copy rows may have read it already).
  useSmpState(isPkg ? host : "");
  const mounter = useMounterFor(host);
  const kraken = krakenRuns(mounter) ? (mounter?.kraken ?? null) : null;
  // A drive or folder the user set here is never replaced by Kraken Mount's folder.
  const driveTouched = useRef(false);
  const folderTouched = useRef(false);

  useEffect(() => {
    let live = true;
    fetchVolumes(consoleAddr(host))
      .then((v) => live && setVolumes(imageDriveChoices(v)))
      .catch(() => live && setVolumes([]));
    return () => {
      live = false;
    };
  }, [host]);

  const volume = imageDrive(host, picked, volumes).volume;
  // A package goes where Kraken Mount finds it: its folder on the chosen drive, else its first
  // folder on a drive this console has.
  const krakenDest =
    isPkg && kraken
      ? krakenCopyDestination(volume, kraken, (volumes ?? []).map((v) => v.path))
      : null;
  useEffect(() => {
    if (!krakenDest || folderTouched.current) return;
    if (krakenDest.volume !== volume) {
      if (driveTouched.current) return;
      setVolume(krakenDest.volume);
    }
    setSubpath(krakenDest.subpath);
  }, [krakenDest?.volume, krakenDest?.subpath]); // eslint-disable-line react-hooks/exhaustive-deps
  const vol = volumes?.find((v) => v.path === volume);
  // Only a certain shortfall blocks; the PS5's write-time reserve only warns.
  const needed = how === "image" ? Math.round(loc.size_bytes * 0.8) : loc.size_bytes;
  const fits: "yes" | "tight" | "no" | "unknown" = !vol
    ? "unknown"
    : needed > volumeAllocatableBytes(vol)
      ? "no"
      : needed > volumeLikelyFitsBytes(vol)
        ? "tight"
        : "yes";
  const plan: SendPlan = { host, volume, subpath, register };
  // Clean up's planner with this game's size and drive filled in, and a way back here after.
  const planLink = spacePlanLink({
    neededBytes: needed,
    drive: volume,
    retry: { kind: "back", to: location.pathname, label: game.title },
  });
  const dest = useMemo(() => sendDestination(loc, plan), [loc, volume, subpath]); // eslint-disable-line react-hooks/exhaustive-deps
  const busy = !!activity && activity.phase !== "done" && activity.phase !== "failed";
  // The package lands in a folder Kraken Mount scans (it scans one level deep).
  const inKrakenFolder =
    !!krakenDest &&
    krakenDest.volume === volume &&
    krakenDest.subpath === subpath.replace(/^\/+|\/+$/g, "");

  const send = () => {
    if (how === "image") {
      void useFpkgConversion
        .getState()
        .buildImage(loc.absolute_path, undefined, true, "ffpkg", {
          host,
          volume,
          subpath,
          deleteAfter: true,
        });
      return;
    }
    const q = useUploadQueueStore.getState();
    if (isPkg) {
      // As Upload's "Copy as a file": plain files, no install, no staging, nothing deleted.
      const ids = packageSendItems(game, loc, plan).map((it) => q.add(it)).filter(Boolean);
      q.startAdded(host, ids);
      // Once it is there: Kraken Mount is asked to scan now, and the user is told where it went.
      if (inKrakenFolder) {
        const folder = dest.replace(/\/[^/]*$/, "");
        afterKrakenSend(ids, host, folder, (message) => pushNotification("info", message));
      }
      return;
    }
    const id = q.add(collectionSendItem(game, loc, plan));
    q.startAdded(host, [id]);
  };

  return (
    <div className="mt-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-4 text-sm">
      {copies.length > 1 && (
        <Select
          label={tr("collection.send_which", undefined, "Copy")}
          value={path}
          onChange={(e) => setPath(e.target.value)}
          options={copies.map((c) => ({
            value: c.absolute_path,
            label: `${locationKind(c.type)} · ${formatCollectionBytes(c.size_bytes)} · ${c.name}`,
          }))}
        />
      )}
      {canImage && (
        <div className="mt-2 grid gap-1">
          <label className="flex items-start gap-2">
            <input
              type="radio"
              name={radioName}
              checked={how === "image"}
              onChange={() => setHow("image")}
              className="mt-1"
            />
            <span>
              <span className="font-medium">
                {tr("collection.send_as_image", undefined, "As one compressed image (.ffpfsc)")}
              </span>{" "}
              <span className="text-xs text-[var(--color-muted)]">
                {tr(
                  "collection.send_as_image_hint",
                  undefined,
                  "Faster to send and smaller on the PS5; ShadowMount+ mounts it. Built on this computer first.",
                )}
              </span>
            </span>
          </label>
          <label className="flex items-start gap-2">
            <input
              type="radio"
              name={radioName}
              checked={how === "as-is"}
              onChange={() => setHow("as-is")}
              className="mt-1"
            />
            <span className="font-medium">
              {tr("collection.send_as_folder", undefined, "As the folder")}
            </span>
          </label>
        </div>
      )}
      <div className="mt-2 flex flex-wrap items-end gap-2">
        <Select
          block={false}
          label={tr("collection.send_drive", undefined, "Drive")}
          value={volume}
          onChange={(e) => {
            driveTouched.current = true;
            setVolume(e.target.value);
          }}
          options={(volumes && volumes.length > 0
            ? volumes
            : [{ path: "/data", free_bytes: 0 } as Volume]
          ).map((v) => ({
            value: v.path,
            label: v.free_bytes
              ? `${v.path} · ${formatCollectionBytes(volumeAllocatableBytes(v))} ${tr("fs_free", undefined, "free")}`
              : v.path,
          }))}
        />
        <label className="grid gap-1 text-xs text-[var(--color-muted)]">
          {tr("collection.send_folder", undefined, "Folder")}
          <input
            value={subpath}
            onChange={(e) => {
              folderTouched.current = true;
              setSubpath(e.target.value);
            }}
            className="w-36 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3 py-1.5 font-mono text-sm text-[var(--color-text)]"
          />
        </label>
      </div>
      {kind === "folder" && how === "as-is" && (
        <Checkbox
          className="mt-2"
          checked={register}
          onChange={setRegister}
          label={tr("collection.send_register", undefined, "Register the game on the PS5 when it is there")}
        />
      )}
      <p className="mt-2 break-all font-mono text-[0.6875rem] text-[var(--color-muted)]">
        {tr("collection.send_to", { dest }, "To {dest}")}
      </p>
      {isPkg && (
        <p className="mt-1 text-xs text-[var(--color-muted)]">
          {inKrakenFolder
            ? tr(
                "collection.send_pkg_kraken_hint",
                undefined,
                "Goes as a plain file, nothing is installed. Kraken Mount mounts it from this folder.",
              )
            : tr(
                "collection.send_pkg_file_hint",
                undefined,
                "Goes as a plain file, nothing is installed.",
              )}
        </p>
      )}
      {fits === "no" && (
        <p className="mt-1 text-xs text-[var(--color-bad)]">
          {tr("collection.send_no_room", undefined, "Not enough room on that drive.")}{" "}
          <Link to="/cleanup" className="underline">
            {tr("collection.send_free_space", undefined, "Free up space")}
          </Link>{" "}
          ·{" "}
          <Link to={planLink} className="underline">
            {tr("cleanup_plan_link", undefined, "Plan space")}
          </Link>
        </p>
      )}
      {fits === "tight" && (
        <p className="mt-1 text-xs text-[var(--color-warn)]">
          {tr(
            "collection.send_tight",
            undefined,
            "It may not fit: the PS5 holds back more space as it writes.",
          )}{" "}
          <Link to={planLink} className="underline">
            {tr("cleanup_plan_link", undefined, "Plan space")}
          </Link>
        </p>
      )}
      {how === "image" && pipelineBusy && !busy && (
        <p className="mt-1 text-xs text-[var(--color-warn)]">
          {tr(
            "collection.send_convert_busy",
            undefined,
            "Convert is building another game. Try again when it is done.",
          )}
        </p>
      )}
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <Button
          size="sm"
          variant="primary"
          leftIcon={how === "image" ? <Layers size={13} /> : <Upload size={13} />}
          disabled={busy || volumes === null || fits === "no" || (how === "image" && pipelineBusy)}
          onClick={send}
        >
          {how === "image"
            ? tr("collection.send_go_image", undefined, "Build and send")
            : tr("collection.send_submit", undefined, "Send")}
        </Button>
        <ActivityLine
          activity={activity}
          onOpen={() =>
            navigate(
              how === "image" && activity?.phase === "building"
                ? "/workshop"
                : queueLinkFor(host, connectedHost),
            )
          }
        />
      </div>
    </div>
  );
}
