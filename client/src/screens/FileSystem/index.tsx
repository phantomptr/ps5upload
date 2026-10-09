import { consoleAddr, hostOf } from "../../lib/addr";
import { trStatic } from "../../lib/trStatic";
import { rememberDestination } from "../../lib/moveTo";
import { MoveToDialog, type MoveItem } from "./MoveToDialog";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentType,
} from "react";
import { useNavigate, useSearchParams } from "react-router";
import { usePackageViewer } from "../../state/packageViewer";
import { useShallow } from "zustand/react/shallow";
import {
  FolderTree,
  Folder,
  File as FileIcon,
  ChevronRight,
  Home,
  ArrowUp,
  Trash2,
  Pencil,
  FolderPlus,
  FolderUp,
  RefreshCw,
  Scissors,
  Copy,
  ClipboardPaste,
  Download,
  Upload,
  FileArchive,
  X,
  HardDrive,
  Usb,
  PackagePlus,
  Eye,
  ScanSearch,
  Hash,
  BadgeCheck,
  FolderOpen,
  Link2,
  FolderInput,
  KeyRound,
} from "lucide-react";
import { pickPath, pickPaths } from "../../lib/pickPath";
import { useWebviewDropAll } from "../../lib/useWebviewDrop";
import { writeClipboard } from "../../lib/clipboard";
import {
  fsKeyAction,
  keysBelongElsewhere,
  nextSort,
  normalizeTypedPath,
  sortEntries,
  type SortKey,
  type SortState,
} from "./fsBrowse";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { isTauriEnv } from "../../lib/tauriEnv";
import { isInstallPackagePath } from "../../lib/pkgDropDedupe";
import {
  PageHeader,
  Button,
  ConnectionGate,
  Spinner,
  ErrorCard,
} from "../../components";
import { BrowseButton } from "../../components/BrowseButton";
import EditSessionBanner from "../../components/EditSessionBanner";
// Direct import to avoid the barrel's circular-dep warning at build.
import {
  useConfirm,
  useAlert,
  usePrompt,
} from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";

import { useConnectionStore } from "../../state/connection";
import {
  fsDelete,
  fsMove,
  fsChmod,
  fsMkdir,
  fsCopy,
  fsListDir,
  fsOpStatus,
  fsOpCancel,
  jobStatus,
  startTransferDownload,
  startTransferDownloadZip,
  fetchVolumes,
  type Volume,
} from "../../api/ps5";
import {
  loadFsLastPath,
  saveFsLastPath,
  FS_DEFAULT_PATH,
} from "../../lib/fsLastPath";
import { useFsNavStore } from "../../state/fsNavigation";
import { trackTask } from "../../state/trackTask";
import { useActivityHistoryStore } from "../../state/activityHistory";
import { pushNotification } from "../../state/notifications";
import { usePkgLibrary } from "../../state/pkgLibrary";
import { isRemovableMount } from "../../lib/mountPaths";
import { useRecentPathsStore } from "../../state/recentPaths";
import { useUploadSettingsStore } from "../../state/uploadSettings";
import {
  useFsClipboardStore,
  type ClipboardItem,
} from "../../state/fsClipboard";
import {
  useFsBulkOpStore,
  useFsDownloadOpStore,
  bulkOpForHost,
  otherActiveBulkHost,
  fsBulkOpHandle,
  downloadForHost,
  otherActiveDownloadHost,
  fsDownloadOpHandle,
} from "../../state/fsBulkOp";
import { useElapsed } from "../../lib/useElapsed";
import {
  cancelFsUpload,
  fsUploadForHost,
  runFsUpload,
  dismissFsUploadStopped,
  resumeFsUpload,
  useFsUploadStore,
  type FsUploadActive,
} from "../../state/fsUpload";
import { fsUploadDeps } from "../../state/fsUploadRuntime";
import { useScrollLock } from "../../lib/useScrollLock";
import { runBulkDelete as runBulkDeleteLoop } from "../../lib/bulkDelete";
import { formatBytes } from "../../lib/format";
import { formatEtaSeconds } from "../../lib/uploadEta";
import { useRateEta } from "../../lib/useRateEta";
import { humanizePs5Error } from "../../lib/humanizeError";

/**
 * PS5 file explorer with multi-select + cut/copy/paste.
 *
 * Select model: checkboxes per row, plus a "select all" in the header.
 * Once ≥1 entry is selected a bulk-action toolbar appears (Cut / Copy /
 * Delete) that applies to the whole selection. Cut/Copy seeds the
 * shared clipboard store (see state/fsClipboard.ts); Paste is available
 * from the toolbar whenever the clipboard is non-empty, pasting into
 * the currently-browsed directory.
 *
 * Cross-volume note: fsMove is rename()-based, which only works within one
 * volume. Across mounts (e.g. USB → internal) the payload does NOT attempt the
 * rename — a cross-device rename panics the PS5 kernel — and returns
 * `fs_move_cross_mount`. fsCopy works across mounts because the payload reads +
 * writes bytes explicitly, so the UI falls back to "Copy, then delete" on that
 * error — matching how file managers on Linux/macOS handle cross-filesystem
 * moves.
 */

interface DirEntry {
  name: string;
  kind: string; // "file" | "dir" | "link" | "other" | "unknown"
  size: number;
  /** Seconds since the epoch; older helpers omit it. */
  mtime?: number;
}

function formatDuration(sec: number): string {
  if (!isFinite(sec) || sec < 0) return "—";
  if (sec < 60) return `${Math.ceil(sec)}s`;
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

// formatBytes moved to lib/format.ts.
// toMgmtAddr migrated to lib/addr::mgmtAddr in 2.12.0 — same name
// previously had two different signatures across screens (bare host
// vs transfer-port addr); the canonical helper accepts either.
import { mgmtAddr as toMgmtAddr } from "../../lib/addr";
import {
  profileNameForHost,
  useRosterStore,
  withConsolePrefix,
} from "../../state/roster";
import { useStaleHostGuard } from "../../lib/staleHostGuard";

function parent(p: string): string {
  if (p === "/" || p === "") return "/";
  const i = p.lastIndexOf("/");
  if (i <= 0) return "/";
  return p.slice(0, i);
}

/** Before a cross-drive move deletes an original: why the copy isn't whole, or null when it
 *  is. Files are checked by size; a folder's copy reports its own errors. */
async function copyIsShort(
  addr: string,
  item: ClipboardItem,
  target: string,
): Promise<string | null> {
  if (item.kind !== "file" || !(item.size > 0)) return null;
  const name = target.slice(target.lastIndexOf("/") + 1);
  const entries = await fsListDir(addr, parent(target)).catch(() => null);
  if (!entries) return trStatic("fs_move_check_failed", "it couldn't be checked");
  const copy = entries.find((e) => e.name === name);
  if (!copy) return trStatic("fs_move_check_missing", "it isn't there");
  return copy.size === item.size
    ? null
    : `${formatBytes(copy.size)} / ${formatBytes(item.size)}`;
}

const isViewablePackage = (name: string) => /\.f?pkg$/i.test(name);

/** What the package viewer can show: a package, a game image, or a folder named like a game
 *  (a title id, as dumps are). */
export function viewableEntry(name: string, isDir: boolean): boolean {
  if (isDir) return /^[A-Z]{4}\d{5}/.test(name);
  return isViewablePackage(name) || /\.(exfat|ffpkg|ffpfsc)$/i.test(name);
}

function joinPath(dir: string, name: string): string {
  if (dir.endsWith("/")) return `${dir}${name}`;
  return `${dir}/${name}`;
}

/** Apply a small DSL to a single name. Returns the new name, or
 *  the original name unchanged when the pattern doesn't match.
 *
 *  Grammar:
 *    s/old/new/         first-match substring replace
 *    s/old/new/g        global substring replace
 *    s/old/new/i        case-insensitive (combine with g: gi or ig)
 *    ^prefix_           prepend literal prefix
 *    _suffix$           append literal suffix (before file extension)
 *    lower              lowercase entire name
 *    upper              uppercase entire name
 *
 *  Deliberately not regex-based: PS5 filenames (e.g. CUSA00001-Backup.zip)
 *  contain regex metacharacters by default, and exposing regex to a
 *  bulk-rename UI is a sharp footgun. */
export function applyRenamePattern(name: string, pattern: string): string {
  const p = pattern.trim();
  if (!p) return name;

  // s/old/new/[flags] — split manually so old/new can contain `/`.
  if (p.startsWith("s/")) {
    // Find unescaped slashes; we don't support escapes, so look for
    // the first `/` after position 2 and the next one after that.
    const firstSlash = 2;
    const secondSlash = p.indexOf("/", firstSlash);
    if (secondSlash < 0) return name;
    const thirdSlash = p.indexOf("/", secondSlash + 1);
    if (thirdSlash < 0) return name;
    const old = p.slice(firstSlash, secondSlash);
    const next = p.slice(secondSlash + 1, thirdSlash);
    const flags = p.slice(thirdSlash + 1).toLowerCase();
    if (!old) return name;
    const global = flags.includes("g");
    const insensitive = flags.includes("i");
    if (insensitive) {
      // No regex — manual case-insensitive find loop.
      const lower = name.toLowerCase();
      const lowerOld = old.toLowerCase();
      let out = "";
      let i = 0;
      while (i < name.length) {
        if (lower.startsWith(lowerOld, i)) {
          out += next;
          i += old.length;
          if (!global) {
            out += name.slice(i);
            return out;
          }
        } else {
          out += name[i];
          i++;
        }
      }
      return out;
    }
    return global ? name.split(old).join(next) : name.replace(old, next);
  }

  if (p.startsWith("^") && p.length > 1) {
    return p.slice(1) + name;
  }
  if (p.endsWith("$") && p.length > 1) {
    const suffix = p.slice(0, -1);
    const dot = name.lastIndexOf(".");
    if (dot > 0) return name.slice(0, dot) + suffix + name.slice(dot);
    return name + suffix;
  }
  if (p === "lower") return name.toLowerCase();
  if (p === "upper") return name.toUpperCase();
  return name;
}

export function isSinglePathName(name: string): boolean {
  return (
    name.trim() !== "" &&
    name === name.trim() &&
    !name.includes("/") &&
    !name.includes("\\") &&
    name !== "." &&
    name !== ".."
  );
}

function crumbs(p: string): { label: string; path: string }[] {
  const parts = p.split("/").filter(Boolean);
  const out: { label: string; path: string }[] = [{ label: "/", path: "/" }];
  let cur = "";
  for (const part of parts) {
    cur += `/${part}`;
    out.push({ label: part, path: cur });
  }
  return out;
}

export default function FileSystemScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const guard = useStaleHostGuard();
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const clipboard = useFsClipboardStore();
  const systemFileRead = useUploadSettingsStore((s) => s.systemFileRead);
  // Install a .pkg straight from where it sits in the browser — saves the
  // round-trip through the Install Package screen's drive scan.
  const installFromConsolePath = usePkgLibrary(
    host,
    (s) => s.installFromConsolePath,
  );
  const pkgInstalling = usePkgLibrary(host, (s) => s.installing);
  const [installingPkgName, setInstallingPkgName] = useState<string | null>(
    null,
  );
  // Restore the user's last-browsed path for THIS host. The lazy
  // initializer reads from localStorage once on mount; later host
  // changes (the user typing a new IP into the Connection screen)
  // are handled by the effect below.
  const [searchParams, setSearchParams] = useSearchParams();
  const [path, setPath] = useState<string>(() => loadFsLastPath(host));
  // Refetched whenever host or payloadStatus changes. Drives the
  // volume picker dropdown — same shape Library's Move modal uses.
  // null while loading or before the first probe completes; an empty
  // array means the probe ran but the PS5 has no writable volumes
  // (rare on stock PS5, but possible after a user mounts and then
  // unmounts everything). Errors are swallowed silently — the picker
  // just hides itself, which is less noisy than yet another red
  // banner for what's a degraded-but-functional state.
  const [volumes, setVolumes] = useState<Volume[] | null>(null);
  const [entries, setEntries] = useState<DirEntry[] | null>(null);
  // Path of the most recent listing request — used to drop a stale (slower)
  // response from a folder the user already navigated away from. See refresh.
  const latestListReqRef = useRef<string>("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [mkdirDraft, setMkdirDraft] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [localBusy, setBusyEntry] = useState<{
    name: string;
    op: "rename" | "mkdir";
  } | null>(null);
  // An "Add files" / drag-in upload belongs to the console, not this screen (see
  // state/fsUpload): it keeps running, and stays visible here, across navigation. Narrow
  // selectors on purpose: the byte counters tick twice a second and only the progress
  // detail below reads them.
  const uploadName = useFsUploadStore(
    (s) => fsUploadForHost(s, host).active?.name ?? null,
  );
  const uploadStartedAtMs = useFsUploadStore(
    (s) => fsUploadForHost(s, host).active?.startedAtMs ?? 0,
  );
  const uploadStopped = useFsUploadStore(
    (s) => fsUploadForHost(s, host).stopped,
  );
  const uploadRuns = useFsUploadStore(
    (s) => fsUploadForHost(s, host).finishedRuns,
  );
  const stoppedAt = {
    name: uploadStopped?.name ?? "",
    done: uploadStopped?.doneCount ?? 0,
    count: uploadStopped?.count ?? 0,
  };
  const busyEntry: { name: string; op: "rename" | "mkdir" | "upload" } | null =
    localBusy ??
    (uploadName !== null ? { name: uploadName, op: "upload" } : null);
  // Lifted into Zustand so the in-flight bulk op survives navigation.
  // The async runner writes to the store; the screen reads from it.
  // Re-mount after a tab switch sees the still-running operation.
  //
  // Deliberately narrowed selectors (NOT whole-store subscriptions):
  // the paste loop stamps `currentBytesCopied` every 500 ms and the
  // download poller stamps `bytesReceived` at poll rate. A whole-store
  // subscription made this 2000+ line screen re-render on every one of
  // those ticks. The per-tick fields are consumed by BulkOpBanner /
  // DownloadOpBanner via their own narrow subscriptions, so the screen
  // body only needs the slow-moving fields below.
  // Per-console bulk-op / download state: select THIS console's slot from
  // the per-host stores. Each console's op is fully independent, so the
  // screen always renders its own host's progress (never another console's).
  const bulkOp = useFsBulkOpStore(
    useShallow((s) => {
      const b = bulkOpForHost(s, host);
      return {
        op: b.op,
        total: b.total,
        done: b.done,
        currentName: b.currentName,
        currentSize: b.currentSize,
        fromPath: b.fromPath,
        toPath: b.toPath,
        startedAtMs: b.startedAtMs,
        errorBanner: b.errorBanner,
      };
    }),
  );
  const downloadOp = useFsDownloadOpStore(
    useShallow((s) => {
      const d = downloadForHost(s, host);
      return {
        active: d.active,
        rootName: d.rootName,
        rootSrcPath: d.rootSrcPath,
        destDir: d.destDir,
        startedAtMs: d.startedAtMs,
        errorBanner: d.errorBanner,
      };
    }),
  );
  const rosterProfiles = useRosterStore((s) => s.profiles);
  // "Here" is now always this console's own slot (selected above). The
  // "elsewhere" indicators surface that ANOTHER console has an op running,
  // so its tab shows a busy badge and switching to it reveals progress —
  // but it never hijacks this console's banner. Consoles run concurrently
  // (no app-wide single-flight anymore).
  const bulkOpHere = bulkOp.op !== null;
  const bulkOpElsewhereHost = useFsBulkOpStore((s) =>
    otherActiveBulkHost(s, host),
  );
  const bulkOpElsewhere = bulkOpElsewhereHost !== null;
  const downloadHere = downloadOp.active;
  const downloadElsewhereHost = useFsDownloadOpStore((s) =>
    otherActiveDownloadHost(s, host),
  );
  const downloadElsewhere = downloadElsewhereHost !== null;
  // Host-bound handles for the async runners + banner controls. Bound to
  // the host of the render they're created in; an onClick captures that
  // render's handle, so an op keeps writing to the console it started on
  // even if the user switches tabs mid-run.
  const fsBulk = fsBulkOpHandle(host);
  const fsDownload = fsDownloadOpHandle(host);
  // Custom confirm modal — replaces window.confirm() which is a
  // no-op in Tauri webview and falls through to plugin-dialog's
  // ACL-gated confirm command.
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();
  const { alert: alertDialog, dialog: alertDialogNode } = useAlert();
  const { prompt: promptDialog, dialog: promptDialogNode } = usePrompt();
  const elapsedMs = useElapsed(
    busyEntry !== null || bulkOp.op !== null || downloadOp.active,
  );
  // Note: no mountedRef needed for the download / bulk-op runners
  // anymore — both write to global Zustand stores, so an unmount
  // mid-run is harmless. The runner loops keep going (writing to
  // the store), the screen re-mount sees the in-flight progress.

  const refresh = useCallback(async () => {
    if (!host?.trim()) return;
    // Stale-result guard (2.9.0). The user can navigate (changes
    // `path`) or switch PS5 (changes `host`) faster than ps5_list_dir
    // resolves on a deep `/data` tree — listings can take 1-3s. The
    // OLDER request's response, arriving SECOND, would otherwise
    // clobber state and the UI would display the old directory's
    // contents under the new URL. Worse: the per-row Delete handler
    // joins the CURRENT `path` with the displayed name, so the user
    // clicking Delete on a name they think belongs to `/data/foo`
    // would actually delete `/data/homebrew/foo` if a coincidentally-
    // named file exists there. Capture inputs at the top, re-read
    // store state after the await, drop the result if anything
    // changed. `path` is captured directly because it's the closure
    // value at refresh start; `host` we re-read because it lives in
    // a global store.
    // 2.12.0: canonical useStaleHostGuard for the host check;
    // probedPath is path-specific and stays inline (no general
    // "stale path" helper since path semantics are screen-local).
    const probe = guard.capture();
    const probedPath = path;
    // Mark this as the latest in-flight listing request. The previous
    // `path !== probedPath` check was dead code — both sides were the same
    // closure constant — so a fast A→B navigation let A's slower response
    // resolve last and paint A's contents under path B (and per-row Delete
    // then targets B/<name>). Comparing the closure's path against this ref
    // after the await actually drops the superseded request.
    latestListReqRef.current = probedPath;
    setLoading(true);
    setError(null);
    try {
      const listing = await fsListDir(probe.host, probedPath);
      // Drop result if a newer listing (different path) superseded this one.
      if (latestListReqRef.current !== probedPath) return;
      if (probe.isStale()) return;
      const raw: DirEntry[] = listing;
      raw.sort((a, b) => {
        if (a.kind !== b.kind) {
          if (a.kind === "dir") return -1;
          if (b.kind === "dir") return 1;
        }
        return a.name.localeCompare(b.name);
      });
      setEntries(raw);
      // Prune selections for items that no longer exist (e.g. after a
      // refresh that followed a delete). Selections keyed by name are
      // cheap to re-validate.
      setSelected((s) => {
        const names = new Set(raw.map((e) => e.name));
        const next = new Set<string>();
        for (const n of s) if (names.has(n)) next.add(n);
        return next;
      });
    } catch (e) {
      // Same stale-guard on the error path: a failure for an
      // abandoned listing shouldn't replace a valid one the user is
      // currently looking at.
      if (latestListReqRef.current !== probedPath) return;
      if (probe.isStale()) return;
      // Route through humanizePs5Error so payload-side errors like
      // `fs_unmount_busy`, `path_not_allowed`, etc. surface as
      // actionable copy instead of opaque snake_case codes.
      const raw = e instanceof Error ? e.message : String(e);
      // Dead-path safety net: rather than strand the user on a path that
      // can't be read — behind a scary error, forcing them to manually pick a
      // new place — fall back to the always-present internal /data root and
      // tell them why. Two cases:
      //   • a USB/external drive that was unplugged (its /mnt/usb*|/mnt/ext*
      //     mount is now dangling), and
      //   • a folder that no longer exists (e.g. a remembered path that was
      //     since deleted) — the payload reports `…opendir_errno_2` (ENOENT).
      // Guard on `!== FS_DEFAULT_PATH` so a genuine /data failure still
      // surfaces instead of looping.
      const onRemovable = isRemovableMount(probedPath);
      const pathGone = /opendir_errno_2\b/i.test(raw);
      if ((onRemovable || pathGone) && probedPath !== FS_DEFAULT_PATH) {
        setError(null);
        setEntries(null);
        setPath(FS_DEFAULT_PATH);
        pushNotification(
          "info",
          pathGone && !onRemovable
            ? tr(
                "fs_path_gone",
                undefined,
                "That folder no longer exists — returned to /data",
              )
            : tr(
                "fs_drive_removed",
                undefined,
                "Drive unavailable — returned to /data",
              ),
          {
            body:
              pathGone && !onRemovable
                ? tr(
                    "fs_path_gone_body",
                    { path: probedPath },
                    `${probedPath} no longer exists on the PS5.`,
                  )
                : tr(
                    "fs_drive_removed_body",
                    { path: probedPath },
                    `${probedPath} could not be read (the drive may have been unplugged).`,
                  ),
          },
        );
        return;
      }
      setError(humanizePs5Error(raw) || raw);
      setEntries(null);
    } finally {
      // Always clear loading — even on stale-drop, since some other
      // refresh has either already cleared it or will clear it next.
      // Leaving it true on the stale path would create the
      // "loading-spinner-stuck-forever" UX bug.
      setLoading(false);
    }
  }, [host, path, guard, tr]);

  useEffect(() => {
    if (payloadStatus === "up") refresh();
  }, [payloadStatus, refresh]);

  // Fetch writable volumes whenever the connection becomes healthy or
  // the host changes. Same data the Library Move modal uses — listing
  // volumes on the FileSystem screen lets users jump between mounts
  // (/data → /mnt/ext1 → /mnt/usb0) with one click instead of
  // navigating up to / and clicking down. Errors swallow silently;
  // the picker just won't render.
  useEffect(() => {
    if (payloadStatus !== "up" || !host?.trim()) {
      setVolumes(null);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const vols = await fetchVolumes(consoleAddr(host));
        if (!cancelled) {
          // Only show writable, non-placeholder volumes — the picker
          // is for navigation, and a placeholder ("disk not yet
          // mounted") would jump to a nonexistent path.
          setVolumes(vols.filter((v) => v.writable && !v.is_placeholder));
        }
      } catch {
        if (!cancelled) setVolumes([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [host, payloadStatus]);

  // An "Add files" run ended (here, or while another screen was open): list the folder
  // again so what landed shows. The count is per console, so a console switch is not a run.
  const seenUploadRuns = useRef({ host, runs: uploadRuns });
  useEffect(() => {
    const seen = seenUploadRuns.current;
    seenUploadRuns.current = { host, runs: uploadRuns };
    if (seen.host === host && seen.runs !== uploadRuns) void refresh();
  }, [host, uploadRuns, refresh]);

  // When the host changes (user typed a new IP), restore the
  // last-browsed path for THAT host. Without this, switching consoles
  // would leave you in the prior console's directory — a real
  // surprise if /mnt/ext1 doesn't exist on the new one and the dir
  // listing 502s.
  useEffect(() => {
    setPath(loadFsLastPath(host));
  }, [host]);

  // Deep link: `/files?path=/some/dir` jumps the browser there.
  //
  // Needed because `path` is seeded from localStorage ONCE on mount — so
  // writing the saved path and calling navigate("/files") does nothing when
  // the user is already on this screen. That is exactly the case the edit
  // session's "Open files" button hits, since its banner also renders here.
  // The param is consumed and stripped so a later reload or Back doesn't
  // re-pin the user to a folder they have since navigated away from.
  useEffect(() => {
    const want = searchParams.get("path");
    if (!want) return;
    setPath(want);
    saveFsLastPath(host, want);
    const next = new URLSearchParams(searchParams);
    next.delete("path");
    setSearchParams(next, { replace: true });
  }, [searchParams, setSearchParams, host]);

  // Honor a cross-screen "open this directory" request (Saves, Install
  // Package, etc. call openInFileSystem then navigate here). Declared AFTER
  // the host-restore effect so on a fresh mount it runs last and wins. The
  // null→path→null transition is safe: the null branch is a no-op, so it
  // never clobbers the jumped-to path.
  const requestedPath = useFsNavStore((s) => s.requestedPath);
  useEffect(() => {
    if (requestedPath != null) {
      setPath(requestedPath);
      useFsNavStore.getState().consume();
    }
  }, [requestedPath]);

  // Persist `path` whenever it changes (and we have a real host).
  // Saved per-host so multiple PS5s remember their own last
  // location independently.
  useEffect(() => {
    saveFsLastPath(host, path);
    // Also push into the global recent-paths MRU so the dropdown
    // surfaces "places I've been recently" across PS5s. Skip "/"
    // since the recents store filters it anyway.
    useRecentPathsStore.getState().push(path);
  }, [host, path]);

  // Changing directories clears the selection — carrying selections
  // across directories would be confusing (what does "delete" act on?).
  useEffect(() => {
    setSelected(new Set());
  }, [path]);

  const toggleSelected = (name: string) => {
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  const toggleAll = () => {
    if (!entries) return;
    setSelected((s) => {
      if (s.size === entries.length) return new Set();
      return new Set(entries.map((e) => e.name));
    });
  };

  const selectedEntries = useMemo(
    () => (entries ? entries.filter((e) => selected.has(e.name)) : []),
    [entries, selected],
  );

  const navigate = useNavigate();
  /** Open the package viewer on an entry here, with the actions that fit it. */
  const viewEntry = (entry: DirEntry) => {
    const full = joinPath(path, entry.name);
    const source = `ps5://${host}${full}`;
    const viewer = usePackageViewer.getState();
    viewer.open(
      source,
      isViewablePackage(entry.name)
        ? [
            {
              label: tr("fs_install_action", undefined, "Install"),
              primary: true,
              onClick: () => {
                viewer.close();
                void runInstallPkg(entry);
              },
            },
          ]
        : [
            {
              label: tr("drop_convert", undefined, "Convert…"),
              primary: true,
              onClick: () => {
                viewer.close();
                navigate("/convert", { state: { source } });
              },
            },
          ],
    );
  };

  const runInstallPkg = async (entry: DirEntry) => {
    const fullPath = joinPath(path, entry.name);
    const removable = /^\/mnt\/(usb|ext)/i.test(fullPath);
    const ok = await confirmDialog({
      title: tr(
        "fs_install_confirm_title",
        { name: entry.name },
        `Install "${entry.name}"?`,
      ),
      message: removable
        ? tr(
            "fs_install_confirm_body_usb",
            undefined,
            "This stages the package to internal storage (the console can't install off USB directly), installs it, then removes the staged copy. The original on your drive is untouched.",
          )
        : tr(
            "fs_install_confirm_body",
            undefined,
            "This installs the package on your PS5 via Sony's installer.",
          ),
      confirmLabel: tr("fs_install_action", undefined, "Install"),
    });
    if (!ok) return;
    setInstallingPkgName(entry.name);
    try {
      const r = await installFromConsolePath(fullPath, host);
      if (r.ok) {
        pushNotification(
          r.mayNotLaunch ? "warning" : "success",
          tr(
            "fs_install_done",
            { name: entry.name },
            `Installed ${entry.name}`,
          ),
          r.mayNotLaunch
            ? {
                body: tr(
                  "fs_install_may_not_launch",
                  undefined,
                  "The install registered, but the title may not launch — check Installed Apps.",
                ),
              }
            : undefined,
        );
      } else {
        pushNotification(
          "error",
          tr("fs_install_failed", { name: entry.name }, `Install failed`),
          { body: r.message },
        );
      }
    } finally {
      setInstallingPkgName(null);
    }
  };

  const runDelete = async (name: string) => {
    const ok = await confirmDialog({
      title: tr("fs_delete_confirm_title", { name }, `Delete "${name}"?`),
      message: tr(
        "fs_delete_confirm_body",
        undefined,
        "This removes the path recursively from your PS5. Can't be undone.",
      ),
      confirmLabel: tr("delete", undefined, "Delete"),
      destructive: true,
    });
    if (!ok) return;
    setBusyEntry({ name, op: "rename" }); // cheap reuse of spinner
    setError(null);
    // Track in the global activity log so the ActivityBar shows
    // "Deleting <name>" while it's in flight. Single-item deletes on
    // a directory tree (a 100 GB game folder) can take minutes — the
    // user needs a global signal that the work is still happening
    // even if they navigate away. Bulk-delete already tracks via
    // useFsBulkOpStore + activityWiring; this single-item path
    // never went through that store, so it goes silent today.
    const itemPath = joinPath(path, name);
    const activityId = useActivityHistoryStore
      .getState()
      .start("fs-delete", `Deleting ${name}`, {
        fromPath: itemPath,
        files: 1,
      });
    let okOutcome = true;
    let errMsg: string | null = null;
    try {
      // The activity bar reads the task store; a big folder can take minutes to delete.
      await trackTask(
        {
          kind: "fs-delete",
          origin: "files.delete",
          label: `Delete ${name}`,
          consoleId: host,
        },
        () => fsDelete(consoleAddr(host), itemPath),
      );
      await refresh();
    } catch (e) {
      okOutcome = false;
      errMsg = e instanceof Error ? e.message : String(e);
      setError(errMsg);
      pushNotification(
        "error",
        withConsolePrefix(
          host,
          tr("notif_fs_delete_failed", undefined, "Delete failed"),
        ),
        { body: errMsg },
      );
    } finally {
      useActivityHistoryStore
        .getState()
        .finish(activityId, okOutcome ? "done" : "failed", {
          error: errMsg ?? undefined,
        });
      setBusyEntry(null);
    }
  };

  const runRename = async (oldName: string) => {
    const newName = renameDraft.trim();
    if (!newName || newName === oldName) {
      setRenaming(null);
      return;
    }
    if (!isSinglePathName(newName)) {
      setError(
        tr(
          "fs_name_invalid",
          undefined,
          'Name can\'t contain / or \\ and can\'t be "." or "..".',
        ),
      );
      return;
    }
    // Don't silently clobber: the payload's rename() over an existing file
    // destroys it and reports success. Block when the target name is already
    // taken in this folder (the Move modal guards this too).
    if ((entries ?? []).some((en) => en.name === newName)) {
      setError(
        tr(
          "fs_rename_target_exists",
          { name: newName },
          `An item named "${newName}" already exists here — rename would overwrite it. Pick another name or delete it first.`,
        ),
      );
      return;
    }
    setBusyEntry({ name: oldName, op: "rename" });
    setError(null);
    try {
      await fsMove(
        consoleAddr(host),
        joinPath(path, oldName),
        joinPath(path, newName),
      );
      setRenaming(null);
      await refresh();
    } catch (e) {
      const raw = e instanceof Error ? e.message : String(e);
      const human = humanizePs5Error(raw) || raw;
      setError(human);
      pushNotification(
        "error",
        withConsolePrefix(
          host,
          tr("notif_fs_rename_failed", undefined, "Rename failed"),
        ),
        { body: human },
      );
    } finally {
      setBusyEntry(null);
    }
  };

  /** Bulk rename via a sed-ish pattern. Pattern grammar (kept tiny):
   *    - `s/old/new/`  → substring replace (first match, case-sens)
   *    - `s/old/new/g` → substring replace (all matches)
   *    - `s/old/new/i` → case-insensitive
   *    - `^prefix_`    → unconditional prefix
   *    - `_suffix$`    → unconditional suffix (before extension)
   *    - `lower` / `upper` → case transform
   *
   *  Shows a preview dialog of what would change before applying. */
  const runBulkRename = async (selectedNames: string[]) => {
    const pattern = await promptDialog({
      title: `Bulk rename ${selectedNames.length} item(s)`,
      message:
        "Pattern grammar:\n" +
        "  s/old/new/[gi]   substring replace\n" +
        "  ^prefix_         add prefix\n" +
        "  _suffix$         add suffix (before extension)\n" +
        "  lower / upper    change case",
      placeholder: "e.g. s/CUSA/PCSA/  or  ^backup_  or  lower",
      okLabel: tr("fs_bulk_rename_next", undefined, "Next"),
    });
    if (!pattern) return;
    const renames: Array<{ from: string; to: string }> = [];
    const invalid: string[] = [];
    for (const name of selectedNames) {
      const next = applyRenamePattern(name, pattern);
      if (next && next !== name) {
        if (isSinglePathName(next)) {
          renames.push({ from: name, to: next });
        } else {
          invalid.push(`${name} → ${next}`);
        }
      }
    }
    if (invalid.length > 0) {
      await alertDialog({
        title: tr("fs_bulk_rename_invalid_title", undefined, "Invalid rename"),
        message:
          tr(
            "fs_bulk_rename_invalid_body",
            undefined,
            "Bulk rename produced names containing / or \\, or reserved names . / ...",
          ) +
          "\n\n" +
          invalid
            .slice(0, 10)
            .map((x) => `  ${x}`)
            .join("\n") +
          (invalid.length > 10 ? `\n  ...and ${invalid.length - 10} more` : ""),
      });
      return;
    }
    if (renames.length === 0) {
      await alertDialog({
        title: tr("fs_bulk_rename_no_change_title", undefined, "No changes"),
        message: tr(
          "fs_bulk_rename_no_change_body",
          undefined,
          "The pattern matched no selected file names.",
        ),
      });
      return;
    }
    // Overwrite guard: the payload's rename() silently destroys an existing
    // destination. Reject if any new name collides with a file already in
    // this folder (that isn't itself being renamed away), or if two renames
    // map onto the same target — otherwise the batch reports "N renamed, 0
    // failed" while the colliding files clobber each other.
    {
      const fromSet = new Set(renames.map((r) => r.from));
      const existing = new Set((entries ?? []).map((en) => en.name));
      const seenTargets = new Set<string>();
      const conflicts: string[] = [];
      for (const r of renames) {
        if (seenTargets.has(r.to)) {
          conflicts.push(`${r.to}  (two items rename to this)`);
        }
        seenTargets.add(r.to);
        if (existing.has(r.to) && !fromSet.has(r.to)) {
          conflicts.push(`${r.to}  (already exists)`);
        }
      }
      if (conflicts.length > 0) {
        await alertDialog({
          title: tr(
            "fs_bulk_rename_conflict_title",
            undefined,
            "Rename would overwrite files",
          ),
          message:
            tr(
              "fs_bulk_rename_conflict_body",
              undefined,
              "These target names collide and would destroy existing files — no files were renamed:",
            ) +
            "\n\n" +
            conflicts
              .slice(0, 10)
              .map((c) => `  ${c}`)
              .join("\n") +
            (conflicts.length > 10
              ? `\n  …and ${conflicts.length - 10} more`
              : ""),
        });
        return;
      }
    }
    const preview = renames
      .slice(0, 10)
      .map((r) => `  ${r.from} → ${r.to}`)
      .join("\n");
    const ok = await confirmDialog({
      title: tr(
        "fs_bulk_rename_confirm_title",
        { count: renames.length },
        `Apply rename to ${renames.length} item(s)?`,
      ),
      message:
        preview +
        (renames.length > 10 ? `\n  …and ${renames.length - 10} more` : ""),
      confirmLabel: tr("fs_bulk_rename_apply", undefined, "Rename"),
    });
    if (!ok) return;
    setError(null);
    let okCount = 0;
    const failures: string[] = [];
    for (const r of renames) {
      try {
        await fsMove(
          consoleAddr(host),
          joinPath(path, r.from),
          joinPath(path, r.to),
        );
        okCount++;
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        failures.push(`${r.from} → ${r.to}: ${humanizePs5Error(msg) || msg}`);
      }
    }
    if (failures.length > 0) {
      setError(`First failure: ${failures[0]}`);
      pushNotification(
        "error",
        withConsolePrefix(
          host,
          tr("notif_fs_rename_failed", undefined, "Rename failed"),
        ),
        { body: `First failure: ${failures[0]}` },
      );
    }
    await alertDialog({
      title: tr(
        "fs_bulk_rename_done_title",
        { ok: okCount, failed: failures.length },
        `Renamed ${okCount} item(s); ${failures.length} failed.`,
      ),
      // Show WHICH renames failed and why, not just a count — a partial
      // batch otherwise gives no actionable detail.
      message:
        failures.length > 0
          ? failures
              .slice(0, 10)
              .map((f) => `  ${f}`)
              .join("\n") +
            (failures.length > 10
              ? `\n  …and ${failures.length - 10} more`
              : "")
          : undefined,
    });
    setSelected(new Set());
    await refresh();
  };

  const runMkdir = async () => {
    const name = (mkdirDraft ?? "").trim();
    if (!name) {
      setMkdirDraft(null);
      return;
    }
    if (!isSinglePathName(name)) {
      setError(
        tr(
          "fs_name_invalid",
          undefined,
          'Name can\'t contain / or \\ and can\'t be "." or "..".',
        ),
      );
      return;
    }
    setBusyEntry({ name, op: "mkdir" });
    setError(null);
    try {
      await fsMkdir(consoleAddr(host), joinPath(path, name));
      setMkdirDraft(null);
      await refresh();
    } catch (e) {
      const raw = e instanceof Error ? e.message : String(e);
      const human = humanizePs5Error(raw) || raw;
      setError(human);
      pushNotification(
        "error",
        withConsolePrefix(
          host,
          tr("notif_fs_mkdir_failed", undefined, "Create folder failed"),
        ),
        { body: human },
      );
    } finally {
      setBusyEntry(null);
    }
  };

  /** Copy local files INTO the directory being browsed.
   *
   *  This is what makes a read-write mounted disk image editable: mount the
   *  image, browse to it, and drop replacement files straight in. Uploading
   *  to a path that already exists overwrites it, so "replace one file" and
   *  "add new files" are the same operation with a different destination —
   *  `replaceRemoteName` pins the destination name for the replace case so a
   *  patched file picked from disk under a different filename still lands on
   *  top of the original.
   *
   *  Sequential on purpose: these land inside one mounted image, and the
   *  payload writes a packed shard's records serially anyway. */
  const runUpload = async (srcPaths: string[], replaceRemoteName?: string) => {
    if (srcPaths.length === 0) return;
    setError(null);
    // The run reports its own end through the store (see the effects below), so it is
    // handled the same whether this screen is still mounted or was left and reopened.
    await runFsUpload(
      {
        host,
        addr: consoleAddr(host),
        destDir: path,
        srcPaths,
        replaceRemoteName,
      },
      {
        ...fsUploadDeps,
        onFailed: (raw) =>
          pushNotification(
            "error",
            withConsolePrefix(
              host,
              tr("notif_fs_upload_failed", undefined, "Copy to PS5 failed"),
            ),
            { body: humanizePs5Error(raw) || raw },
          ),
      },
    );
  };

  /** Stops an "Add files" upload: the engine ends the transfer job, and the batch stops
   *  before the next file. What already landed stays; the half-written file does not. */
  const cancelUpload = () => cancelFsUpload(host, fsUploadDeps);

  /** Toolbar: pick one or more local files and copy them into this folder. */
  const addFilesHere = async () => {
    const picked = await pickPaths({
      title: tr(
        "fs_add_files_dialog_title",
        undefined,
        "Pick files to copy onto the PS5",
      ),
    });
    if (!picked || picked.length === 0) return;
    await addPicked(picked);
  };

  const addFolderHere = async () => {
    const picked = await pickPath({
      mode: "folder",
      title: tr(
        "fs_add_folder_dialog_title",
        undefined,
        "Pick a folder to copy onto the PS5",
      ),
    });
    if (typeof picked !== "string") return;
    await addPicked([picked]);
  };

  // Files and folders dragged in from the computer's file manager land in this folder
  // (a .pkg too: this screen copies it, it does not offer to install it).
  const dropActive = useWebviewDropAll(
    (paths) => void addPicked(paths),
    !loading && !!host?.trim() && busyEntry === null,
  );

  /** Upload picked files (local or on a saved server) into this folder, asking first when
   *  one would overwrite a file already here. */
  const addPicked = async (picked: string[]) => {
    // Merge by default, ask on collision. Adding files that don't exist here
    // yet needs no ceremony; overwriting one the user may not have realised
    // was already there does. The upload itself replaces the remote file, so
    // this prompt is the only thing standing in front of it.
    const existingNames = new Set((entries ?? []).map((e) => e.name));
    const clashes = picked
      .map((p) => p.split(/[/\\]/).pop() ?? p)
      .filter((n) => existingNames.has(n));
    if (clashes.length > 0) {
      const shown = clashes.slice(0, 6).join(", ");
      const more = clashes.length > 6 ? ` and ${clashes.length - 6} more` : "";
      const ok = await confirmDialog({
        title: tr(
          "fs_add_files_conflict_title",
          { count: clashes.length },
          clashes.length === 1
            ? `"${clashes[0]}" already exists here`
            : `${clashes.length} files already exist here`,
        ),
        message: tr(
          "fs_add_files_conflict_body",
          { names: shown + more },
          `${shown}${more} will be overwritten. Everything else in this folder is left alone.\n\nIf this is inside a mounted game image, a bad file can stop the game booting, and there is no undo.`,
        ),
        confirmLabel: tr("fs_add_files_conflict_ok", undefined, "Overwrite"),
        destructive: true,
      });
      if (!ok) return;
    }
    await runUpload(picked);
  };

  /** Row action: overwrite one existing file with a local one. */
  const replaceEntry = async (entry: DirEntry) => {
    const picked = await pickPath({
      mode: "file",
      title: tr(
        "fs_replace_dialog_title",
        { name: entry.name },
        'Pick the file to replace "{name}" with',
      ),
    });
    if (typeof picked !== "string") return;
    const ok = await confirmDialog({
      title: tr("fs_replace_confirm_title", undefined, "Replace this file?"),
      message: tr(
        "fs_replace_confirm_body",
        { name: entry.name },
        '"{name}" will be overwritten and the old contents are gone for good. If this is inside a mounted game image, a bad file can stop the game booting.',
      ),
      confirmLabel: tr("fs_replace_confirm_ok", undefined, "Replace"),
      destructive: true,
    });
    if (!ok) return;
    await runUpload([picked], entry.name);
  };

  // Bulk: delete all selected entries. Best-effort — surfaces the first
  // error but continues so a batch where one item fails doesn't abort
  // the rest.
  const runBulkDelete = async () => {
    // Single-flight PER CONSOLE: refuse if THIS console already has a bulk
    // op running. Without this, a tab-switch + return + click sequence could
    // double-fire since the runner is async and outlives the original event.
    // Other consoles are unaffected — they run their own ops concurrently.
    if (fsBulk.op !== null) return;
    const names = [...selected];
    if (names.length === 0) return;
    const ok = await confirmDialog({
      title: tr(
        "fs_bulk_delete_confirm_title",
        { count: names.length },
        `Delete ${names.length} item${names.length === 1 ? "" : "s"}?`,
      ),
      message: tr(
        "fs_bulk_delete_confirm_body",
        { path },
        `Removes from ${path}. This can't be undone.`,
      ),
      confirmLabel: tr("delete", undefined, "Delete"),
      destructive: true,
    });
    if (!ok) return;
    setError(null);
    fsBulk.begin({
      op: "delete",
      total: names.length,
      fromPath: path,
      toPath: "",
    });
    let result: { firstError: string | null } = { firstError: null };
    try {
      // Snapshot the per-name sizes from the current entries listing
      // so the busy banner can show "deleting 1.17 GiB foo.ffpkg"
      // even for items the user picked from the directory before
      // this op started — list_dir already gave us sizes; re-stat
      // would be a wasted round trip.
      const sizeByName = new Map<string, number>(
        (entries ?? []).map((e) => [e.name, e.size]),
      );
      const addr = consoleAddr(host);
      // Same op_id-tracked deleter shape as the cut/copy/paste loop:
      // mint a fresh 64-bit op_id per item, register it with the
      // bulk-op store, spawn a parallel poller that scrapes
      // FS_OP_STATUS into currentBytesCopied, and a separate cancel
      // watcher that fires FS_OP_CANCEL when the Stop button flips
      // the store's flag. The actual fsDelete call awaits at the
      // bottom — when it returns (or throws "cancelled" on user
      // stop), the finally block tears both watchers down. A
      // small-file-heavy game folder (PPSA01342: 223k files) takes
      // minutes; without the poller the user would stare at a static
      // banner for that whole time.
      result = await runBulkDeleteLoop({
        names,
        basePath: path,
        sizeByName,
        joinPath,
        deleter: async (itemPath) => {
          const opId = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);
          fsBulk.setCurrentOpId(opId);
          let pollerStopped = false;
          const pollerDone = (async () => {
            await new Promise((r) => setTimeout(r, 250));
            while (!pollerStopped) {
              try {
                const snap = await fsOpStatus(addr, opId);
                fsBulk.setCurrentBytesCopied(snap.bytes_copied);
                // Directory-tree deletes start with currentSize=null
                // (list_dir doesn't surface dir sizes); the payload's
                // recursive_size pre-walk fills total_bytes — sync it
                // back so the banner can render a percentage.
                if (
                  snap.total_bytes > 0 &&
                  fsBulk.currentSize !== snap.total_bytes
                ) {
                  fsBulk.setProgress({
                    done: fsBulk.done,
                    currentPath: fsBulk.currentPath,
                    currentName: fsBulk.currentName,
                    currentSize: snap.total_bytes,
                  });
                  fsBulk.setCurrentBytesCopied(snap.bytes_copied);
                }
              } catch {
                // A 404 ("not in flight") arrives before the engine has registered the
                // op as well as after it ends, and a blip throws too. None of them may
                // end the poller: it stops when the delete call returns.
              }
              await new Promise((r) => setTimeout(r, 500));
            }
          })();
          const cancelWatcher = (async () => {
            while (!pollerStopped) {
              if (fsBulk.cancelRequested) {
                try {
                  await fsOpCancel(addr, opId);
                } catch (e) {
                  // Best effort — even if cancel RPC fails, the
                  // payload's rm_rf_op will exit on the next
                  // between-entries check and the loop will see
                  // cancelRequested in shouldCancel. But on a
                  // single huge file the between-iterations check
                  // never fires, so a lost cancel feels like Stop
                  // doesn't work. Greppable warn so we know when.
                  console.warn("fsOpCancel (delete) failed:", e);
                }
                break;
              }
              await new Promise((r) => setTimeout(r, 200));
            }
          })();
          try {
            await fsDelete(addr, itemPath, opId);
          } catch (e) {
            // Engine maps payload's "fs_delete_cancelled" to HTTP 409
            // → fsDelete throws an Error containing "cancelled". The
            // user already requested cancel (cancelRequested=true);
            // swallow so bulkDelete doesn't count this as a failure.
            // The next iteration's shouldCancel guard breaks the loop.
            const msg = e instanceof Error ? e.message : String(e);
            if (!msg.includes("cancelled")) throw e;
          } finally {
            pollerStopped = true;
            await Promise.allSettled([pollerDone, cancelWatcher]);
            fsBulk.setCurrentOpId(null);
          }
        },
        onProgress: (p) => fsBulk.setProgress(p),
        shouldCancel: () => fsBulk.cancelRequested,
      });
    } finally {
      // Always release this console's bulk-op slot, even if a sync error in
      // the loop body throws. Without this finally, an exception would leave
      // op !== null in this console's slot, and the per-console single-flight
      // guard would permanently block future bulk ops on this console until
      // reload.
      fsBulk.end(result.firstError);
      if (result.firstError) {
        pushNotification(
          "error",
          withConsolePrefix(
            host,
            tr(
              "notif_fs_bulk_op_failed",
              undefined,
              "File operation finished with errors",
            ),
          ),
          { body: result.firstError },
        );
      }
    }
    if (result.firstError) setError(result.firstError);
    await refresh();
  };

  // Cut / Copy: stage selection into the shared clipboard. Clears local
  // selection so the UI reflects that the items are "in flight" via
  // the toolbar instead of by highlighting.
  // ── FileZilla-style browsing: sort columns, keyboard, row menu, typed path ──
  const [sort, setSort] = useState<SortState>({ key: "name", desc: false });
  const sortedEntries = useMemo(
    () => (entries ? sortEntries(entries, sort) : []),
    [entries, sort],
  );
  // "Move to…": the items waiting for a destination, or null when the dialog is closed.
  const [moveItems, setMoveItems] = useState<MoveItem[] | null>(null);
  const startMove = (list: DirEntry[] = selectedEntries) => {
    if (list.length === 0) return;
    setMoveItems(list.map((e) => ({ path: joinPath(path, e.name), name: e.name, size: e.size })));
  };
  const confirmMove = (dest: string) => {
    const items = moveItems ?? [];
    setMoveItems(null);
    if (items.length === 0) return;
    rememberDestination(hostOf(host), dest);
    setSelected(new Set());
    const byPath = new Map((entries ?? []).map((e) => [joinPath(path, e.name), e]));
    void runPaste(dest, {
      op: "cut",
      items: items.map((i) => ({
        ...i,
        kind: byPath.get(i.path)?.kind === "dir" ? "dir" : "file",
      })),
      sourceLabel: path,
    });
  };
  // Set permissions to 777 (folders recursively): what games and payload folders need to be
  // readable and runnable, and what users reached for FileZilla to do (#411).
  const [chmodBusy, setChmodBusy] = useState(false);
  const runChmod777 = async (list: DirEntry[] = selectedEntries) => {
    if (list.length === 0 || chmodBusy) return;
    setChmodBusy(true);
    setError(null);
    const failed: string[] = [];
    for (const e of list) {
      try {
        await fsChmod(consoleAddr(host), joinPath(path, e.name), "0777", e.kind === "dir");
      } catch (err) {
        failed.push(`${e.name}: ${err instanceof Error ? err.message : String(err)}`);
      }
    }
    setChmodBusy(false);
    if (failed.length > 0) {
      setError(failed.join(" · "));
      pushNotification(
        "error",
        withConsolePrefix(host, tr("fs_chmod_failed", undefined, "Couldn't set permissions")),
        { body: failed.join(" · ") },
      );
    } else {
      pushNotification(
        "success",
        withConsolePrefix(
          host,
          tr("fs_chmod_done", { count: list.length }, `Permissions set to 777 on ${list.length} item(s)`),
        ),
      );
      setSelected(new Set());
    }
    await refresh();
  };
  // `entry` is null for a right-click on the list's empty space: the folder
  // itself, so the menu offers what applies to it (New folder, Paste, Refresh).
  const [rowMenu, setRowMenu] = useState<{
    x: number;
    y: number;
    entry: DirEntry | null;
  } | null>(null);
  const [pathDraft, setPathDraft] = useState<string | null>(null);

  const openEntry = (e: DirEntry) => {
    if (e.kind === "dir") setPath(joinPath(path, e.name));
    else viewEntry(e);
  };
  const startRename = (name: string) => {
    setRenaming(name);
    setRenameDraft(name);
  };
  const copyEntryPath = async (e: DirEntry) => {
    const ok = await writeClipboard(joinPath(path, e.name));
    if (!ok)
      setError(
        tr(
          "fs_copy_path_failed",
          undefined,
          "Couldn't copy the path to the clipboard.",
        ),
      );
  };

  const stageClipboard = (
    op: "cut" | "copy",
    list: DirEntry[] = selectedEntries,
  ) => {
    const items: ClipboardItem[] = list.map((e) => ({
      path: joinPath(path, e.name),
      name: e.name,
      size: e.size,
      kind: e.kind === "dir" ? "dir" : "file",
    }));
    if (items.length === 0) return;
    clipboard.set(op, items, path);
    setSelected(new Set());
  };

  // Paste: apply clipboard.op to every item, pasting into `path`.
  //
  // Failure modes tracked explicitly:
  //   - `errors[]`: any item that didn't succeed cleanly. Shown to the
  //     user so they see exactly which items failed and why.
  //   - `duplicated[]`: cut+EXDEV fallback did copy but couldn't delete
  //     the source. The file now lives in both places — flagged
  //     distinctly so the user knows it's not a silent loss.
  //
  // Clipboard clears only when every cut succeeded cleanly. Any
  // failure keeps the clipboard so the user can retry (maybe after
  // freeing space or fixing permissions).
  const runPaste = async (
    into: string = path,
    // Move to… hands its selection straight in, so it never touches the clipboard.
    staged?: { op: "cut" | "copy"; items: ClipboardItem[]; sourceLabel: string },
  ) => {
    // Single-flight guard PER CONSOLE: same rationale as runBulkDelete.
    if (fsBulk.op !== null) return;
    const op = staged ? staged.op : clipboard.op;
    const items = staged ? staged.items : clipboard.items;
    const sourceLabel = staged ? staged.sourceLabel : clipboard.sourceLabel;
    if (items.length === 0 || !op) return;

    // Name conflicts: the payload refuses to touch an existing destination
    // unless we explicitly ask it to merge, so find the collisions up front
    // and let the user decide once for the whole paste. Before this, pasting
    // a file over one that already existed just failed with
    // `fs_copy_dest_exists` and no way forward.
    // Pasting into a subfolder (the row menu's "Paste into"): its own listing decides the
    // conflicts, not the folder on screen.
    const existingNames =
      into === path
        ? new Set((entries ?? []).map((e) => e.name))
        : new Set(
            (await fsListDir(consoleAddr(host), into).catch(() => [])).map(
              (e) => e.name,
            ),
          );
    const conflicts = items.filter((i) => existingNames.has(i.name));
    let overwrite = false;
    if (conflicts.length > 0) {
      const names = conflicts
        .slice(0, 6)
        .map((c) => c.name)
        .join(", ");
      const more =
        conflicts.length > 6 ? ` and ${conflicts.length - 6} more` : "";
      overwrite = await confirmDialog({
        title: tr(
          "fs_paste_conflict_title",
          { count: conflicts.length },
          conflicts.length === 1
            ? `"${conflicts[0].name}" already exists here`
            : `${conflicts.length} items already exist here`,
        ),
        message: tr(
          "fs_paste_conflict_body",
          { names: names + more },
          `Replace ${names}${more}?\n\nFolders are merged, not emptied: files inside them that you aren't pasting are left exactly as they are. Files with the same name are overwritten, and that can't be undone.`,
        ),
        confirmLabel: tr("fs_paste_conflict_ok", undefined, "Replace"),
        destructive: true,
      });
      if (!overwrite) return;
    }
    setError(null);
    fsBulk.begin({
      op: op === "cut" ? "paste-move" : "paste-copy",
      total: items.length,
      fromPath: sourceLabel ?? "",
      toPath: into,
    });
    const addr = consoleAddr(host);
    const errors: string[] = [];
    const duplicated: string[] = [];
    try {
      for (let i = 0; i < items.length; i++) {
        // Between-items cancel check. We *also* drive in-flight
        // cancellation for the current item via FS_OP_CANCEL below
        // — that's what gives a 28 GiB copy a sub-second Stop.
        if (fsBulk.cancelRequested) break;
        const item = items[i];
        const target = joinPath(into, item.name);
        fsBulk.setProgress({
          done: i,
          currentPath: item.path,
          currentName: item.name,
          currentSize: item.size ?? null,
        });
        // Generate a unique 64-bit op_id per item. Math.random() ×
        // 2^53 is plenty of entropy for a session — the payload only
        // needs uniqueness across the at-most-MAX_FS_OPS in-flight
        // copies, not collision-resistance.
        const opId = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);
        fsBulk.setCurrentOpId(opId);
        // Spawn a parallel poller that updates byte-progress in the
        // store every 500 ms. The fsCopy/fsMove call below blocks
        // for the full duration of the copy; without this the user
        // would see no progress until the operation completes.
        let pollerStopped = false;
        const pollerDone = (async () => {
          // Initial delay matches the engine's status-call cost so
          // we don't spam an empty payload immediately.
          await new Promise((r) => setTimeout(r, 250));
          while (!pollerStopped) {
            try {
              const snap = await fsOpStatus(addr, opId);
              fsBulk.setCurrentBytesCopied(snap.bytes_copied);
              // Also keep currentSize in sync with the payload's
              // measured total — the directory case starts with
              // size=null but the recursive walk knows the total.
              if (
                snap.total_bytes > 0 &&
                fsBulk.currentSize !== snap.total_bytes
              ) {
                fsBulk.setProgress({
                  done: fsBulk.done,
                  currentPath: fsBulk.currentPath,
                  currentName: fsBulk.currentName,
                  currentSize: snap.total_bytes,
                });
                fsBulk.setCurrentBytesCopied(snap.bytes_copied);
              }
            } catch {
              // Breaking here used to end progress for the whole copy: a 404 arrives
              // before the engine registers the op (the console-ready check runs first)
              // as well as after it ends, and a blip throws too. Keep polling; the
              // poller stops when the copy call returns.
            }
            await new Promise((r) => setTimeout(r, 500));
          }
        })();
        // Hook the Stop button: a separate watcher fires
        // fsOpCancel as soon as the store's flag flips. Distinct
        // from the between-items cancel check — this lets the
        // current 28 GiB copy abort within ~one disk IO.
        const cancelWatcher = (async () => {
          while (!pollerStopped) {
            if (fsBulk.cancelRequested) {
              // Keep asking until the engine says the op took the cancel: an early press
              // lands before the engine has registered the copy, and a single try was
              // lost (the copy ran to the end). The engine also remembers an early
              // cancel, so this converges either way; the loop ends when the copy call
              // returns (`pollerStopped`).
              let acknowledged = false;
              try {
                acknowledged = await fsOpCancel(addr, opId);
              } catch (e) {
                // Best effort — a failed cancel RPC is retried below. On a single
                // 28 GiB file "between items" never fires, so log when this drops.
                console.warn("fsOpCancel (copy) failed:", e);
              }
              if (acknowledged) break;
              await new Promise((r) => setTimeout(r, 300));
              continue;
            }
            await new Promise((r) => setTimeout(r, 200));
          }
        })();
        try {
          if (op === "cut") {
            try {
              await fsMove(addr, item.path, target, opId);
            } catch (e) {
              // Cross-filesystem move failure → fall back to copy + delete.
              // The payload emits "fs_move_cross_mount" for EXDEV; matching
              // on substring keeps us resilient to minor wording drift.
              const msg = e instanceof Error ? e.message : String(e);
              if (msg.includes("cross_mount") || msg.includes("EXDEV")) {
                await fsCopy(addr, item.path, target, opId, overwrite);
                // The original goes only once the copy is known whole: a file's size must
                // match before anything is deleted (a short copy used to cost the original).
                const short = await copyIsShort(addr, item, target);
                if (short) {
                  duplicated.push(
                    tr(
                      "fs_move_copy_short",
                      { name: item.name, detail: short },
                      `${item.name}: the copy is incomplete (${short}), so the original was kept.`,
                    ),
                  );
                  continue;
                }
                try {
                  await fsDelete(addr, item.path);
                } catch (delErr) {
                  // Copy succeeded, delete failed. The file exists in
                  // both locations — surface distinctly so the user
                  // knows the move was incomplete, not silently lost.
                  duplicated.push(
                    `${item.name} (copied to ${target}, couldn't remove original: ${
                      delErr instanceof Error ? delErr.message : String(delErr)
                    })`,
                  );
                }
              } else {
                throw e;
              }
            }
          } else {
            await fsCopy(addr, item.path, target, opId, overwrite);
          }
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          // The engine maps the payload's "fs_copy_cancelled" error
          // to a `cancelled` message via HTTP 409. Don't push it
          // into the errors list — the user already knows they
          // cancelled, and a "cancelled" entry would look like a
          // failure in the post-op summary.
          if (!msg.includes("cancelled")) {
            errors.push(`${item.name}: ${msg}`);
          }
        } finally {
          pollerStopped = true;
          await Promise.allSettled([pollerDone, cancelWatcher]);
          fsBulk.setCurrentOpId(null);
        }
      }
    } finally {
      const problemMsgs = [...duplicated, ...errors];
      // Always release this console's bulk-op slot so the per-console
      // single-flight guard doesn't strand it with op !== null after a sync
      // error in the loop body.
      fsBulk.end(problemMsgs.length > 0 ? problemMsgs.join(" · ") : null);
      if (problemMsgs.length > 0) {
        pushNotification(
          "error",
          withConsolePrefix(
            host,
            tr(
              "notif_fs_bulk_op_failed",
              undefined,
              "File operation finished with errors",
            ),
          ),
          { body: problemMsgs.join(" · ") },
        );
      }
    }
    const problemMsgs = [...duplicated, ...errors];
    if (problemMsgs.length > 0) {
      setError(problemMsgs.join(" · "));
    }
    // Only clear the clipboard when every cut succeeded cleanly — a
    // partial failure means the user might want to retry the leftover
    // items. Copies always keep the clipboard (same-items-many-places
    // is the usual copy workflow).
    if (!staged && op === "cut" && problemMsgs.length === 0) clipboard.clear();
    await refresh();
  };

  const [preview, setPreview] = useState<{
    name: string;
    kind: "text" | "image";
    body: string;
    size: number;
  } | null>(null);
  // Lock background scroll while the (inline) file preview is open.
  useScrollLock(!!preview);

  /** Preview a small file inline. Text decodes as UTF-8; .png/.jpg
   *  show as a data URL. Capped at 256 KB; bigger files surface a
   *  "too large to preview" hint. */
  const runPreview = async (entry: DirEntry) => {
    if (entry.kind !== "file") return;
    if (entry.size > 256 * 1024) {
      await alertDialog({
        title: tr("fs_preview_too_large_title", undefined, "Preview too large"),
        message: tr(
          "fs_preview_too_large_body",
          { size: formatBytes(entry.size) },
          `File is ${formatBytes(entry.size)} — preview is capped at 256 KB. Use Download instead.`,
        ),
      });
      return;
    }
    try {
      const { fsReadPreview } = await import("../../api/ps5");
      const remote = joinPath(path, entry.name);
      const r = await fsReadPreview(toMgmtAddr(host), remote);
      const ext = (entry.name.split(".").pop() ?? "").toLowerCase();
      const isImg = ["png", "jpg", "jpeg", "gif", "bmp", "webp"].includes(ext);
      let body: string;
      let kind: "text" | "image";
      if (isImg) {
        const mime =
          ext === "jpg" || ext === "jpeg" ? "image/jpeg" : `image/${ext}`;
        body = `data:${mime};base64,${r.base64}`;
        kind = "image";
      } else {
        // Decode base64 → bytes → UTF-8 (lossy). Replacement
        // characters are fine for text preview; users get the gist.
        const bin = atob(r.base64);
        const bytes = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        body = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
        kind = "text";
      }
      setPreview({ name: entry.name, kind, body, size: r.size });
    } catch (e) {
      await alertDialog({
        title: tr("fs_preview_failed", undefined, "Preview failed"),
        message: e instanceof Error ? e.message : String(e),
      });
    }
  };

  /** BLAKE3 hash + CRC32 of a single file, side-by-side. Slow but
   *  thorough — call this when you suspect bit-rot or want crypto
   *  strength integrity. */
  const runVerify = async (entry: DirEntry) => {
    if (entry.kind !== "file") return;
    try {
      const { fsBlake3Hash, crc32File } = await import("../../api/ps5");
      const remote = joinPath(path, entry.name);
      const addr = toMgmtAddr(host);
      // Fire both in parallel — BLAKE3 is the slow one (~3s/GiB)
      // and CRC32 should overlap with it.
      const [blake, crc] = await Promise.allSettled([
        fsBlake3Hash(addr, remote),
        crc32File(addr, remote),
      ]);
      const lines: string[] = [`Verifying ${entry.name}`, ""];
      if (blake.status === "fulfilled") {
        lines.push(`BLAKE3: ${blake.value.hash}`);
        lines.push(`Size:   ${blake.value.size} bytes`);
      } else {
        lines.push(`BLAKE3: failed — ${blake.reason}`);
      }
      if (crc.status === "fulfilled") {
        const hex = (crc.value.crc32 ?? 0)
          .toString(16)
          .padStart(8, "0")
          .toUpperCase();
        lines.push(`CRC32:  0x${hex}`);
      } else {
        lines.push(`CRC32: failed — ${crc.reason}`);
      }
      await alertDialog({
        title: tr("fs_verify_result", undefined, "Verify result"),
        message: lines.join("\n"),
      });
    } catch (e) {
      await alertDialog({
        title: tr("fs_verify_failed", undefined, "Verify failed"),
        message: e instanceof Error ? e.message : String(e),
      });
    }
  };

  /** Compute CRC32 of a single file on the PS5 and show it in an
   *  alert. Useful for spot-checking file integrity post-upload
   *  without the full BLAKE3 reconcile. */
  const runCrc32 = async (entry: DirEntry) => {
    if (entry.kind !== "file") return;
    try {
      const { crc32File } = await import("../../api/ps5");
      const remote = joinPath(path, entry.name);
      const r = await crc32File(toMgmtAddr(host), remote);
      if (r.err) {
        await alertDialog({
          title: tr("fs_crc32_failed", undefined, "CRC32 failed"),
          message: r.err,
        });
        return;
      }
      const hex = (r.crc32 ?? 0).toString(16).padStart(8, "0").toUpperCase();
      const expected = await promptDialog({
        title: tr(
          "fs_crc32_compare_title",
          { name: entry.name },
          `CRC32 of ${entry.name}`,
        ),
        message: tr(
          "fs_crc32_compare_body",
          { hex, size: r.size ?? 0 },
          `0x${hex}\n(${r.size ?? 0} bytes)\n\nPaste a known-good CRC32 (hex or decimal) to compare, or leave blank.`,
        ),
        okLabel: tr("fs_crc32_compare", undefined, "Compare"),
      });
      if (expected !== null && expected.trim()) {
        const trimmed = expected.trim().toLowerCase();
        const candidate = trimmed.startsWith("0x")
          ? parseInt(trimmed.slice(2), 16)
          : /^[0-9a-f]+$/i.test(trimmed) && trimmed.length === 8
            ? parseInt(trimmed, 16)
            : parseInt(trimmed, 10);
        if (!isNaN(candidate) && candidate === r.crc32) {
          await alertDialog({
            title: tr("fs_crc32_match", undefined, "✓ CRC32 matches"),
          });
        } else if (!isNaN(candidate)) {
          await alertDialog({
            title: tr("fs_crc32_mismatch", undefined, "✗ CRC32 mismatch"),
            message: `got 0x${hex}, expected 0x${candidate.toString(16).padStart(8, "0").toUpperCase()}`,
          });
        }
      }
    } catch (e) {
      await alertDialog({
        title: tr("fs_crc32_error", undefined, "CRC32 error"),
        message: e instanceof Error ? e.message : String(e),
      });
    }
  };

  /** Save a file or folder under the current dir to the host. Same
   *  flow as Library's per-row Download — pick a host folder, kick
   *  off `transfer_download`, poll jobStatus to terminal. Single-
   *  flight at the screen level: only one download at a time. */
  const runDownload = async (entry: DirEntry, asZip = false) => {
    // Single-flight per console: only one download at a time on THIS
    // console (others download concurrently into their own slots).
    if (fsDownload.active) return;
    const addr = consoleAddr(host);
    const remote = joinPath(path, entry.name);
    const kind: "file" | "folder" = entry.kind === "dir" ? "folder" : "file";

    // asZip: pick a .zip save path and stream-zip into it (no scratch dir).
    // Otherwise: pick a destination folder and pull the file/tree as-is.
    let dest: string;
    let rootName: string;
    let start: () => Promise<string>;
    if (asZip) {
      const destZip = await saveDialog({
        defaultPath: `${entry.name}.zip`,
        filters: [{ name: "ZIP archive", extensions: ["zip"] }],
        title: tr(
          "fs_download_zip_dialog_title",
          { name: entry.name },
          'Save "{name}" as a .zip',
        ),
      });
      if (!destZip || typeof destZip !== "string") return;
      dest = destZip;
      rootName = destZip.split(/[\\/]/).pop() || `${entry.name}.zip`;
      start = () =>
        startTransferDownloadZip(remote, destZip, addr, kind, systemFileRead);
    } else {
      const picked = await pickPath({
        mode: "folder",
        title: tr(
          "fs_download_dialog_title",
          { name: entry.name },
          'Pick a destination folder for "{name}"',
        ),
      });
      if (typeof picked !== "string") return;
      dest = picked;
      rootName = entry.name;
      start = () =>
        startTransferDownload(remote, picked, addr, kind, systemFileRead);
    }
    setError(null);
    let jobId: string;
    try {
      jobId = await start();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      const friendly = tr(
        "fs_download_start_failed",
        { error: msg },
        "Couldn't start the download: {error}",
      );
      setError(friendly);
      pushNotification(
        "error",
        withConsolePrefix(
          host,
          tr("notif_fs_download_failed", undefined, "Download failed"),
        ),
        { body: friendly },
      );
      fsDownload.end(friendly);
      return;
    }
    const myRunId = fsDownload.begin({
      jobId,
      rootName,
      rootSrcPath: remote,
      destDir: dest,
    });
    // Generation-counted abort: the runner captures myRunId and
    // bails when the store's runId moves on (begin() of a new
    // download OR explicit requestStop() from the UI). Without the
    // check, a navigation-away + back + new-download sequence
    // would leave the old runner still polling against the old
    // job and the new runner's setProgress writes would race with
    // it. Bonus: gives the user a way to abort an in-flight pull
    // by clicking Stop in the banner — store.requestStop() flips
    // runId; this loop exits at its next check; the engine job
    // continues server-side and the .part promotion eventually
    // happens (no engine cancel API today).
    const isLive = () => fsDownload.runId === myRunId;
    while (true) {
      if (!isLive()) return;
      try {
        const snap = await jobStatus(jobId);
        if (!isLive()) return;
        if (snap.status === "done") {
          fsDownload.end(null);
          return;
        }
        if (snap.status === "failed") {
          const friendly = tr(
            "fs_download_failed",
            { error: snap.error ?? "download failed" },
            "Download failed: {error}",
          );
          setError(friendly);
          pushNotification(
            "error",
            withConsolePrefix(
              host,
              tr("notif_fs_download_failed", undefined, "Download failed"),
            ),
            { body: friendly },
          );
          fsDownload.end(friendly);
          return;
        }
        fsDownload.setProgress({
          bytesReceived: snap.bytes_sent ?? 0,
          totalBytes: snap.total_bytes ?? 0,
        });
      } catch (e) {
        if (!isLive()) return;
        const msg = e instanceof Error ? e.message : String(e);
        const friendly = tr(
          "fs_download_poll_failed",
          { error: msg },
          "Lost contact with the engine while downloading: {error}",
        );
        setError(friendly);
        pushNotification(
          "error",
          withConsolePrefix(
            host,
            tr("notif_fs_download_failed", undefined, "Download failed"),
          ),
          { body: friendly },
        );
        fsDownload.end(friendly);
        return;
      }
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  };

  const selectionActive = selected.size > 0;
  const clipboardActive = clipboard.items.length > 0;

  // Which volume root contains the current path? Used to highlight
  // the corresponding option in the picker. Longest-prefix match
  // because volume mount points can nest (e.g. /mnt/ps5upload sits
  // under /mnt).
  //
  // Two normalizations matter:
  //   - The current path may carry a trailing slash from a stale
  //     URL or a careless setPath. Strip it before comparing so
  //     "/data/" still matches volume "/data".
  //   - The path "/" is technically a prefix of every volume, but
  //     it's not a meaningful match — it just means "outside any
  //     known volume root." Treat it as null so the picker shows
  //     "(custom path)" instead of arbitrarily picking the
  //     longest-named volume.
  // The keyboard map (fsBrowse.fsKeyAction). One window listener reading the newest state
  // through a ref, so it never re-subscribes per render; a text field or a dialog keeps its keys.
  const keyHandler = useRef<(e: KeyboardEvent) => void>(() => {});
  useEffect(() => {
    keyHandler.current = (e: KeyboardEvent) => {
      if (
        keysBelongElsewhere(e.target) ||
        renaming !== null ||
        mkdirDraft !== null
      )
        return;
      if (!entries || !host?.trim()) return;
      const action = fsKeyAction(e);
      if (!action) return;
      const sel = selectedEntries;
      const busy = busyEntry !== null || fsBulk.op !== null;
      switch (action) {
        case "select-all":
          setSelected(new Set(entries.map((x) => x.name)));
          break;
        case "copy":
        case "cut":
          if (sel.length === 0) return;
          stageClipboard(action);
          break;
        case "paste":
          if (busy || clipboard.items.length === 0) return;
          void runPaste();
          break;
        case "delete":
          if (busy || sel.length === 0) return;
          if (sel.length === 1) void runDelete(sel[0].name);
          else void runBulkDelete();
          break;
        case "rename":
          if (sel.length !== 1) return;
          startRename(sel[0].name);
          break;
        case "refresh":
          void refresh();
          break;
        case "up":
          if (path === "/") return;
          setPath(parent(path));
          break;
        case "open":
          if (sel.length !== 1) return;
          openEntry(sel[0]);
          break;
        case "clear":
          setSelected(new Set());
          setRowMenu(null);
          break;
      }
      e.preventDefault();
    };
  });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => keyHandler.current(e);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const currentVolumePath = useMemo(() => {
    if (!volumes || volumes.length === 0) return null;
    if (path === "/" || path === "") return null;
    const norm =
      path.length > 1 && path.endsWith("/") ? path.slice(0, -1) : path;
    let best: string | null = null;
    for (const v of volumes) {
      if (norm === v.path || norm.startsWith(v.path + "/")) {
        if (best === null || v.path.length > best.length) {
          best = v.path;
        }
      }
    }
    return best;
  }, [volumes, path]);

  // A const, so the menu items' closures keep the non-null narrowing.
  const menuEntry = rowMenu?.entry ?? null;

  return (
    <div className="app-page">
      {dropActive && (
        <div
          className="pointer-events-none fixed inset-0 z-40 flex items-center justify-center bg-[var(--color-accent-soft)] ring-4 ring-inset ring-[var(--color-accent)]"
          data-testid="fs-drop-overlay"
        >
          <div className="rounded-lg bg-[var(--color-surface)] px-5 py-3 text-sm font-medium shadow-lg">
            {tr("fs_drop_here", { path }, `Drop to copy into ${path}`)}
          </div>
        </div>
      )}
      {moveItems && (
        <MoveToDialog
          open
          host={hostOf(host)}
          addr={consoleAddr(host)}
          items={moveItems}
          volumes={volumes ?? []}
          startPath={parent(moveItems[0]?.path ?? path)}
          onCancel={() => setMoveItems(null)}
          onConfirm={confirmMove}
        />
      )}
      {rowMenu && menuEntry && (
        <RowMenu
          x={rowMenu.x}
          y={rowMenu.y}
          onClose={() => setRowMenu(null)}
          items={[
            {
              icon: menuEntry.kind === "dir" ? FolderOpen : Eye,
              label: tr("fs_menu_open", undefined, "Open"),
              run: () => openEntry(menuEntry),
            },
            {
              icon: Download,
              label: tr("fs_menu_download", undefined, "Download"),
              run: () => void runDownload(menuEntry),
              disabled: downloadOp.active,
            },
            {
              icon: FolderInput,
              label: tr("fs_move_to", undefined, "Move to…"),
              run: () => startMove([menuEntry]),
              disabled: fsBulk.op !== null || !volumes?.length,
            },
            {
              icon: Scissors,
              label: tr("fs_cut", "Cut"),
              run: () => stageClipboard("cut", [menuEntry]),
            },
            {
              icon: KeyRound,
              label: tr("fs_chmod_777", undefined, "Set permissions (777)"),
              run: () => void runChmod777([menuEntry]),
              disabled: chmodBusy,
            },
            {
              icon: Copy,
              label: tr("fs_menu_copy", undefined, "Copy"),
              run: () => stageClipboard("copy", [menuEntry]),
            },
            ...(menuEntry.kind === "dir" && clipboard.items.length > 0
              ? [
                  {
                    icon: ClipboardPaste,
                    label: tr(
                      "fs_menu_paste_into",
                      undefined,
                      "Paste into this folder",
                    ),
                    run: () =>
                      void runPaste(joinPath(path, menuEntry.name)),
                    disabled: fsBulk.op !== null,
                  },
                ]
              : []),
            {
              icon: Pencil,
              label: tr("fs_rename", "Rename"),
              run: () => startRename(menuEntry.name),
            },
            {
              icon: Link2,
              label: tr("fs_menu_copy_path", undefined, "Copy path"),
              run: () => void copyEntryPath(menuEntry),
            },
            {
              icon: FolderPlus,
              label: tr("fs_new_folder", "New folder"),
              run: () => setMkdirDraft(""),
              disabled: loading || !host?.trim(),
            },
            {
              icon: Trash2,
              label: tr("delete", undefined, "Delete"),
              run: () => void runDelete(menuEntry.name),
              destructive: true,
              disabled: busyEntry !== null,
            },
          ]}
        />
      )}
      {rowMenu && !menuEntry && (
        <RowMenu
          x={rowMenu.x}
          y={rowMenu.y}
          onClose={() => setRowMenu(null)}
          items={[
            {
              icon: FolderPlus,
              label: tr("fs_new_folder", "New folder"),
              run: () => setMkdirDraft(""),
              disabled: loading || !host?.trim(),
            },
            ...(clipboard.items.length > 0
              ? [
                  {
                    icon: ClipboardPaste,
                    label: tr("fs_paste_here", undefined, "Paste here"),
                    run: () => void runPaste(),
                    disabled: fsBulk.op !== null,
                  },
                ]
              : []),
            {
              icon: RefreshCw,
              label: tr("fs_refresh", "Refresh"),
              run: () => void refresh(),
              disabled: loading || !host?.trim(),
            },
          ]}
        />
      )}
      {confirmDialogNode}
      {alertDialogNode}
      {promptDialogNode}
      <PageHeader
        icon={FolderTree}
        title={tr("file_system", undefined, "File System")}
        loading={loading}
        right={
          <div className="flex items-center gap-1.5">
            <BrowseButton
              mode="file"
              remote
              icon={<Upload size={12} />}
              label={tr("fs_add_files", "Add files")}
              title={tr(
                "fs_add_files_dialog_title",
                undefined,
                "Pick files to copy onto the PS5",
              )}
              disabled={loading || !host?.trim() || busyEntry !== null}
              onMainClick={() => void addFilesHere()}
              onPick={(p) => void addPicked([p])}
            />
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<FolderUp size={12} />}
              onClick={() => void addFolderHere()}
              disabled={loading || !host?.trim() || busyEntry !== null}
              title={tr(
                "fs_add_folder_dialog_title",
                undefined,
                "Pick a folder to copy onto the PS5",
              )}
            >
              {tr("fs_add_folder", undefined, "Add folder")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<FolderPlus size={12} />}
              onClick={() => setMkdirDraft("")}
              disabled={loading || !host?.trim()}
            >
              {tr("fs_new_folder", "New folder")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RefreshCw size={12} />}
              onClick={refresh}
              disabled={loading || !host?.trim()}
              loading={loading}
            >
              {tr("fs_refresh", "Refresh")}
            </Button>
          </div>
        }
      />

      <ConnectionGate require="payload">
        {/* An open edit session means the browsed mount is a game image
            checked out of ShadowMount+ — and that the game is missing from
            the PS5 home screen until it's finished. This is the screen the
            user does the editing on, so the way back has to be here too. */}
        <div className="mb-2 empty:hidden">
          {/* Re-list on check-in: the mount the user was browsing has just
              been unmounted, so the current listing is gone. */}
          <EditSessionBanner onFinished={() => void refresh()} />
        </div>

        {/* Volume picker. Renders only when we have at least one
          writable volume — otherwise it's clutter. Selecting a
          volume jumps to its root, mirroring how Library's Move
          modal lets users pick a destination volume. The picker
          shows path + free-space so users can spot which mount has
          headroom for an upload at a glance. */}
        {/* Drives — internal + external/USB storage as a dedicated, always-
            visible category (rather than a separate Volumes section). Each
            chip jumps to that drive's root; external/USB drives get a USB icon
            + accent border so they stand out. The active drive is highlighted. */}
        {volumes && volumes.length > 0 && (
          <div className="mb-2 flex flex-wrap items-center gap-1.5 text-xs">
            <span className="mr-0.5 inline-flex items-center gap-1 text-[var(--color-muted)]">
              <HardDrive size={12} />
              {tr("fs_drives_label", undefined, "Drives")}
            </span>
            {volumes.map((v) => {
              const external =
                v.path.startsWith("/mnt/usb") || v.path.startsWith("/mnt/ext");
              const active = currentVolumePath === v.path;
              const Icon = external ? Usb : HardDrive;
              return (
                <button
                  key={v.path}
                  type="button"
                  onClick={() => setPath(v.path)}
                  title={`${v.path} · ${formatBytes(v.free_bytes)} ${tr("fs_free", "free")}`}
                  aria-current={active ? "true" : undefined}
                  // Exactly ONE thing is highlighted: the drive you are
                  // currently in. Nothing else competes for that meaning.
                  //
                  // Before, external drives carried a coloured (ps4) border
                  // whether or not they were active, so /mnt/ext1 always
                  // looked selected while /data lit up and dimmed as you
                  // moved — two different signals wearing the same clothes.
                  // Drive TYPE is now carried solely by the icon, leaving
                  // border and background to mean "you are here".
                  className={`inline-flex items-center gap-1 rounded-md border px-2 py-1 font-mono ${
                    active
                      ? "border-[var(--color-accent)] bg-[var(--color-accent)]/10 font-medium text-[var(--color-text)] ring-1 ring-[var(--color-accent)]"
                      : "border-[var(--color-border)] text-[var(--color-text)] hover:bg-[var(--color-surface)]"
                  }`}
                >
                  {/* Type lives in the icon: USB glyph for removable
                      drives, platter for internal. It stays tinted so the
                      distinction survives, without implying selection. */}
                  <Icon
                    size={12}
                    className={
                      external
                        ? "text-[var(--color-ps4)]"
                        : "text-[var(--color-muted)]"
                    }
                  />
                  {v.path}
                  <span className="opacity-60">
                    · {formatBytes(v.free_bytes)}
                  </span>
                </button>
              );
            })}
            {/* When the current path sits under no known volume, mirror the old
                picker's "(custom path)" affordance so the lack of an active
                chip isn't mysterious. */}
            {currentVolumePath === null && (
              <span className="rounded-md border border-dashed border-[var(--color-border)] px-2 py-1 font-mono text-[var(--color-muted)]">
                {tr("fs_volume_picker_custom", undefined, "(custom path)")}
              </span>
            )}
          </div>
        )}

        {/* Breadcrumbs + up-button.

            The crumbs scroll horizontally on their own (`overflow-x-auto` on
            the INNER row), but the Recent dropdown is a sibling OUTSIDE that
            scroll container. Critical: a parent with `overflow-x:auto` makes
            `overflow-y` compute to `auto` too (CSS spec), which would clip the
            dropdown's absolutely-positioned panel to the thin breadcrumb bar
            — the cramped/scrollbar'd overlay users saw. Keeping the dropdown
            out of the scrolling row lets its panel open freely. */}
        <div className="mb-3 flex items-center gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-xs">
          <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
            <button
              type="button"
              onClick={() => setPath(parent(path))}
              disabled={path === "/"}
              title={tr("fs_parent_dir", undefined, "Parent directory")}
              className="shrink-0 rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] disabled:opacity-30"
            >
              <ArrowUp size={14} />
            </button>
            {pathDraft !== null ? (
              <input
                autoFocus
                value={pathDraft}
                onChange={(ev) => setPathDraft(ev.target.value)}
                onKeyDown={(ev) => {
                  if (ev.key === "Enter") {
                    const next = normalizeTypedPath(pathDraft);
                    if (next) {
                      setPath(next);
                      setPathDraft(null);
                    }
                  }
                  if (ev.key === "Escape") setPathDraft(null);
                }}
                onBlur={() => setPathDraft(null)}
                spellCheck={false}
                aria-label={tr("fs_go_to_path", undefined, "Go to path")}
                className="min-w-0 flex-1 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-0.5 font-mono text-xs"
              />
            ) : (
              <>
                {crumbs(path).map((c, i, arr) => (
                  <span
                    key={c.path}
                    className="flex shrink-0 items-center gap-1"
                  >
                    <button
                      type="button"
                      onClick={() => setPath(c.path)}
                      className={
                        "rounded px-1.5 py-0.5 font-mono " +
                        (i === arr.length - 1
                          ? "font-medium text-[var(--color-text)]"
                          : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]")
                      }
                    >
                      {i === 0 ? (
                        <Home size={12} className="inline -translate-y-[1px]" />
                      ) : (
                        c.label
                      )}
                    </button>
                    {i < arr.length - 1 && (
                      <ChevronRight
                        size={12}
                        className="text-[var(--color-muted)]"
                      />
                    )}
                  </span>
                ))}
                <button
                  type="button"
                  onClick={() => setPathDraft(path)}
                  title={tr("fs_go_to_path", undefined, "Go to path")}
                  className="ml-1 shrink-0 rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
                >
                  <Pencil size={12} />
                </button>
              </>
            )}
          </div>
          <RecentPathsDropdown onPick={(p) => setPath(p)} currentPath={path} />
        </div>

        {/* Clipboard banner: non-empty clipboard shows what's staged and
          where it came from; paste target is always the current path. */}
        {clipboardActive && (
          <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-2 text-xs">
            <ClipboardPaste size={14} className="text-[var(--color-accent)]" />
            <span>
              <span className="font-medium">
                {tr(
                  clipboard.items.length === 1 ? "fs_item_one" : "fs_item_many",
                  { count: clipboard.items.length },
                  `${clipboard.items.length} item${clipboard.items.length === 1 ? "" : "s"}`,
                )}
              </span>{" "}
              {clipboard.op === "cut"
                ? tr("fs_to_move", undefined, "to move")
                : tr("fs_to_copy", undefined, "to copy")}
              {clipboard.sourceLabel
                ? ` ${tr("fs_from", undefined, "from")} ${clipboard.sourceLabel}`
                : ""}
            </span>
            <div className="ml-auto flex items-center gap-1">
              <button
                type="button"
                onClick={() => void runPaste()}
                disabled={bulkOp.op !== null || !host?.trim()}
                className="flex items-center gap-1 rounded-md bg-[var(--color-accent)] px-2 py-1 text-xs font-medium text-[var(--color-accent-contrast)] disabled:opacity-50"
              >
                {tr("fs_paste_here", undefined, "Paste here")}
              </button>
              <button
                type="button"
                onClick={() => clipboard.clear()}
                className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
                title={tr(
                  "fs_cancel_clipboard",
                  undefined,
                  "Cancel — forget the staged items",
                )}
              >
                <X size={12} />
              </button>
            </div>
          </div>
        )}

        {/* Selection toolbar: only visible with ≥1 selected. */}
        {selectionActive && (
          <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-2 text-xs">
            <span className="font-medium">
              {tr(
                "fs_selected_count",
                { count: selected.size },
                `${selected.size} selected`,
              )}
            </span>
            <button
              type="button"
              onClick={() => startMove()}
              disabled={bulkOp.op !== null || !volumes?.length}
              className="flex items-center gap-1 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface)] px-2 py-1 font-medium hover:bg-[var(--color-surface-3)] disabled:opacity-50"
            >
              <FolderInput size={12} />
              {tr("fs_move_to", undefined, "Move to…")}
            </button>
            <button
              type="button"
              onClick={() => stageClipboard("cut")}
              disabled={bulkOp.op !== null}
              className="flex items-center gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 hover:bg-[var(--color-surface-3)] disabled:opacity-50"
            >
              <Scissors size={12} />
              {tr("fs_cut", undefined, "Cut")}
            </button>
            <button
              type="button"
              onClick={() => stageClipboard("copy")}
              disabled={bulkOp.op !== null}
              className="flex items-center gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 hover:bg-[var(--color-surface-3)] disabled:opacity-50"
            >
              <Copy size={12} />
              {tr("copy", undefined, "Copy")}
            </button>
            <button
              type="button"
              onClick={() => void runChmod777()}
              disabled={bulkOp.op !== null || chmodBusy}
              className="flex items-center gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 hover:bg-[var(--color-surface-3)] disabled:opacity-50"
            >
              <KeyRound size={12} />
              {tr("fs_chmod_777", undefined, "Set permissions (777)")}
            </button>
            <button
              type="button"
              onClick={() => runBulkRename(Array.from(selected))}
              disabled={bulkOp.op !== null || selected.size === 0}
              title={tr(
                "fs_bulk_rename_tooltip",
                undefined,
                "Bulk rename via pattern (s/old/new/, ^prefix_, _suffix$, lower, upper)",
              )}
              className="flex items-center gap-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 hover:bg-[var(--color-surface-3)] disabled:opacity-50"
            >
              <Pencil size={12} />
              {tr("fs_bulk_rename", undefined, "Bulk rename")}
            </button>
            <button
              type="button"
              onClick={runBulkDelete}
              disabled={bulkOp.op !== null}
              className="flex items-center gap-1 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] px-2 py-1 text-[var(--color-bad)] hover:bg-[var(--color-surface-3)] disabled:opacity-50"
            >
              <Trash2 size={12} />
              {tr("library_delete", undefined, "Delete")}
            </button>
            <button
              type="button"
              onClick={() => setSelected(new Set())}
              className="ml-auto rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
              title={tr("fs_clear_selection", undefined, "Clear selection")}
            >
              <X size={12} />
            </button>
          </div>
        )}

        {mkdirDraft !== null && (
          <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-sm">
            <FolderPlus size={14} className="text-[var(--color-muted)]" />
            <span className="text-xs text-[var(--color-muted)]">
              {tr("fs_create_in", undefined, "Create in")}{" "}
              <span className="font-mono">{path}</span>
            </span>
            <input
              autoFocus
              value={mkdirDraft}
              placeholder={tr("fs_folder_name", undefined, "folder name")}
              onChange={(e) => setMkdirDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") runMkdir();
                if (e.key === "Escape") setMkdirDraft(null);
              }}
              className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
            />
            <button
              type="button"
              onClick={runMkdir}
              className="rounded-md bg-[var(--color-accent)] px-3 py-1 text-xs font-medium text-[var(--color-accent-contrast)]"
            >
              {tr("fs_create", undefined, "Create")}
            </button>
            <button
              type="button"
              onClick={() => setMkdirDraft(null)}
              className="rounded-md border border-[var(--color-border)] px-3 py-1 text-xs hover:bg-[var(--color-surface-3)]"
            >
              {tr("cancel", undefined, "Cancel")}
            </button>
          </div>
        )}

        {bulkOpHere && bulkOp.op !== null && (
          <BulkOpBanner
            host={host}
            op={bulkOp.op}
            total={bulkOp.total}
            done={bulkOp.done}
            currentName={bulkOp.currentName}
            currentSize={bulkOp.currentSize}
            fromPath={bulkOp.fromPath}
            toPath={bulkOp.toPath}
            startedAtMs={bulkOp.startedAtMs}
          />
        )}

        {/* An op running on ANOTHER console no longer blocks this one (ops are
          per-console now), but we still surface it so the user knows work is
          happening elsewhere and where to look — without rendering the other
          console's progress here as if it were this console's. */}
        {(bulkOpElsewhere || downloadElsewhere) && (
          <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-xs text-[var(--color-muted)]">
            <Spinner size={12} />
            <span>
              {tr(
                "fs_op_on_other_console",
                {
                  name: profileNameForHost(
                    bulkOpElsewhereHost ?? downloadElsewhereHost ?? "",
                    rosterProfiles,
                  ),
                },
                "A file operation is running on {name} — switch to that console's tab to see its progress.",
              )}
            </span>
          </div>
        )}

        {busyEntry && bulkOp.op === null && (
          <div className="mb-3 flex flex-wrap items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-xs">
            <Spinner size={12} tone="accent" />
            <span className="font-medium">
              {busyEntry.op === "rename"
                ? tr("fs_busy_renaming", undefined, "Renaming")
                : busyEntry.op === "upload"
                  ? tr("fs_busy_uploading", undefined, "Copying to PS5")
                  : tr("fs_busy_creating_folder", undefined, "Creating folder")}
            </span>
            <span className="text-[var(--color-muted)]">
              {busyEntry.name} ·{" "}
              {busyEntry.op === "upload" && uploadStartedAtMs > 0 ? (
                <ElapsedSince sinceMs={uploadStartedAtMs} />
              ) : (
                formatDuration(elapsedMs / 1000)
              )}
            </span>
            {busyEntry.op === "upload" && (
              <FsUploadProgress host={host} onCancel={cancelUpload} />
            )}
          </div>
        )}

        {downloadHere && (
          <DownloadOpBanner
            host={host}
            rootName={downloadOp.rootName}
            rootSrcPath={downloadOp.rootSrcPath}
            destDir={downloadOp.destDir}
            startedAtMs={downloadOp.startedAtMs}
          />
        )}

        {error && (
          <div className="mb-3">
            <ErrorCard title={tr("fs_error_title", "Error")} detail={error} />
          </div>
        )}

        {/* An "Add files" upload that ended on a failure. It lives in the store, so it is
          here on every visit until the user resumes it or dismisses it. */}
        {uploadStopped && uploadName === null && (
          <div
            className={`mb-3 rounded-md border bg-[var(--color-surface-2)] p-3 text-sm ${uploadStopped.why === "failed" ? "border-[var(--color-bad)]" : "border-[var(--color-border)]"}`}
            data-testid="fs-upload-stopped"
          >
            <div className="font-medium">
              {uploadStopped.why === "user"
                ? tr(
                    "fs_upload_stopped_by_user",
                    stoppedAt,
                    "You stopped this copy at {name} ({done} of {count} done)",
                  )
                : uploadStopped.why === "interrupted"
                  ? tr(
                      "fs_upload_stopped_interrupted",
                      stoppedAt,
                      "This copy was interrupted when the app closed, at {name} ({done} of {count} done)",
                    )
                  : tr(
                      "fs_upload_stopped_title",
                      stoppedAt,
                      "Copy to PS5 stopped at {name} ({done} of {count} done)",
                    )}
            </div>
            {uploadStopped.error && (
              <div className="mt-1 break-words text-xs text-[var(--color-muted)]">
                {humanizePs5Error(uploadStopped.error) || uploadStopped.error}
              </div>
            )}
            <div className="mt-1 text-xs text-[var(--color-muted)]">
              {tr(
                "fs_upload_stopped_hint",
                undefined,
                "What already reached the PS5 is kept. Resume sends only the rest.",
              )}
            </div>
            <div className="mt-2 flex gap-2">
              <Button
                size="sm"
                onClick={() => void resumeFsUpload(host, fsUploadDeps)}
              >
                {tr("upload_dialog_resume", undefined, "Resume")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => dismissFsUploadStopped(host)}
              >
                {tr("dismiss", undefined, "Dismiss")}
              </Button>
            </div>
          </div>
        )}

        {/* Errors written to the bulk-op or download stores are
          surfaced here too — local `error` state is lost on screen
          unmount, but the store survives navigation. Without this
          banner, a paste/delete/download that fails while the user
          is on another tab silently drops the error and the user
          gets no signal that the op finished badly. Each banner is
          dismissible (clearError) so it doesn't block subsequent
          operations. */}
        {bulkOp.errorBanner && (
          <div className="mb-3">
            <ErrorCard
              title={tr("fs_bulk_error_title", "Error")}
              detail={bulkOp.errorBanner}
              onDismiss={() => fsBulk.clearError()}
            />
          </div>
        )}

        {downloadOp.errorBanner && !downloadOp.active && (
          <div className="mb-3">
            <ErrorCard
              title={tr("fs_download_error_title", "Download failed")}
              detail={downloadOp.errorBanner}
              onDismiss={() => fsDownload.clearError()}
            />
          </div>
        )}

        {/* No entries yet and no error → we're connected (this whole block is
            inside ConnectionGate require="payload") and simply still listing
            the directory. Show a loading state, not the old "connect first"
            message — by here the console is already reachable, so that copy
            was both stale and confusing. */}
        {entries === null && !error && (
          <div className="flex items-center justify-center gap-2 py-10 text-xs text-[var(--color-muted)]">
            <Spinner size={14} />
            {tr("fs_listing", undefined, "Reading directory…")}
          </div>
        )}

        {/* Right-click on the list's empty space opens the folder menu (New folder,
            Paste, Refresh); a row's own handler stops the event first. */}
        <div
          data-testid="fs-list-area"
          className="min-h-[12rem]"
          onContextMenu={(ev) => {
            if (!entries) return;
            ev.preventDefault();
            setRowMenu({ x: ev.clientX, y: ev.clientY, entry: null });
          }}
        >
          {entries && entries.length === 0 && (
            <div className="rounded-md border border-dashed border-[var(--color-border)] p-4 text-center text-xs text-[var(--color-muted)]">
              {tr("fs_empty_folder", "Empty folder.")}
            </div>
          )}

          {entries && entries.length > 0 && (
            <div className="mb-1 flex items-center gap-2 px-2 text-xs text-[var(--color-muted)]">
              <input
                type="checkbox"
                checked={selected.size > 0 && selected.size === entries.length}
                ref={(el) => {
                  if (el)
                    el.indeterminate =
                      selected.size > 0 && selected.size < entries.length;
                }}
                onChange={toggleAll}
                className="h-3.5 w-3.5 rounded border-[var(--color-border)]"
                title={tr("fs_select_all", undefined, "Select all")}
              />
              <span>
                {tr(
                  entries.length === 1 ? "fs_item_one" : "fs_item_many",
                  { count: entries.length },
                  `${entries.length} item${entries.length === 1 ? "" : "s"}`,
                )}
              </span>
              <span className="ml-auto flex items-center gap-1">
                {(
                  [
                    ["name", tr("fs_sort_name", undefined, "Name")],
                    ["size", tr("fs_sort_size", undefined, "Size")],
                    ["mtime", tr("fs_sort_modified", undefined, "Modified")],
                  ] as [SortKey, string][]
                ).map(([k, label]) => (
                  <button
                    key={k}
                    type="button"
                    onClick={() => setSort((cur) => nextSort(cur, k))}
                    className={
                      "rounded px-1.5 py-0.5 hover:bg-[var(--color-surface-3)] " +
                      (sort.key === k
                        ? "font-medium text-[var(--color-text)]"
                        : "")
                    }
                    aria-pressed={sort.key === k}
                  >
                    {label}
                    {sort.key === k ? (sort.desc ? " ↓" : " ↑") : ""}
                  </button>
                ))}
              </span>
            </div>
          )}

          <ul className="grid gap-1">
            {sortedEntries.map((e) => {
              const isDir = e.kind === "dir";
              const Icon = isDir ? Folder : FileIcon;
              const isRenaming = renaming === e.name;
              const isSelected = selected.has(e.name);
              return (
                <li
                  key={e.name}
                  onDoubleClick={(ev) => {
                    if ((ev.target as HTMLElement).closest("input,button"))
                      return;
                    openEntry(e);
                  }}
                  onContextMenu={(ev) => {
                    ev.preventDefault();
                    ev.stopPropagation();
                    setRowMenu({ x: ev.clientX, y: ev.clientY, entry: e });
                  }}
                  className={
                    "list-row-contain-sm flex items-center gap-3 rounded-md border p-2 text-sm " +
                    (isSelected
                      ? "border-[var(--color-accent)] bg-[var(--color-surface-2)]"
                      : "border-[var(--color-border)] bg-[var(--color-surface-2)]")
                  }
                >
                  <input
                    type="checkbox"
                    checked={isSelected}
                    onChange={() => toggleSelected(e.name)}
                    className="h-3.5 w-3.5 shrink-0 rounded border-[var(--color-border)]"
                  />
                  <Icon
                    size={16}
                    className={
                      isDir
                        ? "shrink-0 text-[var(--color-accent)]"
                        : "shrink-0 text-[var(--color-muted)]"
                    }
                  />
                  {isRenaming ? (
                    <input
                      autoFocus
                      value={renameDraft}
                      onChange={(ev) => setRenameDraft(ev.target.value)}
                      onKeyDown={(ev) => {
                        if (ev.key === "Enter") runRename(e.name);
                        if (ev.key === "Escape") setRenaming(null);
                      }}
                      onBlur={() => setRenaming(null)}
                      className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-0.5 text-sm"
                    />
                  ) : (
                    <button
                      type="button"
                      onClick={() => {
                        if (isDir) setPath(joinPath(path, e.name));
                      }}
                      className={
                        "flex-1 truncate text-left font-mono text-sm " +
                        (isDir ? "hover:text-[var(--color-accent)]" : "")
                      }
                      disabled={!isDir}
                    >
                      {e.name}
                    </button>
                  )}
                  <span className="shrink-0 text-xs text-[var(--color-muted)] tabular-nums">
                    {isDir ? "—" : formatBytes(e.size)}
                  </span>
                  <div className="ml-2 flex shrink-0 items-center gap-1">
                    {isTauriEnv() && (
                      <>
                        <button
                          type="button"
                          onClick={() => runDownload(e)}
                          disabled={downloadOp.active}
                          title={tr(
                            "fs_download_tooltip",
                            undefined,
                            "Save a copy of this entry to a folder on this computer",
                          )}
                          className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)] disabled:opacity-30"
                        >
                          {downloadHere && downloadOp.rootName === e.name ? (
                            <Spinner size={12} tone="accent" />
                          ) : (
                            <Download size={12} />
                          )}
                        </button>
                        <button
                          type="button"
                          onClick={() => runDownload(e, true)}
                          disabled={downloadOp.active}
                          title={tr(
                            "fs_download_zip_tooltip",
                            undefined,
                            "Download to this computer as a .zip (streamed — no temp copy)",
                          )}
                          className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)] disabled:opacity-30"
                        >
                          <FileArchive size={12} />
                        </button>
                      </>
                    )}
                    <button
                      type="button"
                      onClick={() => {
                        setRenaming(e.name);
                        setRenameDraft(e.name);
                      }}
                      title={tr("fs_rename", undefined, "Rename")}
                      className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
                    >
                      <Pencil size={12} />
                    </button>
                    {!isDir && (
                      <button
                        type="button"
                        onClick={() => replaceEntry(e)}
                        disabled={busyEntry !== null}
                        aria-label={tr("fs_replace", undefined, "Replace file")}
                        title={tr(
                          "fs_replace_tooltip",
                          undefined,
                          "Overwrite this file with one from your computer — how you patch a file inside a mounted game image",
                        )}
                        className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)] disabled:opacity-40"
                      >
                        <Upload size={12} />
                      </button>
                    )}
                    {viewableEntry(e.name, isDir) && host && (
                      <button
                        type="button"
                        onClick={() => viewEntry(e)}
                        aria-label={tr("viewer_open", undefined, "View details")}
                        title={tr("viewer_open", undefined, "View details")}
                        className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
                      >
                        <ScanSearch size={12} />
                      </button>
                    )}
                    {!isDir && e.size <= 256 * 1024 && (
                      <button
                        type="button"
                        onClick={() => runPreview(e)}
                        aria-label={tr("fs_preview", undefined, "Preview")}
                        title={tr(
                          "fs_preview_tooltip",
                          undefined,
                          "Preview small file inline (text or image)",
                        )}
                        className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
                      >
                        <Eye size={12} />
                      </button>
                    )}
                    {!isDir && (
                      <button
                        type="button"
                        onClick={() => runCrc32(e)}
                        aria-label={tr("fs_crc32", undefined, "CRC32 checksum")}
                        title={tr(
                          "fs_crc32_tooltip",
                          undefined,
                          "Compute CRC32 of this file (cheap integrity check)",
                        )}
                        className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
                      >
                        <Hash size={12} />
                      </button>
                    )}
                    {!isDir && (
                      <button
                        type="button"
                        onClick={() => runVerify(e)}
                        aria-label={tr("fs_verify", undefined, "Verify (BLAKE3)")}
                        title={tr(
                          "fs_verify_tooltip",
                          undefined,
                          "BLAKE3 + CRC32 verification (slower, crypto-strength)",
                        )}
                        className="rounded-md border border-[var(--color-border)] p-1 hover:bg-[var(--color-surface-3)]"
                      >
                        <BadgeCheck size={12} />
                      </button>
                    )}
                    {!isDir && isInstallPackagePath(e.name) && (
                      <button
                        type="button"
                        onClick={() => void runInstallPkg(e)}
                        disabled={pkgInstalling}
                        title={tr(
                          "fs_install_pkg_tooltip",
                          undefined,
                          "Install this package on your PS5",
                        )}
                        className="rounded-md border border-[var(--color-accent)] p-1 text-[var(--color-accent)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
                      >
                        {installingPkgName === e.name ? (
                          <Spinner size={12} tone="inherit" />
                        ) : (
                          <PackagePlus size={12} />
                        )}
                      </button>
                    )}
                    <button
                      type="button"
                      onClick={() => runDelete(e.name)}
                      title={tr("library_delete", undefined, "Delete")}
                      className="rounded-md border border-[var(--color-bad)] p-1 text-[var(--color-bad)] hover:bg-[var(--color-surface-3)]"
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        </div>
        {preview && (
          <div
            className="fixed inset-0 z-50 flex items-center justify-center bg-[var(--overlay-scrim)] p-4"
            onClick={() => setPreview(null)}
          >
            <div
              className="flex max-h-[90dvh] w-full max-w-3xl flex-col overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)]"
              onClick={(e) => e.stopPropagation()}
            >
              <header className="flex items-center justify-between border-b border-[var(--color-border)] px-4 py-2">
                <div>
                  <div className="text-sm font-semibold">{preview.name}</div>
                  <div className="text-xs text-[var(--color-muted)]">
                    {formatBytes(preview.size)} · {preview.kind}
                  </div>
                </div>
                <button
                  type="button"
                  onClick={() => setPreview(null)}
                  className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
                >
                  ✕
                </button>
              </header>
              <div className="flex-1 overflow-auto p-3">
                {preview.kind === "image" ? (
                  <img
                    src={preview.body}
                    alt={preview.name}
                    className="mx-auto max-h-full"
                  />
                ) : (
                  <pre className="whitespace-pre-wrap break-all font-mono text-xs">
                    {preview.body}
                  </pre>
                )}
              </div>
            </div>
          </div>
        )}
      </ConnectionGate>
    </div>
  );
}

/**
 * Rich progress banner for bulk file ops (delete, paste-move, paste-copy).
 *
 * Shows the per-file size + source/destination paths so users know
 * what's actually moving and where it's going. The progress bar is
 * "determinate by file count" but the per-file copy is still an
 * opaque payload-side fs_copy — for a single 10 GiB file the bar
 * sits at 0% for the whole copy. We layer an indeterminate animated
 * stripe on top so the bar still LOOKS alive during long single-
 * file copies. Real byte-level progress would need payload-side
 * incremental fs_copy events; future work.
 */
/** Compact "recent paths" dropdown that lives next to the breadcrumb
 *  trail. Hidden when the recents list is empty (no point showing an
 *  empty button). Positioned to the right of the breadcrumbs so it
 *  doesn't push the active path off-screen on small windows. */
function RecentPathsDropdown({
  onPick,
  currentPath,
}: {
  onPick: (p: string) => void;
  currentPath: string;
}) {
  const tr = useTr();
  const paths = useRecentPathsStore((s) => s.paths);
  const remove = useRecentPathsStore((s) => s.remove);
  const clear = useRecentPathsStore((s) => s.clear);
  const [open, setOpen] = useState(false);

  // Filter out the current path — no value in offering "go where you
  // already are." Keeps the list focused on actual destinations.
  const filtered = useMemo(
    () => paths.filter((p) => p !== currentPath),
    [paths, currentPath],
  );

  if (filtered.length === 0) return null;

  return (
    <div className="relative ml-auto">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        title={tr("fs_recent_tooltip", undefined, "Recent destinations")}
        className="flex items-center gap-1 rounded px-1.5 py-0.5 text-xs uppercase tracking-wider text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
      >
        {tr("fs_recent", "Recent")}
        <ChevronRight
          size={10}
          className={`transition-transform ${open ? "rotate-90" : ""}`}
        />
      </button>
      {open && (
        <div className="absolute right-0 top-full z-30 mt-1 w-72 overflow-hidden rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] shadow-lg">
          <ul className="max-h-60 overflow-y-auto py-1">
            {filtered.map((p) => (
              <li key={p} className="flex items-center">
                <button
                  type="button"
                  onClick={() => {
                    onPick(p);
                    setOpen(false);
                  }}
                  className="min-w-0 flex-1 truncate px-2 py-1 text-left font-mono text-xs hover:bg-[var(--color-surface-3)]"
                  title={p}
                >
                  {p}
                </button>
                <button
                  type="button"
                  onClick={() => remove(p)}
                  className="px-1 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-bad)]"
                  title={tr("fs_recent_remove", undefined, "Forget this entry")}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
          <button
            type="button"
            onClick={() => {
              clear();
              setOpen(false);
            }}
            className="block w-full border-t border-[var(--color-border)] px-2 py-1 text-left text-xs text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
          >
            {tr("fs_recent_clear", undefined, "Clear all")}
          </button>
        </div>
      )}
    </div>
  );
}

/** Byte progress of an "Add files" upload (see `runUpload`). */
type UploadProgress = Pick<
  FsUploadActive,
  "sent" | "total" | "index" | "count" | "jobId" | "live" | "retry"
>;

/** Time since `sinceMs`, ticking: a run that outlives the screen keeps its real age. */
function ElapsedSince({ sinceMs }: { sinceMs: number }) {
  const [now, setNow] = useState(sinceMs);
  useEffect(() => {
    const tick = () => setNow(Date.now());
    tick();
    const t = window.setInterval(tick, 500);
    return () => window.clearInterval(t);
  }, [sinceMs]);
  return <>{formatDuration(Math.max(0, now - sinceMs) / 1000)}</>;
}

/** The console's running upload, read straight from the store so only this part of the
 *  screen re-renders at the poll rate. */
function FsUploadProgress({
  host,
  onCancel,
}: {
  host: string;
  onCancel: () => void;
}) {
  const active = useFsUploadStore((s) => fsUploadForHost(s, host).active);
  if (!active) return null;
  return <UploadProgressDetail progress={active} onCancel={onCancel} />;
}

/** Bytes, percent, speed, time left and a bar for an "Add files" upload, and Cancel. Once every
 *  byte is sent the console still has to make the files permanent: that phase says so (with the
 *  console's own count when it sends one) instead of sitting at 100%. */
function UploadProgressDetail({
  progress,
  onCancel,
}: {
  progress: UploadProgress;
  onCancel: () => void;
}) {
  const tr = useTr();
  const { sent, total, index, count, jobId, live, retry } = progress;
  const { rate, etaSeconds } = useRateEta(jobId, sent, total);
  const pct = total > 0 ? Math.min(100, (sent / total) * 100) : null;
  const finishing = total > 0 && sent >= total;
  const settleLeft = live?.settling ? live.settleLeft : undefined;
  return (
    <div className="mt-1 basis-full" data-testid="fs-upload-progress">
      {retry && (
        <div
          className="mb-1 text-[var(--color-warn)]"
          data-testid="fs-upload-retrying"
        >
          {tr(
            "fs_upload_retrying",
            { attempt: retry.attempt, of: retry.of },
            "Connection lost. Trying again ({attempt} of {of})…",
          )}
        </div>
      )}
      <div className="mb-1 flex flex-wrap items-center gap-x-2 font-mono text-[var(--color-muted)]">
        {count > 1 && (
          <span>
            {tr(
              "fs_bulk_progress",
              { done: index + 1, total: count },
              "{done} of {total}",
            )}
          </span>
        )}
        <span>
          {pct !== null
            ? `${formatBytes(sent)} / ${formatBytes(total)} (${pct.toFixed(0)}%)`
            : formatBytes(sent)}
        </span>
        {!finishing && rate > 0 && <span>{formatBytes(rate)}/s</span>}
        {!finishing && etaSeconds !== null && (
          <span>
            {tr(
              "fs_progress_eta",
              { time: formatEtaSeconds(etaSeconds) },
              `about ${formatEtaSeconds(etaSeconds)} left`,
            )}
          </span>
        )}
        {finishing && (
          <span className="text-[var(--color-warn)]">
            {tr(
              "upload_phase_settling",
              undefined,
              "Finishing on the console…",
            )}
            {settleLeft !== undefined &&
              ` ${tr(
                "fs_finishing_files_left",
                { left: settleLeft.toLocaleString() },
                `${settleLeft.toLocaleString()} files left`,
              )}`}
          </span>
        )}
        <button
          type="button"
          onClick={onCancel}
          className="ml-auto rounded-md border border-[var(--color-border)] px-2 py-0.5 hover:bg-[var(--color-surface-3)]"
        >
          {tr("cancel", undefined, "Cancel")}
        </button>
      </div>
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
        <div
          className={`h-full bg-[var(--color-accent)] transition-[width] duration-300 ${
            pct === null || finishing ? "animate-pulse" : ""
          }`}
          style={{ width: `${Math.max(pct ?? 0, 4)}%` }}
        />
      </div>
    </div>
  );
}

function BulkOpBanner({
  host,
  op,
  total,
  done,
  currentName,
  currentSize,
  fromPath,
  toPath,
  startedAtMs,
}: {
  /** Console this banner belongs to — scopes the per-tick subscriptions
   *  and the Stop button to this console's bulk-op slot. */
  host: string;
  op: "delete" | "paste-move" | "paste-copy";
  total: number;
  done: number;
  currentName: string;
  currentSize: number | null;
  fromPath: string;
  toPath: string;
  startedAtMs: number;
}) {
  const tr = useTr();
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(id);
  }, []);
  const elapsedSec =
    startedAtMs > 0 ? Math.max(0, (now - startedAtMs) / 1000) : 0;
  const pctByFiles = total > 0 ? Math.min(100, (done / total) * 100) : 0;
  const verb =
    op === "delete"
      ? tr("library_busy_delete", undefined, "Deleting")
      : op === "paste-move"
        ? tr("fs_busy_moving", undefined, "Moving")
        : tr("fs_busy_copying", undefined, "Copying");

  const cancelRequested = useFsBulkOpStore(
    (s) => bulkOpForHost(s, host).cancelRequested,
  );
  // Per-item byte progress fed by the paste-loop poller via
  // FS_OP_STATUS. 0 for delete (no per-byte concept) and during
  // the brief window between item-start and the first poll reply.
  const currentBytesCopied = useFsBulkOpStore(
    (s) => bulkOpForHost(s, host).currentBytesCopied,
  );
  const itemPct =
    currentSize !== null && currentSize > 0 && currentBytesCopied > 0
      ? Math.min(100, (currentBytesCopied / currentSize) * 100)
      : null;
  // Rate and time left for the item being copied, from the byte counter the poller
  // keeps current (a trailing window: a big copy moves in bursts). The key restarts the
  // window for each item, so an earlier item's pace never leaks into the next one.
  const { rate: itemSpeed, etaSeconds: itemEta } = useRateEta(
    `${currentName}|${done}`,
    currentBytesCopied,
    currentSize ?? 0,
  );
  // Every byte is written but the item is not done yet: the console is renaming into
  // place (and, for a move, deleting the original). It moves no bytes, so say so rather
  // than sit on 100%.
  const finishing =
    op !== "delete" &&
    currentSize !== null &&
    currentSize > 0 &&
    currentBytesCopied >= currentSize;
  // Whole-batch bar: finished items plus the current item's fraction.
  const pctOverall =
    total > 0
      ? Math.min(100, ((done + (itemPct ?? 0) / 100) / total) * 100)
      : pctByFiles;

  return (
    <div className="mb-3 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-3 text-xs">
      <div className="mb-2 flex items-center gap-2">
        <Spinner size={14} tone="accent" />
        <span className="font-semibold">{verb}</span>
        <span className="text-[var(--color-muted)]">
          {tr(
            "fs_bulk_progress",
            { done: done + 1, total },
            "{done} of {total}",
          )}
          {" · "}
          {formatDuration(elapsedSec)}
          {!finishing && itemSpeed > 0 && ` · ${formatBytes(itemSpeed)}/s`}
          {!finishing &&
            itemEta !== null &&
            ` · ${tr(
              "fs_progress_eta",
              { time: formatEtaSeconds(itemEta) },
              `about ${formatEtaSeconds(itemEta)} left`,
            )}`}
          {finishing &&
            ` · ${tr("upload_phase_settling", undefined, "Finishing on the console…")}`}
        </span>
        {/* Cancel drives a real cancel: the loop asks the engine to end the console's
            copy job (job.cancel), which stops within a moment however big the file is, and
            the engine then removes the half-written destination. The source is never
            touched. Delete has no mid-file cancel, so its button waits for the item. */}
        <button
          type="button"
          onClick={() => useFsBulkOpStore.getState().requestCancel(host)}
          disabled={cancelRequested}
          data-testid="fs-bulk-cancel"
          className="ml-auto rounded-md border border-[var(--color-border)] px-2 py-0.5 text-xs hover:bg-[var(--color-surface-3)] disabled:opacity-50"
          title={
            op === "delete"
              ? tr(
                  "fs_bulk_stop_tooltip",
                  undefined,
                  "Stop after the current item finishes",
                )
              : tr(
                  "fs_bulk_cancel_copy_tooltip",
                  undefined,
                  "Cancel the current copy and skip the rest. The original is not touched; the half-copied files are removed.",
                )
          }
        >
          {cancelRequested
            ? tr("fs_bulk_stopping", undefined, "Stopping…")
            : op === "delete"
              ? tr("fs_bulk_stop", undefined, "Stop")
              : tr("fs_bulk_cancel_copy", undefined, "Cancel copy")}
        </button>
      </div>

      {currentName && (
        <div className="mb-2 flex flex-wrap items-baseline gap-x-3 gap-y-0.5 font-mono text-xs">
          <span className="text-[var(--color-text)]">{currentName}</span>
          {currentBytesCopied > 0 && currentSize !== null && currentSize > 0 ? (
            <span className="text-[var(--color-muted)]">
              {formatBytes(currentBytesCopied)} / {formatBytes(currentSize)}
              {itemPct !== null && ` (${itemPct.toFixed(0)}%)`}
            </span>
          ) : currentSize !== null && currentSize > 0 ? (
            <span className="text-[var(--color-muted)]">
              {formatBytes(currentSize)}
            </span>
          ) : null}
        </div>
      )}

      {/* Per-item progress bar. Only renders when we have both bytes
          and a total — avoids a misleading 0% bar while the first
          FS_OP_STATUS reply is in flight. */}
      {currentBytesCopied > 0 &&
        currentSize !== null &&
        currentSize > 0 &&
        itemPct !== null && (
          <div className="mb-2 h-1 overflow-hidden rounded-full bg-[var(--color-surface-3)]">
            <div
              className="h-full bg-[var(--color-accent)] transition-[width] duration-300"
              style={{ width: `${itemPct}%` }}
            />
          </div>
        )}

      {cancelRequested && op !== "delete" && (
        <div className="mb-2 rounded-md border border-[var(--color-warn)] bg-[var(--color-warn-soft)] p-2 text-xs text-[var(--color-warn)]">
          {tr(
            "fs_bulk_cancel_copy_explainer",
            undefined,
            "Cancelling: the PS5 is stopping the copy and removing the half-copied files. Your original files are not touched.",
          )}
        </div>
      )}

      {cancelRequested && op === "delete" && (
        // Delete has no per-byte cancel; explain why it's slower.
        <div className="mb-2 rounded-md border border-[var(--color-warn)] bg-[var(--color-warn-soft)] p-2 text-xs text-[var(--color-warn)]">
          {tr(
            "fs_bulk_stop_explainer_delete",
            { name: currentName },
            "Waiting for the current item ({name}) to finish on the PS5 — fs_delete can't be interrupted mid-file. The next items in this batch will be skipped.",
          )}
        </div>
      )}

      {(fromPath || toPath) && (
        <div className="mb-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-xs text-[var(--color-muted)]">
          {fromPath && (
            <>
              <span>{tr("fs_bulk_from", undefined, "From")}</span>
              <span className="break-all font-mono">{fromPath}</span>
            </>
          )}
          {toPath && op !== "delete" && (
            <>
              <span>{tr("fs_bulk_to", undefined, "To")}</span>
              <span className="break-all font-mono">{toPath}</span>
            </>
          )}
        </div>
      )}

      {/* Determinate-by-file-count bar with a subtle pulse so single-
          file copies (where the bar sits at 0% the whole copy) still
          LOOK alive. The spinner icon at top is the secondary
          motion signal. Real byte-level progress would need
          payload-side incremental fs_copy events; future work. */}
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
        <div
          className={`h-full bg-[var(--color-accent)] transition-[width] duration-300 ${
            done < total && (itemPct === null || finishing)
              ? "animate-pulse"
              : ""
          }`}
          style={{ width: `${Math.max(pctOverall, 4)}%` }}
        />
      </div>
    </div>
  );
}

/**
 * Rich progress banner for downloads. Similar shape to BulkOpBanner
 * but with real bytes/total since download progresses are streamed
 * (the engine sums fs_read chunks into the shared progress AtomicU64).
 */
function DownloadOpBanner({
  host,
  rootName,
  rootSrcPath,
  destDir,
  startedAtMs,
}: {
  /** Console this download belongs to — scopes the per-tick byte
   *  subscriptions and the Stop button to this console's slot. */
  host: string;
  rootName: string;
  rootSrcPath: string;
  destDir: string;
  startedAtMs: number;
}) {
  const tr = useTr();
  // Byte progress is subscribed HERE rather than passed via props so
  // the per-poll-tick store writes only re-render this banner, not the
  // whole FileSystem screen (same pattern as BulkOpBanner's
  // currentBytesCopied).
  const bytesReceived = useFsDownloadOpStore(
    (s) => downloadForHost(s, host).bytesReceived,
  );
  const totalBytes = useFsDownloadOpStore(
    (s) => downloadForHost(s, host).totalBytes,
  );
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(id);
  }, []);
  const elapsedSec =
    startedAtMs > 0 ? Math.max(0, (now - startedAtMs) / 1000) : 0;
  const pct =
    totalBytes > 0 ? Math.min(100, (bytesReceived / totalBytes) * 100) : 0;
  const speed = elapsedSec > 0 ? bytesReceived / elapsedSec : 0;

  return (
    <div className="mb-3 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-3 text-xs">
      <div className="mb-2 flex items-center gap-2">
        <Spinner size={14} tone="accent" />
        <span className="font-semibold">
          {tr("fs_busy_downloading", undefined, "Downloading from PS5")}
        </span>
        <span className="text-[var(--color-muted)]">
          {formatDuration(elapsedSec)}
        </span>
        {/* Stop button: bumps the store's runId so the runner exits
            at its next poll iteration. The engine job continues
            server-side (no engine-side cancel API yet); the .part
            file lands but the UI stops observing. Wires up the
            previously-dead requestStop() action. */}
        <button
          type="button"
          onClick={() => useFsDownloadOpStore.getState().requestStop(host)}
          className="ml-auto rounded-md border border-[var(--color-border)] px-2 py-0.5 text-xs hover:bg-[var(--color-surface-3)]"
          title={tr(
            "fs_download_stop_tooltip",
            undefined,
            "Stop watching this download (engine job continues server-side)",
          )}
        >
          {tr("fs_download_stop", undefined, "Stop")}
        </button>
      </div>

      <div className="mb-2 flex flex-wrap items-baseline gap-x-3 gap-y-0.5 font-mono text-xs">
        <span>{rootName}</span>
        {totalBytes > 0 ? (
          <span className="text-[var(--color-muted)]">
            {formatBytes(bytesReceived)} / {formatBytes(totalBytes)} (
            {pct.toFixed(0)}%)
          </span>
        ) : (
          <span className="text-[var(--color-muted)]">
            {formatBytes(bytesReceived)}
          </span>
        )}
        {speed > 0 && (
          <span className="text-[var(--color-muted)]">
            {formatBytes(speed)}/s
          </span>
        )}
      </div>

      <div className="mb-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-xs text-[var(--color-muted)]">
        <span>{tr("fs_bulk_from", undefined, "From")}</span>
        <span className="break-all font-mono">{rootSrcPath}</span>
        <span>{tr("fs_bulk_to", undefined, "To")}</span>
        <span className="break-all font-mono">{destDir}</span>
      </div>

      <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
        <div
          className="h-full bg-[var(--color-accent)] transition-[width] duration-300"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}

interface RowMenuItem {
  icon: ComponentType<{ size?: number; className?: string }>;
  label: string;
  run: () => void;
  disabled?: boolean;
  destructive?: boolean;
}

/** The Files row's right-click menu: at the pointer, kept on screen, closed by a click
 *  elsewhere, Escape, scroll or a resize. */
function RowMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: RowMenuItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)),
    });
  }, [x, y]);
  useEffect(() => {
    const close = (e: Event) => {
      if (e instanceof MouseEvent && ref.current?.contains(e.target as Node))
        return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("mousedown", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);
  return (
    <div
      ref={ref}
      role="menu"
      data-testid="fs-row-menu"
      style={{ left: pos.left, top: pos.top }}
      className="fixed z-50 min-w-[12rem] rounded-md border border-[var(--color-border)] bg-[var(--color-surface-raised)] py-1 text-sm shadow-lg"
    >
      {items.map((it) => (
        <button
          key={it.label}
          type="button"
          role="menuitem"
          disabled={it.disabled}
          onClick={() => {
            onClose();
            it.run();
          }}
          className={
            "flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-[var(--color-surface-3)] disabled:opacity-40 " +
            (it.destructive ? "text-[var(--color-bad)]" : "")
          }
        >
          <it.icon size={14} className="shrink-0" />
          {it.label}
        </button>
      ))}
    </div>
  );
}
