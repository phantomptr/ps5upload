// Convert a game folder or mount image (.exfat / .ffpkg) into an installable debug FPKG, and
// optionally install it, as three guided cards: ① Game, ② Options, ③ Build & install.
//
// The work runs in the engine (desktop, Docker or Android); the console is only involved at the
// install. `state/fpkgConversion` drives the run; this screen holds the source, its check and the
// remembered options, and hands every action the exact package the run names.

import { useMakeWay } from "../../lib/useMakeWay";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { PackagePlus } from "lucide-react";

import { fpkg, type FpkgEstimates, type FpkgInspection } from "../../api/fpkg";
import { appLaunch } from "../../api/ps5";
import { Callout, Card, PageHeader } from "../../components";
import { FakeGameFirmwareNotice } from "../../components/FakeGameFirmwareNotice";
import { hostOf, transferAddr } from "../../lib/addr";
import { createLatest } from "../../lib/latest";
import { openLocalPath } from "../../lib/openLocalPath";
import { pickPath, pickPaths } from "../../lib/pickPath";
import { isIOS } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useWebviewDrop } from "../../lib/useWebviewDrop";
import { useConnectionStore } from "../../state/connection";
import { useConvertPrefs } from "../../state/convertPrefs";
import { isArchiveSource, useFpkgConversion } from "../../state/fpkgConversion";
import { useUploadStore } from "../../state/upload";
import { DEFAULT_IMAGE_SUBPATH } from "../../lib/imageUpload";
import { useLocation, useNavigate } from "react-router";
import { useTr } from "../../state/lang";
import { pickLocalPath } from "../../state/localPicker";
import { useTaskStore } from "../../state/tasks";
import { GameCard } from "./GameCard";
import { OptionsCard } from "./OptionsCard";
import { RunCard } from "./RunCard";
import { SwapJournals } from "./SwapJournals";
import { QueueCard } from "./QueueCard";
import { useConvertQueue, type ConvertThen } from "../../state/convertQueue";
import { scanChildren } from "../../lib/folderScan";
import { classifyScanEntry } from "../../lib/uploadBatch";
import { usePackageViewer } from "../../state/packageViewer";
import { consoleSwapDeps } from "../../lib/dumpSwapConsole";

/** How long a first press of Delete package stays armed. */
const DELETE_ARM_MS = 5000;

function dirOf(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i > 0 ? path.slice(0, i) : path;
}

export default function FpkgConvertScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host) ?? "";
  const payloadUp = useConnectionStore((s) => s.payloadStatus === "up");
  const canInstall = payloadUp && host.trim() !== "";
  const kernel = useConnectionStore((s) =>
    host ? (s.runtimeByHost[hostOf(host)]?.ps5Kernel ?? null) : null,
  );

  const outputDir = useConvertPrefs((s) => s.outputDir);
  const setOutputDir = useConvertPrefs((s) => s.setOutputDir);
  const compression = useConvertPrefs((s) => s.compression);
  const setCompression = useConvertPrefs((s) => s.setCompression);

  const navigate = useNavigate();
  const pipeline = useFpkgConversion((s) => s.pipeline);
  const start = useFpkgConversion((s) => s.start);
  const compress = useFpkgConversion((s) => s.compress);
  const buildImage = useFpkgConversion((s) => s.buildImage);
  const retryInstall = useFpkgConversion((s) => s.retryInstall);
  const cancel = useFpkgConversion((s) => s.cancel);
  const reset = useFpkgConversion((s) => s.reset);
  const deletePackage = useFpkgConversion((s) => s.deletePackage);
  const finishReplace = useFpkgConversion((s) => s.finishReplace);
  const queueItems = useConvertQueue((s) => s.items);
  const queueRunning = useConvertQueue((s) => s.running);
  const [queueThen, setQueueThen] = useState<ConvertThen>("keep");
  const [queueDeleteAfter, setQueueDeleteAfter] = useState(false);
  // Per game, not remembered: the wrong language carried into the next game would be a surprise.
  const [language, setLanguage] = useState("");
  const queueAdd = (src: string) =>
    useConvertQueue.getState().add({
      source: src,
      outputDir: outputDir.trim() || undefined,
      compression,
      language: language || undefined,
      then: queueThen,
      host: queueThen !== "keep" && canInstall ? host : null,
      deleteAfterInstall: queueThen !== "keep" && queueDeleteAfter,
    });
  // The console's swap journals are read through these; one set per console.
  const swapDeps = useMemo(() => (canInstall ? consoleSwapDeps(host) : null), [canInstall, host]);
  const installTaskId = pipeline.phase === "running" ? pipeline.installTaskId : null;
  const installTask = useTaskStore((s) =>
    installTaskId ? (s.tasks.find((t) => t.id === installTaskId) ?? null) : null,
  );

  const locked = pipeline.phase === "running";
  const [source, setSource] = useState(pipeline.phase === "idle" ? "" : pipeline.source);
  const [inspection, setInspection] = useState<FpkgInspection | null>(null);
  // "pending" while the sample runs; null when it failed or there is no game.
  const [estimates, setEstimates] = useState<FpkgEstimates | "pending" | null>(null);
  // Only the newest check (and its estimate) may land: a slow check of an earlier game must not
  // overwrite the one chosen after it.
  const latest = useRef(createLatest());
  const [checking, setChecking] = useState(false);
  // A .rar's password: held for this source only, never stored.
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [deleteArmed, setDeleteArmed] = useState(false);
  const disarm = useRef<ReturnType<typeof setTimeout> | null>(null);
  const canBrowse = !isIOS() || !isTauriEnv();

  const check = useCallback(
    async (path: string) => {
      if (!path.trim()) return;
      // An archive is looked inside when the run unpacks it; doing it now would mean unpacking
      // it twice. A game on a saved server is inspected in place, like one here.
      if (isArchiveSource(path.trim())) {
        latest.current.begin();
        setInspection(null);
        setEstimates(null);
        setChecking(false);
        setError(null);
        return;
      }
      const token = latest.current.begin();
      setChecking(true);
      setError(null);
      setInspection(null);
      setEstimates(null);
      try {
        const found = await fpkg.inspect(path.trim(), outputDir.trim() || undefined);
        if (!latest.current.isCurrent(token)) return;
        setInspection(found);
        setChecking(false);
        // The estimate is a separate, slower sample: it never holds up the check.
        setEstimates("pending");
        fpkg
          .estimate(path.trim(), outputDir.trim() || undefined)
          .then((e) => latest.current.isCurrent(token) && setEstimates(e))
          .catch(() => latest.current.isCurrent(token) && setEstimates(null));
      } catch (e) {
        if (!latest.current.isCurrent(token)) return;
        setError(e instanceof Error ? e.message : String(e));
        setChecking(false);
      }
    },
    [outputDir],
  );

  /** A new source: the previous result and its buttons go; ignored while a run is going. */
  const chooseSource = useCallback(
    (path: string) => {
      if (useFpkgConversion.getState().pipeline.phase === "running") return;
      reset();
      setDeleteArmed(false);
      setSource(path);
      setPassword("");
      setLanguage("");
      void check(path);
    },
    [check, reset],
  );

  const dropActive = useWebviewDrop(chooseSource, !locked);

  // A game handed over from elsewhere (the viewer's Convert… on a drop).
  const location = useLocation();
  const handed = (location.state as { source?: string } | null)?.source;
  useEffect(() => {
    if (handed) chooseSource(handed);
    // Once per hand-over.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [handed]);

  // The inspection of a restored result's source, so its title id is there for Launch.
  useEffect(() => {
    if (!inspection && source && !checking && pipeline.phase !== "idle") void check(source);
    // Only on mount: later checks come from explicit source changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => () => {
    if (disarm.current) clearTimeout(disarm.current);
  }, []);

  const browse = useCallback(
    async (mode: "file" | "folder") => {
      try {
        const title =
          mode === "folder"
            ? tr("fpkg.pickFolder", undefined, "Choose the game folder")
            : tr("fpkg.pickImage", undefined, "Choose a game image or archive");
        const picked = !isTauriEnv()
          ? await pickLocalPath({ mode, title })
          : await pickPath({
              mode,
              title,
              filters:
                mode === "file" ? [{ name: "Game image or archive", extensions: ["exfat", "ffpkg", "ffpfs", "ffpfsc", "zip", "7z", "rar"] }] : undefined,
            });
        if (picked) chooseSource(picked);
      } catch {
        setError(
          tr(
            "fpkg.noPicker",
            undefined,
            "This build cannot browse this machine — type a path the engine can see.",
          ),
        );
      }
    },
    [chooseSource, tr],
  );

  /** A dump on the console, read in place over its FTP server. */
  const browseConsole = useCallback(async () => {
    const picked = await pickPath({
      mode: "any",
      title: tr("fpkg.pickConsole", undefined, "Choose a game folder or image on the PS5"),
      filters: [{ name: "Game image", extensions: ["exfat", "ffpkg", "ffpfsc"] }],
      source: { console: host },
    });
    if (picked) chooseSource(picked);
  }, [chooseSource, host, tr]);

  const browseOutput = useCallback(async () => {
    try {
      const picked = !isTauriEnv()
        ? await pickLocalPath({ mode: "folder", title: "Choose an output folder on the engine host" })
        : await pickPath({ mode: "folder", title: "Choose an output folder" });
      if (picked) setOutputDir(picked);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [setOutputDir]);

  const run = (install: boolean) => {
    if (!source.trim()) return;
    setError(null);
    void start(
      { source: source.trim(), outputDir: outputDir.trim() || undefined, compression, language: language || undefined },
      { install, host: install ? host : null, password: password || undefined },
    );
  };

  const onDelete = () => {
    if (!deleteArmed) {
      setDeleteArmed(true);
      if (disarm.current) clearTimeout(disarm.current);
      disarm.current = setTimeout(() => setDeleteArmed(false), DELETE_ARM_MS);
      return;
    }
    setDeleteArmed(false);
    void deletePackage().catch((e) => setError(e instanceof Error ? e.message : String(e)));
  };

  const { makeWay, dialog: makeWayDialog } = useMakeWay();
  const onLaunch = () => {
    // The title id of the package this result built, never of whatever the Game card shows.
    const titleId = pipeline.phase === "done" ? pipeline.titleId : null;
    const target = pipeline.phase === "done" ? pipeline.host : null;
    if (!titleId || !target) {
      setError(tr("fpkg.launchUnknown", undefined, "Cannot tell which title to launch; start it from the PS5."));
      return;
    }
    // One game at a time: a running one is closed first, with the user's say-so.
    void makeWay(target, titleId, titleId)
      .then((clear) => (clear ? appLaunch(transferAddr(target), titleId) : undefined))
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  };

  const onAnother = () => {
    reset();
    latest.current.invalidate();
    setDeleteArmed(false);
    setSource("");
    setLanguage("");
    setInspection(null);
    setEstimates(null);
    setChecking(false);
    setError(null);
  };

  // An image here or on a saved server can also become a .ffpfsc; one on the console is
  // converted in place only.
  const isImage = /\.(exfat|ffpkg)$/i.test(source.trim()) && !source.startsWith("ps5://");
  const noFiles = inspection !== null && inspection.files === 0;
  // A game folder on this computer can be written as an image. One on the console or a
  // saved server, an image, or an archive cannot from here (use Convert, or unpack first).
  const src = source.trim();
  const isLocalFolder =
    inspection !== null &&
    !noFiles &&
    !isImage &&
    !isArchiveSource(src) &&
    !/^(ps5|remote):\/\//.test(src) &&
    !/\.(exfat|ffpkg|ffpfs|ffpfsc)$/i.test(src);

  return (
    <div className="app-page flex flex-col gap-4">
      {makeWayDialog}
      <PageHeader
        icon={PackagePlus}
        title={tr("convert_games_title", undefined, "Convert Games")}
        description={tr(
          "convert_games.subtitle",
          undefined,
          "Turn a game folder or game image into an installable package (.pkg), or a game folder into a game image (.exfat, or the smaller .ffpfsc) that ShadowMount+ mounts. The work runs on this computer, not the console.",
        )}
      />

      {/* Building still works (it runs on this computer), but a PS5 fake GAME
          installs and then can't be played on firmware above 11.60. Said
          before the user spends an hour converting; silent for PS4, for
          homebrew and when the firmware is unknown. */}
      <FakeGameFirmwareNotice kernel={kernel} contentId={inspection?.content_id} />

      {/* Wide windows: the game and its options on the left, building and the queue on
          the right, so neither column is a long empty strip. */}
      <div className="grid items-start gap-4 xl:grid-cols-2">
        <div className="flex min-w-0 flex-col gap-4">
          <GameCard
            source={source}
            onSourceTyped={(v) => {
              if (locked) return;
              reset();
              // The old check belongs to the old path: drop it until this one is checked.
              latest.current.invalidate();
              setInspection(null);
              setEstimates(null);
              setChecking(false);
              setSource(v);
              setPassword("");
            }}
            onCheck={() => void check(source)}
            onBrowseFolder={() => void browse("folder")}
            onBrowseImage={() => void browse("file")}
            onRemotePick={chooseSource}
            onBrowseConsole={canInstall ? () => void browseConsole() : undefined}
            onView={
              inspection && source.trim() && !isArchiveSource(source.trim())
                ? () => usePackageViewer.getState().open(source.trim())
                : undefined
            }
            canBrowse={canBrowse}
            inspection={inspection}
            checking={checking}
            locked={locked}
            dropActive={dropActive}
            password={password}
            onPassword={setPassword}
          />

          <OptionsCard
            outputDir={outputDir}
            onChangeOutput={() => void browseOutput()}
            onOutputTyped={setOutputDir}
            canBrowse={canBrowse}
            compression={compression}
            language={language}
            onLanguage={setLanguage}
            onCompression={setCompression}
            estimates={estimates}
            locked={locked}
            plannedSize={inspection?.planned_size}
            outputFree={inspection?.output_free}
          />
        </div>
        <div className="flex min-w-0 flex-col gap-4">

          <SwapJournals
            deps={swapDeps}
            hidden={locked}
            shownTitle={pipeline.phase === "done" && pipeline.swap ? pipeline.swap.titleId : null}
          />

          {error && (
            <Callout tone="error" title={tr("fpkg.error", undefined, "Conversion error")}>
              {error}
            </Callout>
          )}

          <RunCard
            pipeline={pipeline}
            installTask={installTask}
            host={host}
            canInstall={canInstall}
            canConvert={!noFiles && !checking && source.trim() !== ""}
            isImage={isImage}
            deleteArmed={deleteArmed}
            title={inspection?.title ?? null}
            sourceBytes={inspection?.bytes ?? 0}
            onConvert={() => run(false)}
            onConvertInstall={() => run(true)}
            onCompress={() => void compress(source.trim(), outputDir.trim() || undefined)}
            onMakeImage={
              isLocalFolder
                ? (thenCompress, format) =>
                    void buildImage(src, outputDir.trim() || undefined, thenCompress, format)
                : undefined
            }
            onCancel={() => void cancel()}
            onInstall={(method) => void retryInstall(host, method)}
            onLaunch={onLaunch}
            onShowFolder={() => {
              if (pipeline.phase === "done") void openLocalPath(dirOf(pipeline.packagePath));
            }}
            onDelete={onDelete}
            onAnother={onAnother}
            replaces={source.trim().startsWith("ps5://")}
            onViewPackage={() => {
              if (pipeline.phase === "done") usePackageViewer.getState().open(pipeline.packagePath);
            }}
            onUploadImage={(deleteAfter) => {
              if (!host) return;
              const u = useUploadStore.getState();
              useFpkgConversion.getState().uploadImage({
                host,
                volume: u.destinationVolume,
                subpath: u.destinationSubpath || DEFAULT_IMAGE_SUBPATH,
                deleteAfter,
              });
              navigate("/upload");
            }}
            onOpenUpload={() => navigate("/upload")}
            onFinishReplace={(choice) =>
              void finishReplace(choice).catch((e) => setError(e instanceof Error ? e.message : String(e)))
            }
          />

          <QueueCard
            items={queueItems}
            running={queueRunning}
            then={queueThen}
            onThen={setQueueThen}
            deleteAfter={queueDeleteAfter}
            onDeleteAfter={setQueueDeleteAfter}
            canInstall={canInstall}
            canAddCurrent={!!source.trim() && !noFiles}
            onAddCurrent={() => {
              if (!queueAdd(source.trim())) {
                setError(tr("cq_already", undefined, "This game is already in the queue."));
              }
            }}
            onScanFolder={() =>
              void (async () => {
                const folder = !isTauriEnv()
                  ? await pickLocalPath({ mode: "folder", title: tr("batch_scan_pick", undefined, "Choose the folder that holds your games") })
                  : await pickPath({ mode: "folder", title: tr("batch_scan_pick", undefined, "Choose the folder that holds your games") });
                if (!folder) return;
                try {
                  for (const e of await scanChildren(folder)) {
                    const name = e.path.split(/[\\/]/).pop() ?? e.path;
                    const kind = classifyScanEntry(name, e.isDir);
                    if (kind === "folder" || kind === "image" || kind === "archive") queueAdd(e.path);
                  }
                } catch (err) {
                  setError(err instanceof Error ? err.message : String(err));
                }
              })()
            }
            onPickSeveral={() =>
              void (async () => {
                const picked = await pickPaths({
                  title: tr("fpkg.pickImage", undefined, "Choose a game image or archive"),
                  filters: [{ name: "Game image or archive", extensions: ["exfat", "ffpkg", "ffpfs", "ffpfsc", "zip", "7z", "rar"] }],
                }).catch(() => [] as string[]);
                const dupes = picked.filter((p) => !queueAdd(p)).length;
                if (dupes) setError(tr("cq_already", undefined, "This game is already in the queue."));
              })()
            }
            onStart={() => void useConvertQueue.getState().start()}
            onStop={() => useConvertQueue.getState().stop()}
            onRemove={(id) => useConvertQueue.getState().remove(id)}
            onMove={(id, d) => useConvertQueue.getState().move(id, d)}
            onClearFinished={() => useConvertQueue.getState().clearFinished()}
          />
        </div>
      </div>

      <details className="text-sm">
        <summary className="cursor-pointer text-[var(--color-muted)]">
          {tr("fpkg.aboutTitle", undefined, "About converting")}
        </summary>
        <Card>
    <div className="flex flex-col gap-2.5 text-sm text-[var(--color-muted)]">
              <p>
                {tr(
                  "fpkg.about",
                  undefined,
                  "Point it at a game folder, or at an .exfat or .ffpkg mount image. The converter reads the tree, checks that everything a launchable package needs is present, and writes a debug-format .pkg into the output folder.",
                )}
              </p>
              <p>
                {tr(
                  "fpkg.aboutArchives",
                  undefined,
                  "Also accepted: a .ffpfsc image, read through the image inside it, and a .zip, .7z or .rar archive, unpacked into the output folder first. Only .rar archives can have a password.",
                )}
              </p>
              <p>
                {tr(
                  "fpkg.aboutWhere",
                  undefined,
                  "The conversion runs on the machine hosting the engine — this computer, a Docker host or an Android device — not on the console. Nothing reaches the console until you install the package — streamed from this computer, or uploaded to the PS5 first.",
                )}
              </p>
              <p>
                {tr(
                  "fpkg.aboutSource",
                  undefined,
                  "The source has to be a game tree that is already decrypted. A retail install cannot be unwrapped here, and the check below will say so if that is what you picked.",
                )}
              </p>
              <p>
                {tr(
                  "fpkg.aboutFake",
                  undefined,
                  "The result is a fake package, so the console needs fake-package support loaded before it will install: kstuff (the build with PS5 fake-package support), a53_ppr_install_fast.elf and shadowmountplus.elf, in that order. Install Package states the same thing next to its Install button.",
                )}
              </p>
              <details>
                <summary className="cursor-pointer">
                  {tr("fpkg.aboutChecks", undefined, "What the check looks for")}
                </summary>
                <ul className="mt-1.5 flex list-disc flex-col gap-1 pl-5">
                  <li>
                    {tr(
                      "fpkg.checkEboot",
                      undefined,
                      "An eboot.bin at the root of the tree — the title module. It has to be a raw ELF or a wrapped SELF; a retail-signed module cannot be repackaged.",
                    )}
                  </li>
                  <li>
                    {tr(
                      "fpkg.checkParam",
                      undefined,
                      "sce_sys/param.json, and no sce_sys/param.sfo — a param.sfo sends the console's launch path down the PS4 route.",
                    )}
                  </li>
                  <li>
                    {tr(
                      "fpkg.checkIcons",
                      undefined,
                      "A 36-character content id, both icons, and the rights module sce_sys/about/right.sprx.",
                    )}
                  </li>
                  <li>
                    {tr(
                      "fpkg.checkDrm",
                      undefined,
                      "That the package's DRM value is standard. A free or upgradable value makes the console lock the title; it is rewritten in the package only, and your own file is never touched.",
                    )}
                  </li>
                </ul>
              </details>
            </div>
        </Card>
      </details>
    </div>
  );
}
