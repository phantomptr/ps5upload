// Change a game's format before it goes to the PS5: a game folder, image or archive becomes a
// package, a game image or a folder, as guided cards in one column: ① Game (the one place games
// are added: one is checked there, several queue), ② What to make, ③ Options, ④ Build &
// install, ⑤ Queue. Sending a game as it is belongs to Upload & Install (/send); a made game
// that should install or upload is handed to that screen's queue, never sent from here.
//
// The work runs in the engine (desktop, Docker or Android); the console is only involved at the
// install. `state/fpkgConversion` drives the run; this screen holds the source, its check and the
// remembered options, and hands every action the exact package the run names.

import { useMakeWay } from "../../lib/useMakeWay";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { Folder, FolderSearch, HardDrive, Hammer, Send } from "lucide-react";

import { fpkg, type AmprLz4Options, type FpkgEstimates, type FpkgInspection } from "../../api/fpkg";
import { appLaunch } from "../../api/ps5";
import { Button, Callout, Card, PageHeader } from "../../components";
import { FakeGameFirmwareNotice } from "../../components/FakeGameFirmwareNotice";
import { hostOf, transferAddr } from "../../lib/addr";
import { createLatest } from "../../lib/latest";
import { openLocalPath } from "../../lib/openLocalPath";
import { pickPath } from "../../lib/pickPath";
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
import { useUploadSettingsStore } from "../../state/uploadSettings";
import { DEFAULT_IMAGE_SUBPATH } from "../../lib/imageUpload";
import { useLocation, useNavigate, useSearchParams } from "react-router";
import { useTr } from "../../state/lang";
import { pickLocalPath } from "../../state/localPicker";
import { useTaskStore } from "../../state/tasks";
import { GameCard } from "./GameCard";
import { ImageBrowser } from "../LocalImage/ImageBrowser";
import { isDriveImage, LocalDrivesCard, useLocalDrives } from "../LocalImage/LocalDrives";
import { BROWSE_IMAGE_EXTENSIONS } from "../LocalImage/imageTypes";
import { isRemotePath } from "../../lib/remotePath";
import { MakeCard } from "./MakeCard";
import { OptionsCard } from "./OptionsCard";
import { RunCard, idleRun, lz4Active, type Lz4Choice, type RunCardProps } from "./RunCard";
import { outputFor } from "../../lib/outputPlace";
import { SwapJournals } from "./SwapJournals";
import { QueueCard } from "./QueueCard";
import {
  buildExtension,
  startAfterBuild,
  thenFor,
  useConvertQueue,
  type ConvertBuild,
  type ConvertThen,
} from "../../state/convertQueue";
import { gameFoldersAmong, gamesInFolder } from "../../lib/folderScan";
import { usePackageViewer } from "../../state/packageViewer";
import { consoleSwapDeps } from "../../lib/dumpSwapConsole";
import { amprTrace, titleIdOf } from "../../api/amprTrace";

/** Why a source was not queued: the build picked cannot be made from it. */
type QueueSkip = "not-image" | "not-folder";

/** How long a first press of Delete package stays armed. */
const DELETE_ARM_MS = 5000;

function dirOf(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i > 0 ? path.slice(0, i) : path;
}

/** Whether Look Inside can open `path`: a game image or package on the engine's own disk
 *  (the engine reads it in place), not one on a saved server or on the PS5. */
function canLookInside(path: string): boolean {
  const p = path.trim().toLowerCase();
  return (
    !p.startsWith("ps5://") && !isRemotePath(path) && BROWSE_IMAGE_EXTENSIONS.some((ext) => p.endsWith(`.${ext}`))
  );
}

/**
 * Workshop: one game source, and everything that can be done with it on this computer —
 * convert it (a package, an image, a folder), look inside an image or package and write an
 * edited copy, or open an image as a drive (macOS and Linux).
 *
 * Convert Games and Game images were two screens that both began by choosing an image. Now
 * the source is chosen once (① Game); Look Inside swaps the converting steps for the image's
 * files (`?view=inside`), and Close brings them back. Old /convert and /local-image links land
 * here, the latter on Look Inside.
 */
export default function WorkshopScreen() {
  const tr = useTr();
  const [params, setParams] = useSearchParams();
  const inside = params.get("view") === "inside";
  const setInside = (on: boolean) => {
    const next = new URLSearchParams(params);
    if (on) next.set("view", "inside");
    else next.delete("view");
    setParams(next);
  };
  const drives = useLocalDrives();
  const host = useConnectionStore((s) => s.host) ?? "";
  const profiles = useRosterStore((s) => s.profiles);
  const payloadUp = useConnectionStore((s) => s.payloadStatus === "up");
  const canInstall = payloadUp && host.trim() !== "";
  const kernel = useConnectionStore((s) =>
    host ? (s.runtimeByHost[hostOf(host)]?.ps5Kernel ?? null) : null,
  );

  const outputDir = useConvertPrefs((s) => s.outputDir);
  const setOutputDir = useConvertPrefs((s) => s.setOutputDir);
  // In the Output folder, or next to each game's own source (remembered).
  const outputPlace = useConvertPrefs((s) => s.outputPlace);
  const setOutputPlace = useConvertPrefs((s) => s.setOutputPlace);
  /** Where `src`'s output goes, as Options has it set. */
  const outFor = (src: string) => outputFor(src, outputPlace, outputDir);
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
  const queueAutoStart = useUploadSettingsStore((s) => s.queueAutoStart);
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
  const showMake = useConvertPrefs((s) => s.showMake);
  const imageFormat = useConvertPrefs((s) => s.imageFormat);
  const imageCompress = useConvertPrefs((s) => s.imageCompress);
  const setImageChoice = useConvertPrefs((s) => s.setImageChoice);
  useEffect(() => {
    if (pipeline.phase === "running") {
      const building = makesFolder(pipeline.mode) ? "folder" : makesImage(pipeline.mode) ? "image" : "pkg";
      if (building !== make) showMake(building);
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
  /** A skip says which build the source cannot become: an image needs a game folder, image,
   *  package or archive (unpacked first); a folder needs an image or package. */
  const queueAdd = (
    src: string,
    isFolder: boolean,
    lz4?: AmprLz4Options,
  ): boolean | QueueSkip => {
    if (build.kind === "image" && !isFolder && !isFolderableSource(src) && !isArchiveSource(src))
      return "not-image";
    if (build.kind === "folder" && !isFolderableSource(src)) return "not-folder";
    const u = useUploadStore.getState();
    const out = outFor(src);
    return useConvertQueue.getState().add({
      source: src,
      outputDir: out.outputDir,
      keepBoth: out.keepBoth || undefined,
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
    for (const why of ["not-image", "not-folder"] as const) {
      const n = results.filter((r) => r === why).length;
      if (n) notes.push(skippedText(why, n));
    }
    if (results.some((r) => r === false)) notes.push(tr("cq_already", undefined, "This game is already in the queue."));
    setError(notes.length ? notes.join(" ") : null);
    // Added while a game is being made (#621): made after it, without pressing Start.
    if (results.some((r) => r === true)) startAfterBuild(useFpkgConversion.getState().pipeline.phase === "running");
  };
  /** Why a source was left out of the queue, said for a person. */
  const skippedText = (why: QueueSkip, n: number) =>
    why === "not-image"
      ? tr(
          "cq_skipped_not_image",
          { count: n },
          "Skipped {count}: only a game folder, game image, package or archive (.zip, .7z, .rar) can become a game image.",
        )
      : tr(
          "cq_skipped_not_folder",
          { count: n },
          "Skipped {count}: only a game image or package can become a game folder. A folder already is one; an archive can become a package or image.",
        );
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
      // The room left is reported for where this game's output goes.
      const out = outputFor(path.trim(), outputPlace, outputDir).outputDir;
      try {
        const found = await fpkg.inspect(path.trim(), out);
        if (!latest.current.isCurrent(token)) return;
        setInspection(found);
        setChecking(false);
        // The estimate is a separate, slower sample: it never holds up the check.
        setEstimates("pending");
        fpkg
          .estimate(path.trim(), out)
          .then((e) => latest.current.isCurrent(token) && setEstimates(e))
          .catch(() => latest.current.isCurrent(token) && setEstimates(null));
      } catch (e) {
        if (!latest.current.isCurrent(token)) return;
        setError(e instanceof Error ? e.message : String(e));
        setChecking(false);
      }
    },
    [outputDir, outputPlace],
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

  /** One game picked, dropped or handed over: checked under ①, or, while a game is being made
   *  (① cannot change then), into the queue to be made after it (#621). */
  const chooseOrQueue = (path: string) => {
    if (useFpkgConversion.getState().pipeline.phase !== "running") {
      chooseSource(path);
      return;
    }
    // A dropped or handed path does not say whether it is a folder: an image, package or
    // archive is a file, anything else is taken for a game folder.
    queueAll([{ path, isDir: !isFolderableSource(path) && !isArchiveSource(path) }], 0);
  };

  const dropActive = useWebviewDrop(chooseOrQueue, true);

  // A game handed over from elsewhere (the viewer's Convert… on a drop).
  const location = useLocation();
  const handedState = location.state as {
    source?: string;
    sources?: { path: string; isDir: boolean }[];
  } | null;
  const handed = handedState?.source;
  useEffect(() => {
    if (handed) chooseOrQueue(handed);
    // Once per hand-over.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [handed]);
  // Several games handed over (the Collection's selection): straight into the queue, with what
  // to make as Convert has it set.
  const handedMany = handedState?.sources;
  useEffect(() => {
    if (handedMany && handedMany.length > 0) queueAll(handedMany, 0);
    // Once per hand-over.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [handedMany]);

  // The inspection of a restored result's source, so its title id is there for Launch.
  useEffect(() => {
    if (!inspection && source && !checking && pipeline.phase !== "idle") void check(source);
    // Only on mount: later checks come from explicit source changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => () => {
    if (disarm.current) clearTimeout(disarm.current);
  }, []);

  /** Games picked under ①: one becomes the game checked there, several go into the queue. While
   *  a game is being made, ① cannot change, so even one picked game waits in the queue. */
  const addSources = (sources: { path: string; isDir: boolean }[], notGames = 0) => {
    if (sources.length === 0) {
      if (notGames) queueAll([], notGames);
      return;
    }
    if (sources.length === 1 && notGames === 0 && useFpkgConversion.getState().pipeline.phase !== "running") {
      chooseSource(sources[0].path);
      return;
    }
    queueAll(sources, notGames);
  };
  /** Picked folders: the ones that are games (several picked at once are checked first). */
  const addFolders = async (folders: { path: string; isDir: boolean }[]) => {
    if (folders.length === 1) {
      addSources(folders);
      return;
    }
    const { games, skipped } = await gameFoldersAmong(folders.map((f) => f.path));
    addSources(
      games.map((path) => ({ path, isDir: true })),
      skipped,
    );
  };
  /** "Add games from a folder…": the games in it, or the folder when it is one. */
  const scanFolder = async (folder: string) => {
    try {
      const found = await gamesInFolder(folder);
      addSources(found.sources, found.skipped);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  /** A dump on the console, read in place over its FTP server. */
  const browseConsole = async () => {
    const picked = await pickPath({
      mode: "any",
      title: tr("fpkg.pickConsole", undefined, "Choose a game folder or image on the PS5"),
      filters: [{ name: "Game image or package", extensions: ["exfat", "ffpkg", "ffpfs", "ffpfsc", "pkg"] }],
      source: { console: host },
    });
    if (picked) chooseOrQueue(picked);
  };

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
    const out = outFor(src);
    if (build.kind === "folder") {
      void buildFolder(src, out.outputDir, out.keepBoth);
      return;
    }
    if (build.kind === "image") {
      const u = useUploadStore.getState();
      void buildImage(
        src,
        out.outputDir,
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
        password || undefined,
        out.keepBoth,
      );
      return;
    }
    const install = then !== "keep";
    void start(
      {
        source: source.trim(),
        outputDir: out.outputDir,
        compression,
        language: language || undefined,
        keepBoth: out.keepBoth,
      },
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
  // An archive is not looked inside until the run unpacks it; an image run unpacks it first,
  // as a package run does.
  const canImage = isLocalFolder || (src !== "" && isArchiveSource(src));
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

  // ④, and whether it is on screen: while it is scrolled away, a slim bar at the bottom of the
  // window offers its main button (the user asked for the main actions without scrolling).
  const runRef = useRef<HTMLDivElement>(null);
  const [runVisible, setRunVisible] = useState(true);
  useEffect(() => {
    const el = runRef.current;
    if (!el || typeof IntersectionObserver === "undefined") return;
    const io = new IntersectionObserver(([e]) => setRunVisible(e.isIntersecting), { threshold: 0.05 });
    io.observe(el);
    return () => io.disconnect();
  }, [inside]);
  const showRun = () => runRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });

  /** Convert to folder: the whole image or package written out as a plain folder where the user
   *  picks (the image browser's Save, for all of it). The run shows under ④ with its progress
   *  and Cancel; the engine refuses before writing when the destination is short of room, and
   *  numbers the name when a folder of that name is already there. */
  const convertToFolder = async () => {
    if (!src || locked) return;
    let dest: string | undefined;
    if (canBrowse) {
      const title = tr("workshop_to_folder_where", undefined, "Choose where to put the folder");
      try {
        const picked = !isTauriEnv()
          ? await pickLocalPath({ mode: "folder", title })
          : await pickPath({ mode: "folder", title });
        if (!picked) return;
        dest = picked;
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
        return;
      }
    } else {
      dest = outFor(src).outputDir;
    }
    setError(null);
    // This one run, not the user's remembered choice of what to make.
    showMake("folder");
    setThen("keep");
    if (inside) setInside(false);
    void buildFolder(src, dest, true);
    // ④ shows the run: bring it into view once it has rendered.
    setTimeout(showRun, 50);
  };

  // What else this source can be, besides converted: looked inside, written out as a folder, or
  // opened as a drive.
  const lookable = !locked && canLookInside(source);
  const folderable = !locked && canFolder;
  const mountable = drives.canMount && !locked && isDriveImage(source) && !source.startsWith("ps5://") && !isRemotePath(source);
  const tools =
    lookable || mountable || folderable ? (
      <div className="flex flex-wrap gap-2 border-t border-[var(--color-border)] pt-2.5">
        {lookable && !inside && (
          <Button size="sm" variant="secondary" leftIcon={<FolderSearch size={14} />} onClick={() => setInside(true)}>
            {tr("workshop_look_inside", undefined, "Look Inside")}
          </Button>
        )}
        {folderable && (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<Folder size={14} />}
            data-testid="convert-to-folder"
            title={tr(
              "workshop_to_folder_hint",
              undefined,
              "Write the whole image or package as a plain folder on this computer.",
            )}
            onClick={() => void convertToFolder()}
          >
            {tr("workshop_to_folder", undefined, "Convert to folder")}
          </Button>
        )}
        {mountable && (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<HardDrive size={14} />}
            disabled={drives.busy}
            onClick={() => void drives.open(source.trim())}
          >
            {tr("workshop_open_as_drive", undefined, "Open as a Drive")}
          </Button>
        )}
      </div>
    ) : undefined;

  const gameCard = (
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
      onAdd={(picked) => {
        const folders = picked.filter((x) => x.isDir);
        if (folders.length) void addFolders(folders);
        else addSources(picked);
      }}
      onScanFolder={(folder) => void scanFolder(folder)}
      onRemotePick={(path, isDir) => addSources([{ path, isDir }])}
      onPickError={setError}
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
      queueStarts={queueAutoStart}
      dropActive={dropActive}
      password={password}
      onPassword={setPassword}
      tools={tools}
    />
  );

  if (inside) {
    return (
      <div className="app-page flex flex-col gap-3">
        <PageHeader
          icon={Hammer}
          compact
          title={tr("workshop_title", undefined, "Workshop")}
          description={tr(
            "workshop_inside_description",
            undefined,
            "Look inside a game image or package: save files to this computer, or change files and write an edited copy. The image you opened is never changed.",
          )}
        />
        {gameCard}
        <ImageBrowser
          source={canLookInside(source) ? source.trim() : null}
          onChoose={chooseSource}
          onClose={() => setInside(false)}
        />
        <LocalDrivesCard drives={drives} />
      </div>
    );
  }

  const idle = pipeline.phase === "idle";
  const runChoice: RunCardProps["choice"] = {
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
  };
  const barRun = idleRun(
    {
      choice: runChoice,
      canConvert: !noFiles && !checking && src !== "",
      canFolder,
      canImage,
      replaces: src.startsWith("ps5://"),
    },
    tr,
  );

  return (
    <div className="app-page flex flex-col gap-3">
      {makeWayDialog}
      <PageHeader
        icon={Hammer}
        title={tr("workshop_title", undefined, "Workshop")}
        compact
        description={tr(
          "workshop_description_v3",
          { send: tr("send_title_v2", undefined, "Upload & Install") },
          "Make a package, a game image or a folder from a game, or look inside an image. The work runs on this computer. To send a game as it is, use {send}.",
        )}
        right={
          <Button variant="secondary" size="sm" leftIcon={<Send size={14} />} onClick={() => navigate("/send")}>
            {tr("send_title_v2", undefined, "Upload & Install")}
          </Button>
        }
      />

      {/* Building still works (it runs on this computer), but a PS5 fake GAME
          installs and then can't be played on firmware above 11.60. Said
          before the user spends an hour converting; silent for PS4, for
          homebrew and when the firmware is unknown. */}
      <FakeGameFirmwareNotice kernel={kernel} contentId={inspection?.content_id} />

      {/* One column at every width (the user asked for it): the steps read top to bottom,
          ① to ⑤, and each card takes the window's whole width. */}
      <div className="flex min-w-0 flex-col gap-3">
          {gameCard}

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

          <OptionsCard
            outputDir={outputDir}
            onChangeOutput={() => void browseOutput()}
            onOutputTyped={setOutputDir}
            outputPlace={outputPlace}
            onOutputPlace={setOutputPlace}
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

          <div ref={runRef}>
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
            choice={runChoice}
            onRun={run}
            canImage={canImage}
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
                      dir: outFor(src).outputDir ?? "~/Downloads/fpkgs",
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
              if (r === "not-image" || r === "not-folder") setError(skippedText(r, 1));
              else if (!r) setError(tr("cq_already", undefined, "This game is already in the queue."));
            }}
            onCompress={() => void compress(source.trim(), outFor(src).outputDir)}
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
              navigate("/send");
            }}
            onOpenUpload={() => navigate("/send")}
            onFinishReplace={(choice) =>
              void finishReplace(choice).catch((e) => setError(e instanceof Error ? e.message : String(e)))
            }
          />
          </div>
      </div>

      {/* ④'s main button while ④ is out of view: idle with a game chosen, or running. */}
      {!runVisible && ((idle && src !== "") || locked) && (
        <div
          data-testid="workshop-bar"
          className="sticky bottom-2 z-10 flex flex-wrap items-center gap-2 rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-float)] px-3 py-2 text-sm shadow-[var(--shadow-2)] backdrop-blur"
        >
          <span className="min-w-0 flex-1 truncate text-[var(--color-muted)]">
            {locked
              ? tr("workshop_bar_running", { title: inspection?.title || src }, "Working on {title}…")
              : inspection?.title || src}
          </span>
          {locked ? (
            <Button size="sm" variant="secondary" onClick={showRun}>
              {tr("workshop_bar_show", undefined, "Show progress")}
            </Button>
          ) : (
            <>
              <Button size="sm" variant="secondary" onClick={showRun}>
                {tr("workshop_bar_review", undefined, "Review")}
              </Button>
              <Button size="sm" variant="primary" onClick={run} disabled={!barRun.canRun}>
                {barRun.label}
              </Button>
            </>
          )}
        </div>
      )}

      {/* ⑤ Queue: games added under ① wait here; shown once there is one. */}
      <QueueCard
        items={queueItems}
        running={queueRunning}
        onOpenUpload={() => navigate("/send")}
        onStart={() => void useConvertQueue.getState().start()}
        onStop={() => useConvertQueue.getState().stop()}
        onRemove={(id) => useConvertQueue.getState().remove(id)}
        onUpdate={(id, change) => void useConvertQueue.getState().update(id, change)}
        canInstall={canInstall}
        host={host}
        onMove={(id, d) => useConvertQueue.getState().move(id, d)}
        onClearFinished={() => useConvertQueue.getState().clearFinished()}
      />

      <LocalDrivesCard drives={drives} quiet />

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
                  "fpkg.aboutFake_v2",
                  undefined,
                  "The result is a fake package, so the console needs fake-package support loaded before it will install: kstuff (the build with PS5 fake-package support), a53_ppr_install_fast.elf and shadowmountplus.elf, in that order. Upload & Install says the same next to its install buttons.",
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
