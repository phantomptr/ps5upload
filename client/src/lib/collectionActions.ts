// Collection actions shared by the cards, the table, the batch bar and the game page: hand
// copies to Convert, and send games to one or more consoles.

import type { NavigateFunction } from "react-router";

import type {
  CollectionGame,
  CollectionLocation,
  GameConsoleState,
} from "../api/collection";
import { installOffers } from "../screens/GameHub/collectionInstall";
import { jobCancel, jobStatus, startTransferDownload } from "../api/ps5";
import {
  loadCollection,
  loadConsoleStates,
  startCollectionScan,
  useCollectionStore,
} from "../state/collection";
import { pushNotification } from "../state/notifications";
import { watchJob } from "../state/watchedJobs";
import { consoleAddr, hostOf } from "./addr";
import { isRemotePath } from "./remotePath";
import { useConvertPrefs } from "../state/convertPrefs";
import { useUploadQueueStore } from "../state/uploadQueue";
import { destinationVolumeFor } from "../state/upload";
import { sendStep } from "./collectionInsights";
import { DEFAULT_SEND_SUBPATH, collectionSendItem } from "./collectionSend";
import { imageDriveNow } from "./imageUpload";

/** Shows Workshop set to a compressed image for this visit: a preset, not the user's own choice,
 *  so what Workshop opens with next time is unchanged. */
function presetCompressedImage(): void {
  const p = useConvertPrefs.getState();
  p.showMake("image");
  p.showImageChoice(p.imageFormat, true);
}

/** Opens Convert with these copies: one is checked there, several go into its queue with the
 *  settings Convert has. `compress`: make a compressed game image (.ffpfsc) of them, the
 *  "take less room" build. */
export function openInConvert(
  navigate: NavigateFunction,
  copies: CollectionLocation[],
  opts: { compress?: boolean } = {},
): void {
  if (copies.length === 0) return;
  if (opts.compress) presetCompressedImage();
  if (copies.length === 1) {
    navigate("/workshop", { state: { source: copies[0].absolute_path } });
    return;
  }
  navigate("/workshop", {
    state: {
      sources: copies.map((c) => ({
        path: c.absolute_path,
        isDir: c.type === "folder",
      })),
    },
  });
}

export interface SendResult {
  host: string;
  /** Packages put in the install queue. */
  installs: number;
  /** Folders, images and archives put in the Upload queue. */
  uploads: number;
  /** Games the console already has, up to date. */
  upToDate: number;
  /** Games with nothing that can be sent (an incomplete or unreadable copy only). */
  nothing: number;
  /** The console's state could not be read: games it may have were still queued, and the
   *  install queue refuses one that would erase an installed game. */
  unread: boolean;
}

/** Sends `games` to the console at `host`: newer updates and missing DLC for what it has; the
 *  base package (and its add-ons), or the best folder, image or archive, for what it lacks. */
export async function sendGamesTo(
  host: string,
  games: CollectionGame[],
): Promise<SendResult> {
  await loadConsoleStates(host);
  const entry = useCollectionStore.getState().consoleByHost[host];
  const unread = !entry || !!entry.error;
  const states: Record<string, GameConsoleState> = unread ? {} : entry.states;
  const out: SendResult = {
    host,
    installs: 0,
    uploads: 0,
    upToDate: 0,
    nothing: 0,
    unread,
  };
  const offers: Parameters<typeof installOffers>[1] = [];
  const uploadIds: string[] = [];
  const q = useUploadQueueStore.getState();
  // Asked once, and only if something is sent as a copy (#673).
  let drive: string | null = null;
  for (const g of games) {
    const step = sendStep(g, unread ? undefined : states[g.game_id]);
    if (step.kind === "install") {
      for (const offer of step.offers) offers.push({ game: g, offer });
    } else if (step.kind === "upload") {
      drive ??= (await imageDriveNow(host, destinationVolumeFor(host))).volume;
      uploadIds.push(
        q.add(
          collectionSendItem(g, step.copy, {
            host,
            volume: drive,
            subpath: DEFAULT_SEND_SUBPATH,
            register: true,
          }),
        ),
      );
    } else if (step.why === "up-to-date") {
      out.upToDate += 1;
    } else {
      out.nothing += 1;
    }
  }
  out.installs = installOffers(host, offers);
  out.uploads = uploadIds.length;
  if (uploadIds.length > 0) q.startAdded(host, uploadIds);
  return out;
}

/** A game folder or image on a console, as Convert's source (`ps5://host/path`). A package
 *  installed from it replaces the dump; an image is made on this computer and leaves it alone. */
export function consoleSource(host: string, path: string): string {
  return `ps5://${hostOf(host)}${path.startsWith("/") ? path : `/${path}`}`;
}

/** Opens a console copy in Convert; `compress` presets a compressed game image (.ffpfsc). */
export function openConsoleCopyInConvert(
  navigate: NavigateFunction,
  host: string,
  path: string,
  opts: { compress?: boolean } = {},
): void {
  if (opts.compress) presetCompressedImage();
  navigate("/workshop", { state: { source: consoleSource(host, path) } });
}

/** The Collection folder a copy from a console lands in: the first folder on this computer
 *  (a server folder is skipped: downloads write to this computer). Null when there is none. */
export async function collectionDropFolder(): Promise<string | null> {
  if (!useCollectionStore.getState().settings) await loadCollection();
  const roots = useCollectionStore.getState().settings?.roots ?? [];
  return roots.find((r) => !isRemotePath(r)) ?? null;
}

/** Downloads a game folder or image from the console at `host` into the Collection, then scans
 *  so it shows there. `false`: the Collection has no folder on this computer to put it in. */
export async function copyToCollection(opts: {
  host: string;
  path: string;
  kind: "file" | "folder";
  label: string;
  /** Notification text, already translated. */
  doneTitle: string;
  failedTitle: string;
}): Promise<boolean> {
  const dest = await collectionDropFolder();
  if (!dest) return false;
  void watchJob(
    {
      key: `tocollection:${hostOf(opts.host)}:${opts.path}`,
      kind: "download",
      origin: "collection.copy",
      label: opts.label,
      host: opts.host,
      fallbackDest: dest,
    },
    () =>
      startTransferDownload(opts.path, dest, consoleAddr(opts.host), opts.kind),
    {
      jobStatus,
      jobCancel,
      sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
    },
    {
      onEnd: (job) => {
        if (job.phase === "done") {
          pushNotification("info", opts.doneTitle, {
            body: dest,
            link: "/games?tab=all",
          });
          void startCollectionScan(false);
        } else if (job.phase === "failed") {
          pushNotification("error", opts.failedTitle, {
            body: job.error ?? undefined,
          });
        }
      },
    },
  );
  return true;
}
