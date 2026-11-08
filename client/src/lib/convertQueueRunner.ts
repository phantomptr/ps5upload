// The Convert queue's real runner: each build is the Convert screen's own pipeline (so the
// running item shows its stages there), and a finished package is handed to the console queue.

import { fpkg } from "../api/fpkg";
import { pipelinePhaseChanged, setConvertRunner, type ConvertItem } from "../state/convertQueue";
import { useFpkgConversion } from "../state/fpkgConversion";
import { pkgLibraryStore } from "../state/pkgLibrary";
import { imageDestFor } from "./imageDest";

/** Resolves once the pipeline is not running (a manual run started first finishes first). */
function whenIdle(): Promise<void> {
  if (useFpkgConversion.getState().pipeline.phase !== "running") return Promise.resolve();
  return new Promise((resolve) => {
    const off = useFpkgConversion.subscribe((s) => {
      if (s.pipeline.phase !== "running") {
        off();
        resolve();
      }
    });
  });
}

/** The output folder as the waiting game's options have it (typed there, so maybe with spaces
 *  at the ends); none means the engine's default. */
function outputOf(item: ConvertItem): string | undefined {
  return item.outputDir?.trim() || undefined;
}

/** How a pipeline run ended, for the queue. */
function outcome(installed: boolean) {
  const p = useFpkgConversion.getState().pipeline;
  if (p.phase === "done") return { ok: true, packagePath: p.packagePath, installed };
  if (p.phase === "failed") return { ok: false, message: p.message, cancelled: p.cancelled };
  return { ok: false, message: "The build did not start." };
}

async function build(item: ConvertItem) {
  await whenIdle();
  const conv = useFpkgConversion.getState();
  // A build that kept running after a reload: its result, not a second build of the same game.
  const left = conv.pipeline;
  if (left.phase === "done" && left.attached && left.source === item.source) {
    useFpkgConversion.setState({ pipeline: { ...left, attached: false } });
    const send = item.build?.kind === "image" && !item.toServer && item.then !== "keep" && !!item.host;
    if (send) {
      const dest = imageDestFor(item.imageDest, item.host!);
      conv.uploadImage({ host: item.host!, volume: dest.volume, subpath: dest.subpath, deleteAfter: !!item.deleteAfterInstall });
    }
    return { ok: true, packagePath: left.packagePath, installed: !!item.build && item.build.kind !== "pkg" };
  }
  if (item.build?.kind === "folder") {
    // A plain folder from an image or package: kept here, nothing to hand on.
    await conv.buildFolder(item.source, outputOf(item), item.keepBoth, {
      fullVerify: item.fullVerify,
      zstd: item.build.zstd,
      ...(item.toServer ? { toServer: item.toServer } : {}),
    });
    await whenIdle();
    return outcome(true);
  }
  if (item.build?.kind === "image") {
    // A game image, in the format picked; sent on to the Upload queue when asked. Saved to a
    // server, it is written there as it is built: nothing here to upload after.
    const send = !item.toServer && item.then !== "keep" && !!item.host;
    // A game changed to an image while it waited was added with no destination: the one set now.
    const dest = send ? imageDestFor(item.imageDest, item.host!) : null;
    await conv.buildImage(
      item.source,
      outputOf(item),
      item.build.compress && !item.build.lz4,
      item.build.format,
      dest
        ? {
            host: item.host!,
            volume: dest.volume,
            subpath: dest.subpath,
            deleteAfter: !!item.deleteAfterInstall,
          }
        : undefined,
      item.build.lz4,
      item.password,
      item.keepBoth,
      {
        fullVerify: item.fullVerify,
        imageLevel: item.imageLevel,
        addAmprEmu: item.addAmprEmu,
        amprEmuBuild: item.amprEmuBuild,
        ...(item.toServer ? { toServer: item.toServer } : {}),
      },
    );
    await whenIdle();
    // Handed to the Upload queue (or kept): nothing more for this queue to do.
    return outcome(true);
  }
  // A console dump is swapped for its package inside the build (never installed beside it).
  const swap = item.source.startsWith("ps5://") && item.then !== "keep" && !!item.host;
  await conv.start(
    {
      source: item.source,
      outputDir: outputOf(item),
      compression: item.compression,
      firmware: item.firmware,
      language: item.language,
      keepBoth: item.keepBoth,
    },
    {
      install: swap,
      host: swap ? item.host : null,
      method: item.then === "upload" ? "upload" : "stream",
      password: item.password,
      fullVerify: item.fullVerify,
    },
  );
  await whenIdle();
  return outcome(swap);
}

async function handOff(item: ConvertItem, packagePath: string) {
  const host = item.host!;
  const store = pkgLibraryStore(host).getState();
  const r =
    item.then === "upload" ? await store.uploadInstall(packagePath, host) : await store.installStream(packagePath, host);
  // Only a package this engine built, and only after its install verified.
  if (r.ok && item.deleteAfterInstall) await fpkg.deletePackage(packagePath).catch(() => {});
  return { ok: !!r.ok, message: r.message };
}

let installed = false;
/** Wire the queue to the real pipeline and console queue (once, from the app shell). */
export function installConvertRunner() {
  if (installed) return;
  installed = true;
  setConvertRunner({ build, handOff, ready: whenIdle });
  // A build left running by a reload or another window shows again.
  void useFpkgConversion.getState().attachRunning();
  // A queue armed during a hand-started build starts once its result is put away (#621).
  useFpkgConversion.subscribe((s, prev) => {
    if (s.pipeline.phase !== prev.pipeline.phase) pipelinePhaseChanged(s.pipeline.phase);
  });
}
