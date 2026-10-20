// Convert a game folder or mount image (.exfat / .ffpkg) into an installable debug FPKG, and
// optionally install it, as four guided cards: ① Game, ② What to make, ③ Options (the
// package's or the image's), ④ Build & install.
//
// The work runs in the engine (desktop, Docker or Android); the console is only involved at the
// install. `state/fpkgConversion` drives the run; this screen holds the source, its check and the
// remembered options, and hands every action the exact package the run names.

import { useMakeWay } from "../../lib/useMakeWay";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { PackagePlus } from "lucide-react";

import { fpkg, type AmprLz4Options, type FpkgEstimates, type FpkgInspection } from "../../api/fpkg";
import { appLaunch } from "../../api/ps5";
import { Callout, Card, PageHeader } from "../../components";
import { FakeGameFirmwareNotice } from "../../components/FakeGameFirmwareNotice";
import { hostOf, transferAddr } from "../../lib/addr";
import { createLatest } from "../../lib/latest";
import { openLocalPath } from "../../lib/openLocalPath";
import { pickPath, pickPaths } from "../../lib/pickPath";
import { isIOS } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";
import { engineIsOnThisDevice } from "../../state/engine";
import { profileNameForHost, useRosterStore } from "../../state/roster";
import { useWebviewDrop } from "../../lib/useWebviewDrop";
import { useConnectionStore } from "../../state/connection";
import { useConvertPrefs } from "../../state/convertPrefs";
import {
  isArchiveSource,
  isFolderableSource,
  makesFolder,
  makesImage,
  useFpkgConversion,
} from "../../state/fpkgConversion";
import { useUploadStore } from "../../state/upload";
import { DEFAULT_IMAGE_SUBPATH } from "../../lib/imageUpload";
import { useLocation, useNavigate } from "react-router";
import { useTr } from "../../state/lang";
import { pickLocalPath } from "../../state/localPicker";
import { useTaskStore } from "../../state/tasks";
import { GameCard } from "./GameCard";
import { MakeCard } from "./MakeCard";
import { OptionsCard } from "./OptionsCard";
import { RunCard, lz4Active, type Lz4Choice } from "./RunCard";
import { SwapJournals } from "./SwapJournals";
import { QueueCard } from "./QueueCard";
import { buildExtension, thenFor, useConvertQueue, type ConvertBuild, type ConvertThen } from "../../state/convertQueue";
import { gameFoldersAmong, gamesInFolder } from "../../lib/folderScan";
import { usePackageViewer } from "../../state/packageViewer";
import { consoleSwapDeps } from "../../lib/dumpSwapConsole";
import { amprTrace, titleIdOf } from "../../api/amprTrace";

/** How long a first press of Delete package stays armed. */
const DELETE_ARM_MS = 5000;

function dirOf(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i > 0 ? path.slice(0, i) : path;
}

export default function FpkgConvertScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host) ?? "";
  const profiles = useRosterStore((s) => s.profiles);
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
  const buildFolder = useFpkgConversion((s) => s.buildFolder);
  const retryInstall = useFpkgConversion((s) => s.retryInstall);
  const cancel = useFpkgConversion((s) => s.cancel);
  const reset = useFpkgConversion((s) => s.reset);
  const deletePackage = useFpkgConversion((s) => s.deletePackage);
  const finishReplace = useFpkgConversion((s) => s.finishReplace);
  const queueItems = useConvertQueue((s) => s.items);
  const queueRunning = useConvertQueue((s) => s.running);
  // ③'s one choice of what happens after converting, for Convert and the queue alike.
  const [thenChoice, setThen] = useState<ConvertThen>("keep");
  const [deleteAfter, setDeleteAfter] = useState(false);
  // Per game, not remembered: the wrong language carried into the next game would be a surprise.
  const [language, setLanguage] = useState("");
  // ③'s one choice of what to make: a package, or the image picked (format, compressed). The
  // queue builds exactly this, never a package in place of the image picked (#433 report).
  // What to make is remembered (prefs), and while a build runs it follows the build: coming
  // back to the screen mid-image must not show it as a package build.
  const make = useConvertPrefs((s) => s.make);
  const setMake = useConvertPrefs((s) => s.setMake);
  const imageFormat = useConvertPrefs((s) => s.imageFormat);
  const imageCompress = useConvertPrefs((s) => s.imageCompress);
  const setImageChoice = useConvertPrefs((s) => s.setImageChoice);
  useEffect(() => {
    if (pipeline.phase === "running") {
      const building = makesFolder(pipeline.mode) ? "folder" : makesImage(pipeline.mode) ? "image" : "pkg";
      if (building !== make) setMake(building);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pipeline.phase]);
  // AMPR LZ4 asset packs for this game's .exfat image, and the profile it packs with. Per game
  // (they depend on the game's own ampr_emu), so only the game checked here carries them into
  // the queue.
  const [lz4On, setLz4On] = useState(false);
  const [lz4Profile, setLz4Profile] = useState<{ name: string; text: string } | null>(null);
  const [lz4InstallRuntime, setLz4InstallRuntime] = useState(false);
  const build: ConvertBuild =
    make === "image"
      ? { kind: "image", format: imageFormat, compress: imageCompress }
      : make === "folder"
        ? { kind: "folder" }
        : { kind: "pkg" };
  const then = thenFor(build.kind, thenChoice, canInstall);
  /** "skipped": an image was asked for a source that is not a game folder here, or a folder
   *  for one that is not an image or package. */
  const queueAdd = (
    src: string,
    isFolder: boolean,
    lz4?: AmprLz4Options,
  ): boolean | "skipped" => {
    if (build.kind === "image" && !isFolder && !isFolderableSource(src)) return "skipped";
    if (build.kind === "folder" && !isFolderableSource(src)) return "skipped";
    const u = useUploadStore.getState();
    return useConvertQueue.getState().add({
      source: src,
      outputDir: outputDir.trim() || undefined,
      compression,
      build: build.kind === "image" && lz4 ? { ...build, lz4 } : build,
      imageDest:
        build.kind === "image"
          ? { volume: u.destinationVolume, subpath: u.destinationSubpath || DEFAULT_IMAGE_SUBPATH }
          : undefined,
      language: language || undefined,
      then,
      host: then !== "keep" ? host : null,
      deleteAfterInstall: then !== "keep" && deleteAfter,
    });
  };
  /** Queue what was picked or found, then say what was left out. */
  const queueAll = (sources: { path: string; isDir: boolean }[], notGames: number) => {
    const results = sources.map((x) => queueAdd(x.path, x.isDir));
    const notes: string[] = [];
    if (notGames)
      notes.push(
        tr("cq_not_games", { count: notGames }, "{count} folder(s) were not games and were skipped."),
      );
    const skipped = results.filter((r) => r === "skipped").length;
    if (skipped) notes.push(skippedText(skipped));
    if (results.some((r) => r === false)) notes.push(tr("cq_already", undefined, "This game is already in the queue."));
    setError(notes.length ? notes.join(" ") : null);
  };
  const skippedText = (n: number) =>
    build.kind === "folder"
      ? tr(
          "cq_folder_sources_only",
          { count: n },
          "Only a game image or a package made with the debug key can become a folder: {count} skipped.",
        )
      : tr(
          "cq_folder_sources_only",
          { count: n },
          "Only a game image or a package made with the debug key can become a folder: {count} skipped.",
        );
  const skippedNote = (n: number) => setError(skippedText(n));
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
      setLz4On(false);
      setLz4Profile(null);
      setLz4InstallRuntime(false);
      void check(path);
    },
    [check, reset, setSource, setPassword],
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
                mode === "file" ? [{ name: "Game image, package or archive", extensions: ["exfat", "ffpkg", "ffpfs", "ffpfsc", "pkg", "zip", "7z", "rar"] }] : undefined,
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
      filters: [{ name: "Game image or package", extensions: ["exfat", "ffpkg", "ffpfs", "ffpfsc", "pkg"] }],
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

  /** Convert the current game as chosen under ③. */
  const run = () => {
    if (!source.trim()) return;
    setError(null);
    if (build.kind === "folder") {
      void buildFolder(src, outputDir.trim() || undefined);
      return;
    }
    if (build.kind === "image") {
      const u = useUploadStore.getState();
      void buildImage(
        src,
        outputDir.trim() || undefined,
        build.compress && !lz4Active(build.format, lz4Choice),
        build.format,
        then === "upload"
          ? {
              host,
              volume: u.destinationVolume,
              subpath: u.destinationSubpath || DEFAULT_IMAGE_SUBPATH,
              deleteAfter,
            }
          : undefined,
        lz4Active(build.format, lz4Choice)
          ? { profileToml: lz4Profile?.text, installRuntime: lz4InstallRuntime }
          : undefined,
      );
      return;
    }
    const install = then !== "keep";
    void start(
      { source: source.trim(), outputDir: outputDir.trim() || undefined, compression, language: language || undefined },
      {
        install,
        host: install ? host : null,
        method: then === "upload" ? "upload" : "stream",
        password: password || undefined,
        deleteAfter: install && deleteAfter,
      },
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
    setLz4On(false);
    setLz4Profile(null);
    setLz4InstallRuntime(false);
    setInspection(null);
    setEstimates(null);
    setChecking(false);
    setError(null);
  };

  // An image here or on a saved server can also become a .ffpfsc; one on the console is
  // converted in place only.
  const isImage = /\.(exfat|ffpkg)$/i.test(source.trim()) && !source.startsWith("ps5://");
  const noFiles = inspection !== null && inspection.files === 0;
  // A game folder or a game image can be written as an image (an .exfat into a .ffpkg, say):
  // on this computer, on a saved server, or on the console (read through the helper, as a
  // package build reads it). An archive cannot from here: unpack it first.
  const src = source.trim();
  const isLocalFolder = inspection !== null && !noFiles && !isArchiveSource(src);
  // A game image or a package can be unpacked into a plain folder, wherever it is; a folder is
  // one already, and an archive is unpacked first.
  const canFolder = src !== "" && isFolderableSource(src);
  const isFolderSource = src !== "" && !canFolder && !isArchiveSource(src);
  // A profile measured from play (the game page's Measure how it plays) is the LZ4 default
  // for this title; a file picked by hand replaces it.
  const measuredFor = titleIdOf(inspection?.content_id);
  useEffect(() => {
    if (!measuredFor || !inspection?.ampr_packs || inspection.ampr_packs.refusal) return;
    let live = true;
    amprTrace
      .profile(measuredFor)
      .then((p) => {
        if (!live || !p) return;
        setLz4Profile((cur) =>
          cur
            ? cur
            : {
                name: tr("trace_profile_name", { n: p.profile.sessions }, "measured from {n} play sessions"),
                text: p.toml,
              },
        );
      })
      .catch(() => {});
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [measuredFor, inspection?.ampr_packs?.refusal]);
  // Offered only for a game that reads its files through libSceAmpr (the check says so).
  const lz4Choice: Lz4Choice | undefined = inspection?.ampr_packs
    ? {
        readiness: inspection.ampr_packs,
        on: lz4On,
        onToggle: setLz4On,
        profileName: lz4Profile?.name ?? null,
        onProfile: setLz4Profile,
        installRuntime: lz4InstallRuntime,
        onInstallRuntime: setLz4InstallRuntime,
      }
    : undefined;

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

      {/* Wide windows: ① and ② on the left, ③ and ④ on the right, so the steps read left to
          right, top to bottom. */}
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

          <MakeCard
            make={make}
            onMake={(m) => {
              setMake(m);
              if (m === "image" && then === "stream") setThen("upload");
              if (m === "folder") setThen("keep");
            }}
            kernel={kernel}
            contentId={inspection?.content_id}
            checked={inspection !== null}
            locked={locked}
          />

        </div>
        <div className="flex min-w-0 flex-col gap-4">
          <OptionsCard
            outputDir={outputDir}
            onChangeOutput={() => void browseOutput()}
            onOutputTyped={setOutputDir}
            canBrowse={canBrowse}
            compression={compression}
            make={make}
            image={{
              format: imageFormat,
              compress: imageCompress,
              onChoice: (f, c) => setImageChoice(f, c),
              lz4: lz4Choice,
            }}
            language={language}
            onLanguage={setLanguage}
            onCompression={setCompression}
            estimates={estimates}
            locked={locked}
            plannedSize={inspection?.planned_size}
            outputFree={inspection?.output_free}
          />

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
            choice={{
              make,
              onMake: setMake,
              imageFormat,
              imageCompress,
              onImageChoice: (f, c) => setImageChoice(f, c),
              then,
              onThen: setThen,
              deleteAfter,
              onDeleteAfter: setDeleteAfter,
              canInstall,
              lz4: lz4Choice,
            }}
            onRun={run}
            canImage={isLocalFolder}
            canFolder={canFolder}
            isFolderSource={isFolderSource}
            summary={
              inspection
                ? tr(
                    "fpkg.run.summary",
                    {
                      what:
                        build.kind === "image"
                          ? tr("fpkg.run.what_image", { ext: buildExtension(build) }, "a {ext} game image")
                          : build.kind === "folder"
                            ? tr("fpkg.run.what_folder", undefined, "a game folder")
                            : tr("fpkg.run.what_pkg", undefined, "a package (.pkg)"),
                      title: inspection.title || inspection.content_id || source.trim(),
                      dir: outputDir.trim() || "~/Downloads/fpkgs",
                    },
                    "Makes {what} of {title} in {dir}.",
                  )
                : undefined
            }
            canQueue={!!source.trim() && !noFiles}
            onQueue={() => {
              const r = queueAdd(
                source.trim(),
                isLocalFolder,
                build.kind === "image" && lz4Active(build.format, lz4Choice)
                  ? { profileToml: lz4Profile?.text, installRuntime: lz4InstallRuntime }
                  : undefined,
              );
              if (r === "skipped") skippedNote(1);
              else if (!r) setError(tr("cq_already", undefined, "This game is already in the queue."));
            }}
            onCompress={() => void compress(source.trim(), outputDir.trim() || undefined)}
            onCancel={() => void cancel()}
            onInstall={(method) => void retryInstall(host, method)}
            onLaunch={onLaunch}
            targetLabel={
              pipeline.phase === "running" && pipeline.host
                ? profileNameForHost(pipeline.host, profiles)
                : null
            }
            onShowFolder={
              // A file manager can open it only when the engine's disk is this computer's.
              isTauriEnv() && engineIsOnThisDevice()
                ? () => {
                    if (pipeline.phase !== "done") return;
                    // A folder build's result is the folder itself: open it.
                    const dir = makesFolder(pipeline.mode) ? pipeline.packagePath : dirOf(pipeline.packagePath);
                    void openLocalPath(dir).then((ok) => {
                      if (!ok)
                        setError(
                          tr("fpkg.showFolderFailed", { path: dir }, `Couldn't open the folder. The package is in ${dir}.`),
                        );
                    });
                  }
                : undefined
            }
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

        </div>
      </div>

      {/* The queue takes the whole width: its rows are long, and it is the one card that is
          not a step of converting the current game. */}
      <QueueCard
        items={queueItems}
        running={queueRunning}
        onScanFolder={() =>
          void (async () => {
            const folder = !isTauriEnv()
              ? await pickLocalPath({ mode: "folder", title: tr("batch_scan_pick", undefined, "Choose the folder that holds your games") })
              : await pickPath({ mode: "folder", title: tr("batch_scan_pick", undefined, "Choose the folder that holds your games") });
            if (!folder) return;
            try {
              const found = await gamesInFolder(folder);
              queueAll(found.sources, found.skipped);
            } catch (err) {
              setError(err instanceof Error ? err.message : String(err));
            }
          })()
        }
        onPickFolders={() =>
          void (async () => {
            const picked = await pickPaths({
              mode: "folder",
              title: tr("cq_pick_folders", undefined, "Pick game folders…"),
            }).catch(() => [] as string[]);
            if (!picked.length) return;
            const { games, skipped } = await gameFoldersAmong(picked);
            queueAll(
              games.map((path) => ({ path, isDir: true })),
              skipped,
            );
          })()
        }
        onPickFiles={() =>
          void (async () => {
            const picked = await pickPaths({
              title: tr("fpkg.pickImage", undefined, "Choose a game image or archive"),
              filters: [{ name: "Game image, package or archive", extensions: ["exfat", "ffpkg", "ffpfs", "ffpfsc", "pkg", "zip", "7z", "rar"] }],
            }).catch(() => [] as string[]);
            queueAll(
              picked.map((path) => ({ path, isDir: false })),
              0,
            );
          })()
        }
        onStart={() => void useConvertQueue.getState().start()}
        onStop={() => useConvertQueue.getState().stop()}
        onRemove={(id) => useConvertQueue.getState().remove(id)}
        onMove={(id, d) => useConvertQueue.getState().move(id, d)}
        onClearFinished={() => useConvertQueue.getState().clearFinished()}
      />

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
