// The Convert queue's real runner: each build is the Convert screen's own pipeline (so the
// running item shows its stages there), and a finished package is handed to the console queue.

import { fpkg } from "../api/fpkg";
import { setConvertRunner, type ConvertItem } from "../state/convertQueue";
import { useFpkgConversion } from "../state/fpkgConversion";
import { pkgLibraryStore } from "../state/pkgLibrary";

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

async function build(item: ConvertItem) {
  await whenIdle();
  const conv = useFpkgConversion.getState();
  if (item.build?.kind === "image") {
    // A game image, in the format picked; sent on to the Upload queue when asked.
    const send = item.then !== "keep" && !!item.host && !!item.imageDest;
    await conv.buildImage(
      item.source,
      item.outputDir,
      item.build.compress,
      item.build.format,
      send
        ? {
            host: item.host!,
            volume: item.imageDest!.volume,
            subpath: item.imageDest!.subpath,
            deleteAfter: !!item.deleteAfterInstall,
          }
        : undefined,
    );
    await whenIdle();
    const p = useFpkgConversion.getState().pipeline;
    // Handed to the Upload queue (or kept): nothing more for this queue to do.
    if (p.phase === "done") return { ok: true, packagePath: p.packagePath, installed: true };
    if (p.phase === "failed") return { ok: false, message: p.message };
    return { ok: false, message: "The build did not start." };
  }
  // A console dump is swapped for its package inside the build (never installed beside it).
  const swap = item.source.startsWith("ps5://") && item.then !== "keep" && !!item.host;
  await conv.start(
    { source: item.source, outputDir: item.outputDir, compression: item.compression, firmware: item.firmware, language: item.language },
    { install: swap, host: swap ? item.host : null, method: item.then === "upload" ? "upload" : "stream" },
  );
  await whenIdle();
  const p = useFpkgConversion.getState().pipeline;
  if (p.phase === "done") return { ok: true, packagePath: p.packagePath, installed: swap };
  if (p.phase === "failed") return { ok: false, message: p.message };
  return { ok: false, message: "The build did not start." };
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
  setConvertRunner({ build, handOff, cancel: () => useFpkgConversion.getState().cancel() });
}
