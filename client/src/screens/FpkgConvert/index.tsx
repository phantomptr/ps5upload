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

import { fpkg, type AmprLz4Options, type FpkgEstimates, type FpkgInspection, type ZstdPacks } from "../../api/fpkg";
import { appLaunch } from "../../api/ps5";
import { Button, Callout, Card, PageHeader } from "../../components";
import { transferAddr } from "../../lib/addr";
import { createLatest } from "../../lib/latest";
import { openLocalPath } from "../../lib/openLocalPath";
import { pickPath } from "../../lib/pickPath";
import { isIOS } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";
import { engineIsOnThisDevice } from "../../state/engine";
import { profileNameForHost, useRosterStore } from "../../state/roster";
import { useWebviewDropAll } from "../../lib/useWebviewDrop";
import { useConnectionStore } from "../../state/connection";
import { useConvertPrefs } from "../../state/convertPrefs";
import {
  archiveIsUnpacked,
  imageFormatOf,
  isArchiveSource,
  isFolderableSource,
  isInPlaceArchive,
  makesFolder,
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
import { displayPath, isRemotePath, parseRemotePath } from "../../lib/remotePath";
import { useConnectionsStore } from "../../state/connections";
import { MakeCard } from "./MakeCard";
import { mergeRole, Ps4MergePicker } from "./Ps4Merge";
import { OptionsCard } from "./OptionsCard";
import { folderLz4Options, folderPackOptions, folderPacks, packOptions, type UnpackChoice } from "./ImageOptions";
import {
  DeleteSourceOption,
  FreeArchiveOption,
  RemoveBackportOption,
  backportRefusal,
  deleteSourceRefusal,
} from "./SourceOptions";
import { useConfirm } from "../../components/ConfirmDialog";
import {
  OutputChoice,
  RunCard,
  idleRun,
  lz4Active,
  savesToPs5,
  savesToServer,
  type Lz4Choice,
  type RunCardProps,
  type SaveTo,
} from "./RunCard";
import { outputFor } from "../../lib/outputPlace";
import { SwapJournals } from "./SwapJournals";
import { nameOf, QueueCard } from "./QueueCard";
import { isRarLaterVolume } from "../../lib/rarVolumes";
import { AmprEmuTool, amprEmuOffered } from "./AmprEmuTool";
import { ZstdRuntimeTool, zstdRuntimeOffered } from "./ZstdRuntimeTool";
import { AmprIndexTool, amprIndexPlan } from "./AmprIndexTool";
import {
  archiveSurelyUnpacked,
  imageBuildWith,
  buildExtension,
  startAfterBuild,
  thenFor,
  useConvertQueue,
  type ConvertBuild,
  type ConvertItem,
  type ConvertThen,
} from "../../state/convertQueue";
import { gameFoldersAmong, gamesInFolder } from "../../lib/folderScan";
import { takeHandOver } from "../../lib/convertFirst";
import { sourcesForPath, withoutBuilding } from "../../lib/convertQueueSources";
import { usePackageViewer } from "../../state/packageViewer";
import { consoleSwapDeps } from "../../lib/dumpSwapConsole";
import { amprTrace, titleIdOf } from "../../api/amprTrace";
import { krakenIsTheMounter } from "../../lib/mounter";
import { placePkgForMount } from "../../state/pkgMount";
import { IDLE_MOUNT_RUN, mountablePackage, mountRunKey, runMount, type MountRun } from "./mountRun";
import { useSmpState } from "../GameHub/SmpNeeded";
import { useMountClashAsk } from "../InstallPackage/KrakenMountCard";

/** Why a source was not queued: the build picked cannot be made from it. */
type QueueSkip = "not-image" | "not-folder" | "later-part";

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
  // Where an image goes (#673): the Upload screen's own choice, for one place to pick it.
  const destVolume = useUploadStore((s) => s.destinationVolume);
  const destSubpath = useUploadStore((s) => s.destinationSubpath);
  const profiles = useRosterStore((s) => s.profiles);
  const payloadUp = useConnectionStore((s) => s.payloadStatus === "up");
  const canInstall = payloadUp && host.trim() !== "";
  // Kraken Mount on the console: a package made here can be mounted from a drive instead of
  // installed (see lib/mounter).
  const mounterProbe = useSmpState(host);
  const kraken = krakenIsTheMounter(mounterProbe.status === "checking" ? null : mounterProbe.status);
  const [mountRun, setMountRun] = useState<MountRun>(IDLE_MOUNT_RUN);
  const mountClash = useMountClashAsk(host);

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
  // A mount result belongs to one run on one console: cleared when either changes, and a late
  // answer for an old run is dropped.
  const mountKey = mountRunKey(host, pipeline);
  const mountKeyRef = useRef(mountKey);
  useEffect(() => {
    // The same run shown again (the effect runs then too) keeps its state: clearing it put the
    // button back while the copy to the PS5 was still going, so a second copy could start.
    if (mountKeyRef.current === mountKey) return;
    mountKeyRef.current = mountKey;
    setMountRun(IDLE_MOUNT_RUN);
  }, [mountKey]);
  const start =useFpkgConversion((s) => s.start);
  const buildImage = useFpkgConversion((s) => s.buildImage);
  const buildFolder = useFpkgConversion((s) => s.buildFolder);
  const retryInstall = useFpkgConversion((s) => s.retryInstall);
  const cancel = useFpkgConversion((s) => s.cancel);
  const pauseRun = useFpkgConversion((s) => s.pause);
  const resumeRun = useFpkgConversion((s) => s.resume);
  const reset = useFpkgConversion((s) => s.reset);
  const deletePackage = useFpkgConversion((s) => s.deletePackage);
  const finishReplace = useFpkgConversion((s) => s.finishReplace);
  const queueItems = useConvertQueue((s) => s.items);
  const queueRunning = useConvertQueue((s) => s.running);
  const queueStoppedByClose = useConvertQueue((s) => s.stoppedByClose);
  const queueAutoStart = useUploadSettingsStore((s) => s.queueAutoStart);
  // ③'s one choice of what happens after converting, for Convert and the queue alike.
  const [thenChoice, setThen] = useState<ConvertThen>("keep");
  // An image saved on this computer, or written straight to the PS5 (nothing stored here).
  const [saveTo, setSaveTo] = useState<SaveTo>("computer");
  // Save to: a server: the folder there (remembered, as a `remote://` path).
  const savedServerFolder = useConvertPrefs((s) => s.serverDir);
  // Only a folder on a server that is still saved here (the folder is synced between devices,
  // whose saved servers can differ): anything else is no destination, and Run waits for one.
  const connections = useConnectionsStore((s) => s.connections);
  const savedServerId = parseRemotePath(savedServerFolder)?.connectionId;
  const serverFolder = savedServerId && connections.some((c) => c.id === savedServerId) ? savedServerFolder : "";
  const setServerFolder = useConvertPrefs((s) => s.setServerDir);
  const nameOfServer = useConnectionsStore((s) => s.nameOf);
  const [deleteAfter, setDeleteAfter] = useState(false);
  // Per game, not remembered: the wrong language carried into the next game would be a surprise.
  const [language, setLanguage] = useState("");
  // The same, for the next game added while one is being made (#669): the game being made has
  // its language already, and it is not the next game's.
  const [nextLanguage, setNextLanguage] = useState("");
  // ③'s one choice of what to make: a package, or the image picked (format, compressed). The
  // queue builds exactly this, never a package in place of the image picked (#433 report).
  // What to make is remembered (prefs). While a build runs, ④ shows that build as it is, and ②
  // and ③ set what the next game added becomes (#669: they used to stay locked on the running
  // build's choices, so a game folder could not be queued behind a build that makes a folder).
  const make = useConvertPrefs((s) => s.make);
  const setMake = useConvertPrefs((s) => s.setMake);
  const showMake = useConvertPrefs((s) => s.showMake);
  const imageFormat = useConvertPrefs((s) => s.imageFormat);
  const imageCompress = useConvertPrefs((s) => s.imageCompress);
  const setImageChoice = useConvertPrefs((s) => s.setImageChoice);
  // Asset packs (LZ4 or zstd) for this game's image or folder, and the profile it packs with. Per game
  // (they depend on the game's own ampr_emu), so only the game checked here carries them into
  // the queue.
  const [lz4On, setLz4On] = useState(false);
  const [lz4Profile, setLz4Profile] = useState<{ name: string; text: string; measured?: boolean } | null>(null);
  // A game in asset packs written as plain files again; the game's own, off for the next one.
  const [unpackPacks, setUnpackPacks] = useState(false);
  // The same for a package (#933): on for each new game, since a package is installed and read
  // by the console itself; its AMPR emulator stays unless the user leaves it out.
  const [pkgUnpack, setPkgUnpack] = useState(true);
  const [leaveOutEmu, setLeaveOutEmu] = useState(false);
  const [packKind, setPackKind] = useState<"lz4" | "zstd">("lz4");
  const [emuAddedTo, setEmuAddedTo] = useState<string | null>(null);
  const [zstdUpdatedTo, setZstdUpdatedTo] = useState<string | null>(null);
  const [zstdLevel, setZstdLevel] = useState(5);
  const build: ConvertBuild =
    make === "image"
      ? { kind: "image", format: imageFormat, compress: imageCompress }
      : make === "folder"
        ? { kind: "folder" }
        : { kind: "pkg" };
  const then = thenFor(build.kind, thenChoice, canInstall);
  /** A skip says which build the source cannot become: an image needs a game folder, image,
   *  package or archive; a folder needs an image, package or `.zip`/`.7z`/`.rar`, or a game
   *  folder when it is made in packs (`zstd`, or for a folder `lz4`: this game's own choice). */
  const queueAdd = (
    src: string,
    isFolder: boolean,
    lz4?: AmprLz4Options,
    zstd?: ZstdPacks,
    /** Write this game's asset packs out as plain files (the game under ①, when ticked). */
    unpack?: boolean,
    /** With `unpack`, a package's: leave its AMPR emulator out. */
    leaveOutAmprEmu?: boolean,
    /** The packs the game is in ("none": in none), when it is the game checked under ①. */
    packs?: ConvertItem["packs"],
  ): boolean | QueueSkip => {
    // A later part of a multi-part archive is read through its first part.
    if (isRarLaterVolume(src)) return "later-part";
    if (build.kind === "image" && !isFolder && !isFolderableSource(src) && !isArchiveSource(src))
      return "not-image";
    if (
      build.kind === "folder" &&
      !isFolderableSource(src) &&
      !(!isFolder && isInPlaceArchive(src)) &&
      !(isFolder && (zstd || lz4 || unpack))
    )
      return "not-folder";
    const u = useUploadStore.getState();
    const out = outFor(src);
    const prefs = useConvertPrefs.getState();
    const making = useFpkgConversion.getState().pipeline.phase === "running";
    const lang = making ? nextLanguage : language;
    // The archive's space freed as it unpacks: only for the game it was confirmed for under ③,
    // never the others added with it, and only when this build is sure to unpack the archive.
    const frees = freeArchiveFor?.src === src && src === source.trim() && archiveSurelyUnpacked(src, build.kind);
    const added = useConvertQueue.getState().add({
      source: src,
      isDir: isFolder || undefined,
      // The password under ① is this game's; it stays in memory only.
      password: password && src === source.trim() ? password : undefined,
      outputDir: out.outputDir,
      keepBoth: out.keepBoth || undefined,
      compression,
      build:
        build.kind === "image"
          ? { ...imageBuildWith(build, lz4), ...(unpack && !lz4 ? { unpackPacks: true } : {}) }
          : build.kind === "folder" && zstd
            ? { ...build, zstd }
            : build.kind === "folder" && lz4
              ? { ...build, lz4 }
              : build.kind === "folder" && unpack
              ? { ...build, unpackPacks: true }
              : build.kind === "pkg" && unpack
                ? { kind: "pkg", unpackPacks: true, ...(leaveOutAmprEmu ? { leaveOutAmprEmu: true } : {}) }
                : build,
      imageDest:
        build.kind === "image"
          ? { volume: u.destinationVolume, subpath: u.destinationSubpath || DEFAULT_IMAGE_SUBPATH }
          : undefined,
      language: lang || undefined,
      packs,
      then,
      host: then !== "keep" ? host : null,
      deleteAfterInstall: then !== "keep" && deleteAfter,
      fullVerify: prefs.fullVerify,
      imageLevel: build.kind === "image" && build.compress ? prefs.imageLevel : undefined,
      // The AMPR emulator choice as it is now: changing it later is for games added later.
      addAmprEmu: prefs.addAmprEmu,
      amprEmuBuild: prefs.amprEmuBuild,
      // Save to: a server, as chosen now (an image or a game folder only).
      toServer:
        (build.kind === "image" || build.kind === "folder") && savesToServer(runChoice) ? serverFolder : undefined,
      // Only the game it was ticked for under ③, never the others added with it.
      removeBackport: (removesBackport && src === source.trim()) || undefined,
      freeArchive: frees || undefined,
    });
    // The answer went with the queued game: it is asked again for anything else.
    if (added && frees) setFreeArchiveFor(null);
    return added;
  };
  /** Queue what was picked or found, then say what was left out. */
  const queueAll = (sources: { path: string; isDir: boolean }[], notGames: number) => {
    // The game being made right now is not queued again.
    const now = useFpkgConversion.getState().pipeline;
    const { take, building } = withoutBuilding(sources, now.phase === "running" ? now.source : null);
    const results = take.map((x) => queueAdd(x.path, x.isDir));
    const notes: string[] = [];
    if (building) notes.push(tr("cq_building_now", undefined, "This game is being made now."));
    if (notGames)
      notes.push(
        tr("cq_not_games", { count: notGames }, "{count} folder(s) were not games and were skipped."),
      );
    for (const why of ["not-image", "not-folder", "later-part"] as const) {
      const n = results.filter((r) => r === why).length;
      if (n) notes.push(skippedText(why, n));
    }
    if (results.some((r) => r === false)) notes.push(tr("cq_already", undefined, "This game is already in the queue."));
    setError(notes.length ? notes.join(" ") : null);
    // Added while a game is being made (#621): made after it, without pressing Start.
    if (results.some((r) => r === true)) {
      startAfterBuild(useFpkgConversion.getState().pipeline.phase === "running");
      setNextLanguage("");
    }
  };
  /** Why a source was left out of the queue, said for a person. */
  const skippedText = (why: QueueSkip, n: number) =>
    why === "later-part"
      ? tr(
          "cq_skipped_later_part",
          { count: n },
          "Skipped {count}: a later part of a multi-part archive. Add its first part (.part1.rar) instead.",
        )
      : why === "not-image"
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
  // A Workshop job left running by a reload, or started in another window, shows here again.
  useEffect(() => {
    void useFpkgConversion.getState().attachRunning();
  }, []);
  const [source, setSource] = useState(pipeline.phase === "idle" ? "" : pipeline.source);
  const [inspection, setInspection] = useState<FpkgInspection | null>(null);
  // A PS4 add-on's files become a folder: ShadowMount+ mounts games, not add-ons.
  const ps4Addon = inspection?.ps4_kind === "addon";
  // A PS4 game's package is a PS4 one (the engine writes it; a PS5 package needs param.json).
  const ps4 = !!inspection?.ps4_game && !ps4Addon;
  useEffect(() => {
    if (ps4Addon && make !== "folder") setMake("folder");
  }, [ps4Addon, make, setMake]);
  // A PS4 update and its game, merged into one image or folder: the other half of the pair is
  // picked here, and the pair is checked by the engine before anything is made.
  const role = mergeRole(inspection);
  const [mergeOther, setMergeOther] = useState("");
  const [merged, setMerged] = useState<FpkgInspection | null>(null);
  const [mergeError, setMergeError] = useState<string | null>(null);
  const [mergeChecking, setMergeChecking] = useState(false);
  const mergeLatest = useRef(createLatest());
  // "pending" while the sample runs; null when it failed or there is no game.
  const [estimates, setEstimates] = useState<FpkgEstimates | "pending" | null>(null);
  // Only the newest check (and its estimate) may land: a slow check of an earlier game must not
  // overwrite the one chosen after it.
  const latest = useRef(createLatest());
  const [checking, setChecking] = useState(false);
  // A .rar's password: held for this source only, never stored.
  const [password, setPassword] = useState("");
  // "Remove backport libraries": ticked for this source only (the one it names), so another
  // game starts with it off.
  const [removeBackportFor, setRemoveBackportFor] = useState<string | null>(null);
  // "Move the source to the Trash once the output is verified": the one source it was
  // confirmed for, in this window's memory only, and cleared when that conversion starts.
  const [deleteSourceFor, setDeleteSourceFor] = useState<string | null>(null);
  // "Free the archive's space while it unpacks" (the archive is deleted on the way): the one
  // archive it was confirmed for, with what the engine found in it. In this window's memory
  // only, cleared when that archive's run starts or it is queued, and for any other source.
  const [freeArchiveFor, setFreeArchiveFor] = useState<{ src: string; game: "folder" | "image" } | null>(null);
  const { confirm, dialog: confirmDialog } = useConfirm();
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

  /** The same game checked again in place (its index was just rebuilt): what is shown stays
   *  until the new check lands, so nothing under ① blinks away. */
  const recheckQuietly = async (path: string) => {
    const token = latest.current.begin();
    try {
      const found = await fpkg.inspect(path.trim(), outputFor(path.trim(), outputPlace, outputDir).outputDir);
      if (latest.current.isCurrent(token)) setInspection(found);
    } catch {
      // The earlier check stands.
    }
  };

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
      setUnpackPacks(false);
      setPkgUnpack(true);
      setLeaveOutEmu(false);
      setFreeArchiveFor(null);
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
    // archive is a file; a folder is scanned as "Add games from a folder…" scans one (one item
    // per game in it).
    sourcesForPath(path)
      .then((found) => queueAll(found.sources, found.skipped))
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)));
  };

  /** A drop: one game is chosen (or queued while one is being made); several go into the queue,
   *  each looked at as a handed path is. A .pkg is a game here like an image (#716). */
  const dropPaths = (paths: string[]) => {
    if (paths.length === 1) {
      chooseOrQueue(paths[0]);
      return;
    }
    Promise.all(paths.map((p) => sourcesForPath(p)))
      .then((found) =>
        queueAll(
          found.flatMap((f) => f.sources),
          found.reduce((n, f) => n + f.skipped, 0),
        ),
      )
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)));
  };

  const dropActive = useWebviewDropAll(dropPaths, true);

  // A game handed over from elsewhere (the viewer's Convert… on a drop).
  const location = useLocation();
  const handedState = location.state as {
    source?: string;
    sources?: { path: string; isDir: boolean }[];
  } | null;
  const handed = handedState?.source;
  useEffect(() => {
    if (handed && takeHandOver(`${location.key}:one`)) chooseOrQueue(handed);
    // Once per hand-over.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [handed]);
  // Several games handed over (the Collection's selection): straight into the queue, with what
  // to make as Convert has it set.
  const handedMany = handedState?.sources;
  useEffect(() => {
    if (handedMany && handedMany.length > 0 && takeHandOver(`${location.key}:many`)) queueAll(handedMany, 0);
    // Once per hand-over.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [handedMany]);

  // The inspection of a restored result's source, so its title id is there for Launch.
  useEffect(() => {
    // Not a source the run moved to the Trash: there is nothing left to inspect.
    const gone = pipeline.phase === "done" && pipeline.sourceNote?.removed;
    if (!inspection && source && !checking && pipeline.phase !== "idle" && !gone) void check(source);
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

  /** The output folder picker: null when it was closed, or could not open (said under ④). */
  const pickOutput = useCallback(async (): Promise<string | null> => {
    try {
      const picked = !isTauriEnv()
        ? await pickLocalPath({
            mode: "folder",
            title: tr("fpkg.pickOutputHost", undefined, "Choose an output folder on the engine host"),
          })
        : await pickPath({ mode: "folder", title: tr("fpkg.pickOutput", undefined, "Choose an output folder") });
      return picked || null;
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      return null;
    }
  }, [tr]);
  const browseOutput = useCallback(async () => {
    const picked = await pickOutput();
    if (picked) setOutputDir(picked);
  }, [pickOutput, setOutputDir]);

  /** Convert the current game as chosen under ③. */
  const run = () => {
    if (!source.trim()) return;
    setError(null);
    // With an update merged in, the game is what is made and the update goes into it.
    const game = mergeReady && mergePair ? mergePair.game : src;
    const update = mergeReady && mergePair ? mergePair.update : undefined;
    const out = outFor(game);
    if (build.kind === "folder") {
      // Save to: a server writes it there; nothing is kept here.
      const toServer = savesToServer(runChoice) ? serverFolder : undefined;
      void buildFolder(game, out.outputDir, out.keepBoth, {
        zstd: folderPackOptions(lz4Choice, packProfile),
        lz4: folderLz4Options(lz4Choice, packProfile),
        update,
        ...(toServer ? { toServer } : {}),
        ...(removesBackport ? { removeBackport: true } : {}),
        ...(deletesSource ? { deleteSource: true } : {}),
        ...(unpackOn ? { unpackPacks: true } : {}),
        // A .rar read in place needs its password as an image build does.
        ...(password ? { password } : {}),
        ...(freesArchive ? { freeArchive: true } : {}),
      });
      // Asked for this one conversion: off again for whatever comes next.
      setDeleteSourceFor(null);
      setFreeArchiveFor(null);
      return;
    }
    if (build.kind === "image") {
      const u = useUploadStore.getState();
      const plan = {
        host,
        volume: u.destinationVolume,
        subpath: u.destinationSubpath || DEFAULT_IMAGE_SUBPATH,
        deleteAfter,
      };
      // Save to: PS5 writes it there as it is built; nothing is kept here to upload after.
      const direct = savesToPs5(runChoice);
      // Save to: a server writes it there as it is built, the same way.
      const toServer = !direct && savesToServer(runChoice) ? serverFolder : undefined;
      void buildImage(
        game,
        out.outputDir,
        build.compress,
        build.format,
        !direct && !toServer && then === "upload" ? plan : undefined,
        packOptions(lz4Choice, packProfile),
        password || undefined,
        out.keepBoth,
        direct || toServer || update || removesBackport || deletesSource || unpackOn || freesArchive
          ? {
              ...(direct ? { toConsole: plan } : {}),
              ...(toServer ? { toServer } : {}),
              ...(update ? { update } : {}),
              ...(removesBackport ? { removeBackport: true } : {}),
              ...(deletesSource ? { deleteSource: true } : {}),
              ...(unpackOn ? { unpackPacks: true } : {}),
              ...(freesArchive ? { freeArchive: true } : {}),
            }
          : undefined,
      );
      setDeleteSourceFor(null);
      setFreeArchiveFor(null);
      return;
    }
    const install = then !== "keep";
    void start(
      {
        // A PS4 game with its update merged in: the game is the source of the PS4 package.
        source: update ? game : source.trim(),
        outputDir: out.outputDir,
        compression,
        language: language || undefined,
        keepBoth: out.keepBoth,
        update,
        ...(pkgUnpackOn && !update
          ? { unpackPacks: true, ...(pkgLeavesOutEmu ? { leaveOutAmprEmu: true } : {}) }
          : {}),
      },
      {
        install,
        host: install ? host : null,
        method: then === "upload" ? "upload" : "stream",
        password: password || undefined,
        deleteAfter: install && deleteAfter,
        ...(freesArchive ? { freeArchive: true } : {}),
      },
    );
    setFreeArchiveFor(null);
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
    setUnpackPacks(false);
    setPkgUnpack(true);
    setLeaveOutEmu(false);
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
  // Whether the run shown under ④ is the game under ① (a queue run is another game).
  const runIsShown = pipeline.phase === "idle" || pipeline.source.trim() === src;
  const pipelineName = pipeline.phase === "idle" ? null : nameOf(pipeline.source);
  const isLocalFolder = inspection !== null && !noFiles && !isArchiveSource(src);
  // The PS4 game and update to merge, once both are picked: the game is what is made, the update
  // goes into it. `mergeReady` once the engine has checked that they go together.
  const other = mergeOther.trim();
  const mergePair =
    role && other && src ? (role === "patch" ? { game: other, update: src } : { game: src, update: other }) : null;
  const mergeReady = mergePair !== null && !!merged?.ps4_merge;
  // An update alone is no game, and a game whose update was picked waits for the check.
  const mergeBlocks = (role === "patch" || mergePair !== null) && !mergeReady;
  // An archive is not looked inside until the run unpacks it; an image run unpacks it first,
  // as a package run does. An add-on becomes a folder only.
  const canImage = !ps4Addon && (isLocalFolder || (src !== "" && isArchiveSource(src)));
  // A game image or a package can be unpacked into a plain folder, wherever it is; a folder is
  // one already (it can become a folder in zstd packs, below), and an archive is unpacked first.
  const unpackable = src !== "" && isFolderableSource(src);
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
                measured: true,
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
        profileMeasured: !!lz4Profile?.measured,
        onProfile: setLz4Profile,
        kind: packKind,
        onKind: setPackKind,
        zstdLevel,
        onZstdLevel: setZstdLevel,
        ps4: !!inspection?.ps4_game,
        packed: inspection.packs ?? null,
        unpacking: unpackPacks,
      }
    : undefined;
  // The profile the packs are made with. A profile measured from play is written for LZ4 packs
  // (what it keeps loose suits drakmor's ampr_emu), so zstd packs take only one picked by hand.
  const packProfile = packKind === "zstd" && lz4Profile?.measured ? undefined : lz4Profile?.text;
  // A folder in packs (LZ4 or zstd) can be made of a game folder too.
  const packsFolder = folderPacks(lz4Choice);
  // A game in asset packs can be written as plain files again, as an image or a folder, while
  // no pack option is on (packing it again decodes the old packs anyway).
  const packsOn = build.kind === "image" ? lz4Active(lz4Choice) : packsFolder;
  const unpackChoice: UnpackChoice | undefined =
    inspection?.packs && (build.kind === "image" || build.kind === "folder") && !packsOn
      ? { kind: inspection.packs, on: unpackPacks, onToggle: setUnpackPacks }
      : undefined;
  const unpackOn = !!unpackChoice?.on;
  // A package of a packed game (#933): its files plain unless unticked, and under that the
  // choice to leave its AMPR emulator out.
  const pkgUnpackChoice: UnpackChoice | undefined =
    inspection?.packs && build.kind === "pkg" && !inspection.ps4_game
      ? {
          kind: inspection.packs,
          on: pkgUnpack,
          onToggle: setPkgUnpack,
          leaveOutEmulator: { on: leaveOutEmu, onToggle: setLeaveOutEmu },
        }
      : undefined;
  const pkgUnpackOn = !!pkgUnpackChoice?.on;
  const pkgLeavesOutEmu = pkgUnpackOn && leaveOutEmu;
  // Whether what is about to be made holds the game in the packs it is in now, and so carries
  // its own ampr_emu.index as it is. Unpacking it or making new packs writes a fresh index, and
  // a warning about the old one would be about a file that never ships.
  const packsKept =
    !!inspection?.packs && !inspection.ps4_game && (build.kind === "pkg" ? !pkgUnpackOn : !unpackOn && !packsOn);
  // "Remove backport libraries" is for an image or folder of the one game checked, when its
  // fakelib holds any; not with an update merged in (the merged game is another source).
  const backport =
    !mergeReady && (build.kind === "image" || build.kind === "folder") ? (inspection?.backport ?? null) : null;
  // New zstd packs unpack a packed game on the way, and so does unpacking it to plain files:
  // either is what lets its backport go.
  const backportUnpacks =
    unpackOn ||
    ((build.kind === "folder" || build.kind === "image") && lz4Active(lz4Choice) && lz4Choice?.kind === "zstd");
  const removesBackport =
    backport !== null && removeBackportFor === src && !backportRefusal(backport, backportUnpacks);
  // A game folder with an update merged in is a new folder too, and so is a packed one once
  // "Unpack the asset packs" is ticked (that checkbox is under "A game folder"; until it is, the
  // folder would be the same one and the engine refuses it).
  // A .zip, .7z or .rar becomes a folder too: its game read straight out of it.
  const canFolder =
    unpackable ||
    (src !== "" && isInPlaceArchive(src)) ||
    (isLocalFolder && (packsFolder || mergeReady || unpackOn));
  const isFolderSource =
    src !== "" && !unpackable && !isArchiveSource(src) && !packsFolder && !mergeReady && !unpackOn;

  /** Check the picked pair: the merged game, or why the two don't go together. */
  const checkMerge = async (pair = mergePair) => {
    const token = mergeLatest.current.begin();
    setMerged(null);
    setMergeError(null);
    if (!pair) {
      setMergeChecking(false);
      return;
    }
    setMergeChecking(true);
    try {
      const found = await fpkg.inspect(pair.game, outFor(pair.game).outputDir, pair.update);
      if (mergeLatest.current.isCurrent(token)) setMerged(found);
    } catch (e) {
      if (mergeLatest.current.isCurrent(token)) setMergeError(e instanceof Error ? e.message : String(e));
    } finally {
      if (mergeLatest.current.isCurrent(token)) setMergeChecking(false);
    }
  };
  // A new game under ① starts without a pair. A new one only: the effect also runs again each
  // time the screen is shown again, and dropping the pair then let the game be built without
  // its update (Run was no longer held for it).
  const pairedSrc = useRef(src);
  useEffect(() => {
    if (pairedSrc.current === src) return;
    pairedSrc.current = src;
    mergeLatest.current.invalidate();
    setMergeOther("");
    setMerged(null);
    setMergeError(null);
    setMergeChecking(false);
  }, [src]);
  const browseMerge = async () => {
    const picked = await pickPath({
      mode: "any",
      title:
        role === "patch"
          ? tr("fpkg.merge.pick_game", undefined, "Choose the PS4 game this update goes into")
          : tr("fpkg.merge.pick_update", undefined, "Choose a PS4 update for this game"),
      filters: [{ name: "Game image or package", extensions: ["pkg", "exfat", "ffpkg", "ffpfs", "ffpfsc"] }],
    }).catch((e) => {
      setError(e instanceof Error ? e.message : String(e));
      return null;
    });
    if (!picked) return;
    setMergeOther(picked);
    const pair = role === "patch" ? { game: picked, update: src } : { game: src, update: picked };
    void checkMerge(pair);
  };

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
  const folderable = !locked && unpackable;
  const mountable = drives.canMount && !locked && isDriveImage(source) && !source.startsWith("ps5://") && !isRemotePath(source);
  // #621: a libSceAmpr game's ampr_emu.index made again from its files, without converting.
  const indexable = !inside && amprIndexPlan(source, inspection) !== "hidden";
  // Kept on screen after adding it, when the check no longer offers it: it says what it did.
  const emulatable = !inside && (amprEmuOffered(source, inspection) || emuAddedTo === source);
  // A zstd-packed game with an old or missing runtime (ASTRO BOT hung on ampr-emu-ng before 0.3.4).
  const zstdUpdatable = !inside && (zstdRuntimeOffered(source, inspection) || zstdUpdatedTo === source);
  /** A .ffpfsc's index is renewed by making a fresh .ffpfsc of it. */
  const convertAgain = () => {
    if (!src || locked) return;
    setError(null);
    const out = outFor(src);
    showMake("image");
    setThen("keep");
    void buildImage(src, out.outputDir, true, imageFormat, undefined, undefined, undefined, true);
    setTimeout(showRun, 50);
  };
  const tools =
    lookable || mountable || folderable || indexable || emulatable || zstdUpdatable ? (
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
        {emulatable && (
          <AmprEmuTool
            source={source}
            inspection={inspection}
            locked={locked}
            onAdded={() => {
              setEmuAddedTo(source);
              void recheckQuietly(source);
            }}
          />
        )}
        {zstdUpdatable && (
          <ZstdRuntimeTool
            source={source}
            inspection={inspection}
            locked={locked}
            onUpdated={() => {
              setZstdUpdatedTo(source);
              void recheckQuietly(source);
            }}
          />
        )}
        {indexable && (
          <AmprIndexTool
            source={source}
            inspection={inspection}
            locked={locked}
            onConvertAgain={convertAgain}
            onRebuilt={() => void recheckQuietly(source)}
          />
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
      packsKept={packsKept}
      checking={checking}
      locked={locked}
      queueStarts={queueAutoStart}
      dropActive={dropActive}
      password={password}
      onPassword={setPassword}
      tools={
        role && !inside ? (
          <>
            {tools}
            <Ps4MergePicker
              role={role}
              other={mergeOther}
              onOther={(v) => {
                mergeLatest.current.invalidate();
                setMergeOther(v);
                setMerged(null);
                setMergeError(null);
                setMergeChecking(false);
              }}
              onCheck={() => void checkMerge()}
              onBrowse={canBrowse ? () => void browseMerge() : undefined}
              merged={merged}
              error={mergeError}
              checking={mergeChecking}
              locked={locked}
            />
          </>
        ) : (
          tools
        )
      }
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
            "workshop_inside_description_v2",
            undefined,
            "Look inside a game image or package: save files to this computer, or change files and write an edited copy. The image you opened changes only if you choose Apply in place.",
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
    dest: host
      ? {
          host,
          volume: destVolume,
          subpath: destSubpath,
          onChange: (volume, subpath) => useUploadStore.getState().setDestination(volume, subpath),
        }
      : undefined,
    // A folder can't go straight to the PS5: shown as this computer.
    saveTo: make === "folder" && saveTo === "ps5" ? "computer" : saveTo,
    onSaveTo: setSaveTo,
    serverFolder,
    onServerFolder: setServerFolder,
  };
  // "Move the source to the Trash" is for an image or folder made of the game checked; a
  // package is left alone (it has its own steps after the build).
  const deleteSourceOffered = inspection !== null && src !== "" && (build.kind === "image" || build.kind === "folder");
  const deleteSourceBlocked = deleteSourceRefusal({
    engineReason: inspection?.delete_source_refusal,
    archive: isArchiveSource(src),
    merge: mergePair !== null,
    elsewhere: (build.kind === "image" && savesToPs5(runChoice)) || savesToServer(runChoice),
  });
  const deletesSource = deleteSourceOffered && deleteSourceFor === src && !deleteSourceBlocked;
  // "Free the archive's space while it unpacks" is for an archive on this computer. It counts
  // only for the archive it was confirmed for, and only while this run unpacks it to disk (a
  // package always does; an image or folder is made straight from most archives).
  const freeArchiveOffered = isArchiveSource(src) && !isRemotePath(src) && !src.startsWith("ps5://");
  const freesArchive =
    freeArchiveOffered &&
    freeArchiveFor?.src === src &&
    archiveIsUnpacked(src, build.kind, { merge: mergePair !== null, game: freeArchiveFor.game });
  /** Ticking it asks first, naming what would go; unticking needs no question. */
  const askDeleteSource = async (on: boolean) => {
    if (!on) {
      setDeleteSourceFor(null);
      return;
    }
    const ok = await confirm({
      title: tr("fpkg.delete_source.confirm_title", undefined, "Move the source to the Trash afterwards?"),
      message: tr(
        "fpkg.delete_source.confirm_body",
        { path: src },
        "Once the output is written and every byte of it has been read back and checked, this is moved to the Trash or Recycle Bin: {path}. It stays where it is if the conversion fails or is stopped, or if it changed meanwhile.",
      ),
      details: [src],
      confirmLabel: tr("fpkg.delete_source.confirm_yes", undefined, "Move it to the Trash afterwards"),
      destructive: true,
    });
    if (ok) setDeleteSourceFor(src);
  };
  const barRun = idleRun(
    {
      choice: runChoice,
      canConvert: !noFiles && !checking && src !== "" && !mergeBlocks,
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
            // While a game is being made, what is checked under ① is that game, not the next.
            checked={!locked && inspection !== null}
            archive={!locked && isArchiveSource(src)}
            forNext={locked}
            ps4={!locked && ps4}
            kraken={kraken}
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
              // A game's own: nothing to offer until the next game is checked.
              lz4: locked ? undefined : lz4Choice,
            }}
            unpack={locked ? undefined : (unpackChoice ?? pkgUnpackChoice)}
            // An archive is not looked inside until the run unpacks it: its packs aren't known.
            gameChecked={!locked && inspection !== null && !noFiles && !isArchiveSource(src)}
            checking={!locked && checking}
            keepsEmulator={
              !locked &&
              !inspection?.packs &&
              !!inspection?.ampr_packs &&
              inspection.ampr_packs.kind !== "no_ampr" &&
              !inspection.ampr_packs.emulator_missing
            }
            language={locked ? nextLanguage : language}
            onLanguage={locked ? setNextLanguage : setLanguage}
            onCompression={setCompression}
            // These are the game being made's; the next game has none yet.
            estimates={locked ? null : estimates}
            locked={false}
            forNext={locked}
            ps4={!locked && ps4}
            plannedSize={locked ? undefined : inspection?.planned_size}
            outputFree={locked ? undefined : inspection?.output_free}
            compressedFrom={
              !locked && build.kind === "image" && build.compress
                ? inspection?.bytes
                : undefined
            }
          >
            {locked && <OutputChoice {...runChoice} />}
            {!locked && backport && (
              <RemoveBackportOption
                backport={backport}
                unpacks={backportUnpacks}
                checked={removeBackportFor === src}
                onChange={(on) => setRemoveBackportFor(on ? src : null)}
              />
            )}
            {!locked && deleteSourceOffered && (
              <DeleteSourceOption
                path={src}
                refusal={deleteSourceBlocked}
                checked={deleteSourceFor === src}
                onChange={(on) => void askDeleteSource(on)}
              />
            )}
            {!locked && freeArchiveOffered && (
              <FreeArchiveOption
                source={src}
                outputDir={outFor(src).outputDir}
                password={password || undefined}
                kind={build.kind}
                merge={mergePair !== null}
                checked={freeArchiveFor?.src === src}
                onChange={(on, found) => setFreeArchiveFor(on && found ? { src, game: found.game } : null)}
              />
            )}
          </OptionsCard>
          {confirmDialog}

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
            canConvert={!noFiles && !checking && source.trim() !== "" && !mergeBlocks}
            isImage={isImage}
            deleteArmed={deleteArmed}
            // ①'s game is not the one a queue run is making: its name and size say nothing of it.
            title={runIsShown ? (inspection?.title ?? null) : pipelineName}
            sourceBytes={runIsShown ? (inspection?.bytes ?? 0) : 0}
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
                            : ps4
                              ? tr("fpkg.run.what_ps4_pkg", undefined, "a PS4 package (.pkg)")
                              : tr("fpkg.run.what_pkg", undefined, "a package (.pkg)"),
                      title: inspection.title || inspection.content_id || source.trim(),
                      dir: savesToPs5(runChoice)
                        ? tr("fpkg.run.on_ps5", undefined, "the PS5")
                        : savesToServer(runChoice)
                          ? displayPath(serverFolder, nameOfServer)
                          : (outFor(src).outputDir ?? "~/Downloads/fpkgs"),
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
                build.kind === "image"
                  ? packOptions(lz4Choice, packProfile)
                  : build.kind === "folder"
                    ? folderLz4Options(lz4Choice, packProfile)
                    : undefined,
                build.kind === "folder" ? folderPackOptions(lz4Choice, packProfile) : undefined,
                build.kind === "pkg" ? pkgUnpackOn : unpackOn,
                build.kind === "pkg" && pkgLeavesOutEmu,
                // Checked here: the queue shows this game's own pack choices while it waits.
                inspection && !noFiles && !isArchiveSource(source.trim()) && !inspection.ps4_game
                  ? (inspection.packs ?? "none")
                  : undefined,
              );
              if (r === "not-image" || r === "not-folder" || r === "later-part") setError(skippedText(r, 1));
              else if (!r) setError(tr("cq_already", undefined, "This game is already in the queue."));
            }}
            onCompress={() => {
              const out = outFor(src);
              // One way to make a .ffpfsc: the level chosen, numbered when the name is taken.
              void buildImage(src, out.outputDir, true, imageFormatOf(src), undefined, undefined, undefined, out.keepBoth);
            }}
            onCancel={() => void cancel()}
            onCancelAndStop={
              queueRunning && queueItems.some((i) => i.status === "pending")
                ? () => {
                    useConvertQueue.getState().holdNext();
                    void cancel();
                  }
                : undefined
            }
            onPause={() =>
              void pauseRun().catch((e: unknown) =>
                setError(tr("fpkg.pause_failed", { error: e instanceof Error ? e.message : String(e) }, "Pause or resume did not work: {error}")),
              )
            }
            onResume={() =>
              void resumeRun().catch((e: unknown) =>
                setError(tr("fpkg.pause_failed", { error: e instanceof Error ? e.message : String(e) }, "Pause or resume did not work: {error}")),
              )
            }
            onInstall={(method) => void retryInstall(host, method)}
            kraken={kraken}
            onMount={
              kraken && !ps4 && pipeline.phase !== "idle" && mountablePackage(pipeline.packagePath)
                ? () => {
                    const path = pipeline.packagePath ?? "";
                    const key = mountKey;
                    void runMount(
                      () => placePkgForMount(path, host, { onExists: mountClash.onExists }),
                      () => mountKeyRef.current === key,
                      setMountRun,
                    );
                  }
                : undefined
            }
            mountRun={mountRun}
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
              ? tr("workshop_bar_running", { title: runIsShown ? inspection?.title || src : (pipelineName ?? src) }, "Working on {title}…")
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
        stoppedByClose={queueStoppedByClose}
        paused={pipeline.phase === "running" && !!pipeline.paused}
        onOpenUpload={() => navigate("/send")}
        onStart={() => void useConvertQueue.getState().start()}
        onStop={() => useConvertQueue.getState().stop()}
        onRemove={(id) => useConvertQueue.getState().remove(id)}
        onRetryPassword={(id, pw) => useConvertQueue.getState().retryWithPassword(id, pw)}
        onUpdate={(id, change) => void useConvertQueue.getState().update(id, change)}
        canInstall={canInstall}
        host={host}
        canBrowse={canBrowse}
        onPickOutput={pickOutput}
        onMove={(id, d) => useConvertQueue.getState().move(id, d)}
        onClearFinished={() => useConvertQueue.getState().clearFinished()}
      />

      <LocalDrivesCard drives={drives} quiet />
      {mountClash.dialog}

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
                  "Also accepted: a .ffpfsc image, read through the image inside it, and a .zip, .7z, .rar or tar archive (.tar, .tar.zst, .tar.gz, .tar.xz, .tar.bz2) or a .zst file, unpacked into the output folder first. Only .rar archives can have a password.",
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
                  "fpkg.aboutFake_v3",
                  undefined,
                "The result is a fake PS5 package with a plaintext game image, the kind the latest kstuff (fake-package support up to 13.60) and Kraken Mount both take. There are two ways to play it: install it with kstuff loaded, or copy it to a folder Kraken Mount scans to mount it. Upload & Install explains both next to its install buttons.",
                )}
              </p>
              <details>
                <summary className="cursor-pointer">
                  {tr("fpkg.aboutChecks", undefined, "What the check looks for")}
                </summary>
                <ul className="mt-1.5 flex list-disc flex-col gap-1 ps-5">
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
