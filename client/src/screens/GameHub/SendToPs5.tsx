import { useEffect, useId, useMemo, useState } from "react";
import { useNavigate } from "react-router";
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
  sendDestination,
  sendKind,
  type SendPlan,
} from "../../lib/collectionSend";
import { formatCollectionBytes, locationKind } from "../../lib/collectionView";
import { isRemotePath } from "../../lib/remotePath";
import { useFpkgConversion } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";
import { useUploadQueueStore } from "../../state/uploadQueue";
import { useUploadStore } from "../../state/upload";
import { ActivityLine } from "./ActivityLine";
import { useCopyActivity } from "../../state/copyActivity";
import { useConnectionStore } from "../../state/connection";
import { queueLinkFor } from "../../lib/gamePage";

type How = "as-is" | "image";

/** Send a game folder, image or archive from the collection to the PS5 in one click: it goes
 *  straight into the Upload queue and starts (only it: other queued uploads keep waiting). */
export function SendToPs5({
  game,
  host,
  copies,
  initial,
}: {
  game: Pick<CollectionGame, "title">;
  host: string;
  /** The copies that can be sent (folders, images, archives). */
  copies: CollectionLocation[];
  initial: CollectionLocation;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const [path, setPath] = useState(initial.absolute_path);
  const loc = copies.find((c) => c.absolute_path === path) ?? initial;
  const kind = sendKind(loc);
  // A local game folder can go as one compressed image: far fewer files to write, smaller on
  // the PS5, and ShadowMount+ mounts it. It is built on this computer first.
  const canImage = kind === "folder" && !isRemotePath(loc.absolute_path);
  const [how, setHow] = useState<How>("as-is");
  const radioName = useId();
  const [volumes, setVolumes] = useState<Volume[] | null>(null);
  const [volume, setVolume] = useState<string | null>(
    useUploadStore.getState().destinationVolume ?? "/data",
  );
  const [subpath, setSubpath] = useState(DEFAULT_SEND_SUBPATH);
  const [register, setRegister] = useState(true);
  const pipelineBusy = useFpkgConversion((s) => s.pipeline.phase === "running");
  const [activity] = useCopyActivity([loc.absolute_path], host);
  const connectedHost = useConnectionStore((s) => s.host ?? "");

  useEffect(() => {
    let live = true;
    fetchVolumes(consoleAddr(host))
      .then((v) => live && setVolumes(v.filter((x) => x.writable && !x.is_placeholder)))
      .catch(() => live && setVolumes([]));
    return () => {
      live = false;
    };
  }, [host]);

  const vol = volumes?.find((v) => v.path === (volume ?? "/data"));
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
  const dest = useMemo(() => sendDestination(loc, plan), [loc, volume, subpath]); // eslint-disable-line react-hooks/exhaustive-deps
  const busy = !!activity && activity.phase !== "done" && activity.phase !== "failed";

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
    const id = q.add(collectionSendItem(game, loc, plan));
    void q.startHost(host, { onlyIds: [id] });
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
          value={volume ?? "/data"}
          onChange={(e) => setVolume(e.target.value)}
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
            onChange={(e) => setSubpath(e.target.value)}
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
      {fits === "no" && (
        <p className="mt-1 text-xs text-[var(--color-bad)]">
          {tr("collection.send_no_room", undefined, "Not enough room on that drive.")}
        </p>
      )}
      {fits === "tight" && (
        <p className="mt-1 text-xs text-[var(--color-warn)]">
          {tr(
            "collection.send_tight",
            undefined,
            "It may not fit: the PS5 holds back more space as it writes.",
          )}
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
          disabled={busy || fits === "no" || (how === "image" && pipelineBusy)}
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
                ? "/convert"
                : queueLinkFor(host, connectedHost, false),
            )
          }
        />
      </div>
    </div>
  );
}
