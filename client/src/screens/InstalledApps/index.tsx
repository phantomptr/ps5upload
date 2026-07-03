import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "../../lib/invokeLogged";
import {
  Gamepad2,
  RefreshCw,
  Trash2,
  Disc3,
  FolderOpen,
  Package,
  AlertTriangle,
  Play,
  Loader2,
  Download,
  ShieldCheck,
  Square,
  CircleDot,
} from "lucide-react";

import { useNavigate } from "react-router-dom";
import { openInFileSystem } from "../../state/fsNavigation";
import { useConnectionStore } from "../../state/connection";
import {
  appsInstalled,
  appUnregister,
  appLaunch,
  appIconUrl,
  appKill,
  processKill,
  smpStatus,
  type InstalledTitle,
  type SmpStatus,
} from "../../api/ps5";
import {
  fetchRunningGames,
  type RunningGame,
} from "../../lib/runningGames";
import {
  PageHeader,
  EmptyState,
  ErrorCard,
  WarningCard,
  Button,
  ConnectionGate,
  Skeleton,
  PlatformBadge,
} from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { humanizePs5Error } from "../../lib/humanizeError";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { useTr } from "../../state/lang";
import { transferAddr, mgmtAddr, hostOf } from "../../lib/addr";
import { transferScreenBusy } from "../../lib/ps5Transfers";
import { useStaleHostGuard } from "../../lib/staleHostGuard";

// ── Classification helpers ───────────────────────────────────────────────────

type Platform = "ps4" | "ps5" | "system" | "other";

/** PS4 vs PS5 from the title id prefix. CUSAxxxxx = PS4, PPSAxxxxx = PS5,
 *  NPXS = Sony system app. Everything else (IDLE, custom homebrew ids) is
 *  "other" so we don't mislabel it. */
function platformOf(t: InstalledTitle): Platform {
  if (t.system) return "system";
  const id = (t.titleId || "").toUpperCase();
  if (id.startsWith("CUSA")) return "ps4";
  if (id.startsWith("PPSA")) return "ps5";
  if (id.startsWith("NPXS")) return "system";
  return "other";
}

type Kind = "installed" | "disc" | "folder" | "system";

/** How the title got onto the console — the user's mental model:
 *   - installed: no source path (a .pkg install or a shipped Sony app)
 *   - disc:      a disc-image mount (/mnt/shadowmnt, needs ShadowMount+)
 *   - folder:    a /data/homebrew/<id>-app folder registered by us
 *   - system:    NPXS system title */
function kindOf(t: InstalledTitle): Kind {
  if (t.system) return "system";
  if (t.origin === "registered") {
    const src = t.source || "";
    if (t.imageBacked || src.startsWith("/mnt/shadowmnt")) return "disc";
    if (src.startsWith("/data/homebrew")) return "folder";
    // Registered but with no (or some other) source path — treat as a
    // plain installed title rather than inventing a folder.
    return src ? "folder" : "installed";
  }
  // origin "pkg": installed via Sony's installer from a .pkg, no path.
  return "installed";
}

function KindBadge({ title }: { title: InstalledTitle }) {
  const tr = useTr();
  const k = kindOf(title);
  const base =
    "inline-flex shrink-0 items-center gap-1 whitespace-nowrap rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-xs font-medium text-[var(--color-muted)]";
  if (k === "system")
    return (
      <span className="inline-flex shrink-0 items-center gap-1 whitespace-nowrap rounded bg-[var(--color-bad-soft)] px-1.5 py-0.5 text-xs font-medium text-[var(--color-bad)]">
        <AlertTriangle size={11} />
        {tr("installed_badge_system", undefined, "System")}
      </span>
    );
  if (k === "disc")
    return (
      <span className={base}>
        <Disc3 size={11} />
        {tr("installed_badge_image", undefined, "Disk image")}
      </span>
    );
  if (k === "folder")
    return (
      <span className={base}>
        <FolderOpen size={11} />
        {tr("installed_badge_folder", undefined, "Folder")}
      </span>
    );
  return (
    <span className={base}>
      <Package size={11} />
      {tr("installed_badge_installed", undefined, "Installed")}
    </span>
  );
}

// ── Cover art ────────────────────────────────────────────────────────────────

function Cover({ host, title }: { host: string; title: InstalledTitle }) {
  const [failed, setFailed] = useState(false);
  const show = !failed && !!host.trim();
  return (
    <div className="relative flex aspect-square w-full items-center justify-center overflow-hidden rounded-lg bg-[var(--color-surface-3)]">
      {show ? (
        <img
          src={appIconUrl(transferAddr(host), title.titleId)}
          alt=""
          // `contain`, not `cover`: PS4/PS5 cover art isn't always square
          // (some titles ship wide key-art), and `cover` was cropping the
          // top/bottom off those — logos like "ASTRO BOT" lost their lower
          // half. `contain` guarantees the whole image is visible; square
          // icons still fill the square box edge-to-edge, and the neutral
          // surface backs any letterbox margins on non-square art.
          className="h-full w-full object-contain"
          loading="lazy"
          onError={() => setFailed(true)}
        />
      ) : (
        <Gamepad2 size={28} className="text-[var(--color-muted)]" />
      )}
    </div>
  );
}

// How long to keep a just-launched title in the patient "Starting…" state
// while we watch for its process to appear, and how often to check. A first
// launch (just-installed, cold cache, disc image) can be slow, so this is
// generous — it never fails the launch or touches the game, it just stops
// reporting "Starting…" after this and tells the user it may still be coming up.
const LAUNCH_CONFIRM_TIMEOUT_MS = 90_000;
const LAUNCH_CONFIRM_POLL_MS = 2_000;

// ── App card ─────────────────────────────────────────────────────────────────

function AppCard({
  host,
  title,
  busy,
  launching,
  running,
  stopping,
  discNeedsSmp,
  onUninstall,
  onLaunch,
  onStop,
}: {
  host: string;
  title: InstalledTitle;
  busy: boolean;
  launching: boolean;
  /** True while this title is running on the console (show Stop + "Playing"). */
  running: boolean;
  /** True while a Stop request for this title is in flight. */
  stopping: boolean;
  /** True for a disc-image title while ShadowMount+ isn't running. */
  discNeedsSmp: boolean;
  onUninstall: (t: InstalledTitle) => void;
  onLaunch: (t: InstalledTitle) => void;
  onStop: (t: InstalledTitle) => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const canPlay = !title.system;
  // On-console folder this title lives in — only folder/disc kinds carry a
  // source path (pkg-installed/system titles don't), so the open-folder
  // affordance only appears when there's actually a folder to open.
  const sourceFolder = title.source || null;
  return (
    <div className="group flex flex-col overflow-hidden rounded-xl border border-[var(--color-border)] bg-[var(--color-surface-2)]">
      {/* Cover with corner overlays: platform (top-left), SMP warning
          (top-right) — keeps the body clean + every card the same height. */}
      <div className="relative">
        <Cover host={host} title={title} />
        <div className="absolute left-2 top-2 drop-shadow">
          <PlatformBadge platform={platformOf(title)} />
        </div>
        {discNeedsSmp ? (
          <span
            className="absolute right-2 top-2 inline-flex items-center gap-1 rounded bg-[var(--color-warn)] px-1.5 py-0.5 text-xs font-semibold text-black drop-shadow"
            title={tr(
              "installed_disc_needs_smp_row",
              undefined,
              "Needs ShadowMount+ running to mount + launch.",
            )}
          >
            <AlertTriangle size={11} />
            {tr("installed_badge_smp_needed", undefined, "SMP")}
          </span>
        ) : null}
        {running ? (
          <span
            className="absolute bottom-2 right-2 inline-flex items-center gap-1 rounded bg-[var(--color-good)] px-1.5 py-0.5 text-xs font-semibold text-black drop-shadow"
            title={tr("installed_now_playing", undefined, "Now playing")}
          >
            <CircleDot size={11} className="animate-pulse" />
            {tr("installed_badge_playing", undefined, "Playing")}
          </span>
        ) : null}
      </div>

      {/* Body: name → type + id → actions pinned to the bottom (mt-auto) so
          rows of cards line their buttons up regardless of name length. */}
      <div className="flex flex-1 flex-col gap-2 p-3">
        <div className="min-w-0">
          {/* line-clamp-2 (not truncate): a 1-line clamp turned readable
              names like "Payload Manager" into "Payload Mana…". Two lines fit
              the vast majority of titles in full; the title attr still covers
              the rare overflow on hover. min-h reserves the full two-line box
              so a 1-line name and a 2-line name keep the badge/id row (and the
              whole card body) at the same height across a grid row — otherwise
              neighbouring cards looked vertically misaligned ("overlapping"). */}
          <div
            className="line-clamp-2 min-h-[2.5rem] text-sm font-semibold"
            title={title.titleName}
          >
            {title.titleName}
          </div>
          <div
            className="mt-1 flex min-w-0 items-center gap-2"
            title={title.source || title.titleId}
          >
            <KindBadge title={title} />
            <span className="truncate font-mono text-xs text-[var(--color-muted)]">
              {title.titleId}
            </span>
          </div>
        </div>

        <div className="mt-auto flex flex-wrap items-center gap-2 pt-1">
          {canPlay ? (
            running && !launching ? (
              // The title is running → offer Stop (close the game) instead of
              // Play. Stop is the ONLY thing that ends a game, and it's always
              // explicit + confirmed — we never auto-close a running or
              // starting title.
              <Button
                variant="danger"
                size="md"
                loading={stopping}
                leftIcon={<Square size={15} />}
                className="flex-1 min-w-fit"
                onClick={() => onStop(title)}
                title={tr(
                  "installed_stop_tooltip",
                  undefined,
                  "Close this running game on the PS5",
                )}
              >
                {stopping
                  ? tr("installed_stopping", undefined, "Closing…")
                  : tr("installed_stop", undefined, "Close game")}
              </Button>
            ) : (
              <Button
                variant="primary"
                size="md"
                loading={launching}
                leftIcon={<Play size={15} />}
                // `min-w-fit` overrides the global `min-width:0` so the button
                // never shrinks below its own label — on a disc-image card (which
                // adds an "Open folder" button) the Play button used to get
                // squeezed and clip "Play" → "Pla". `flex-wrap` on the row lets the
                // icon buttons drop to a second line instead, before that happens.
                className="flex-1 min-w-fit"
                onClick={() => onLaunch(title)}
                // A disc-image title can't launch until ShadowMount+ mounts it.
                // Also disabled while starting so a second click can't fire a
                // launch into a game that's still coming up (which kills it).
                disabled={discNeedsSmp || launching}
                title={
                  discNeedsSmp
                    ? tr(
                        "installed_play_needs_smp",
                        undefined,
                        "Needs ShadowMount+ running to mount and launch",
                      )
                    : tr(
                        "installed_play_tooltip",
                        undefined,
                        "Launch this title on the PS5",
                      )
                }
              >
                {discNeedsSmp
                  ? tr("installed_needs_smp", undefined, "Needs ShadowMount+")
                  : launching
                    ? tr("installed_starting", undefined, "Starting…")
                    : tr("installed_play", undefined, "Play")}
              </Button>
            )
          ) : (
            <span className="flex-1 truncate text-xs text-[var(--color-muted)]">
              {tr("installed_badge_system", undefined, "System")}
            </span>
          )}
          {/* Open this title's on-console folder in the File System browser
              (only when the title has a source path — folder/disc kinds). */}
          {sourceFolder && (
            <button
              type="button"
              onClick={() => openInFileSystem(navigate, sourceFolder)}
              title={tr(
                "installed_open_folder",
                undefined,
                "Open this app's folder in the File System browser",
              )}
              aria-label={tr("installed_open_folder", undefined, "Open folder")}
              className="shrink-0 rounded-md border border-[var(--color-border)] p-2.5 text-[var(--color-muted)] transition-colors hover:border-[var(--color-accent)] hover:text-[var(--color-accent)]"
            >
              <FolderOpen size={15} />
            </button>
          )}
          {/* Uninstall — de-emphasized icon button (destructive action stays
              out of the way; turns red on hover). */}
          <button
            type="button"
            disabled={busy}
            onClick={() => onUninstall(title)}
            title={tr("installed_uninstall", undefined, "Uninstall")}
            aria-label={tr("installed_uninstall", undefined, "Uninstall")}
            className="shrink-0 rounded-md border border-[var(--color-border)] p-2.5 text-[var(--color-muted)] transition-colors hover:border-[var(--color-bad)] hover:text-[var(--color-bad)] disabled:cursor-not-allowed disabled:opacity-40"
          >
            {busy ? (
              <Loader2 size={15} className="animate-spin" />
            ) : (
              <Trash2 size={15} />
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Section wrapper ──────────────────────────────────────────────────────────

function Section({
  icon: Icon,
  title,
  hint,
  count,
  children,
}: {
  icon: React.ComponentType<{ size?: number; className?: string }>;
  title: string;
  hint: string;
  count: number;
  children: React.ReactNode;
}) {
  return (
    <section className="flex flex-col gap-3">
      <div className="flex items-start gap-2">
        <Icon size={16} className="mt-0.5 shrink-0 text-[var(--color-muted)]" />
        <div className="min-w-0">
          <h2 className="text-sm font-semibold">
            {title}{" "}
            <span className="font-normal text-[var(--color-muted)]">
              ({count})
            </span>
          </h2>
          <p className="text-xs text-[var(--color-muted)]">{hint}</p>
        </div>
      </div>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
        {children}
      </div>
    </section>
  );
}

// ── Screen ───────────────────────────────────────────────────────────────────

export default function InstalledAppsScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  // Kernel R/W = a jailbroken entry point (kstuff) is active. Without it,
  // installs/launches of fpkg titles fail. null = unknown (old payload / no
  // probe yet) — we only warn on a definite `false`.
  const ucredElevated = useConnectionStore((s) => s.ucredElevated);
  const guard = useStaleHostGuard();
  const [titles, setTitles] = useState<InstalledTitle[] | null>(null);
  // Tri-state, NOT just SmpStatus|null. "checking" means the probe is in
  // flight; null means it definitively failed/unreachable. The disc-image
  // warning keys off "confirmed not running", so distinguishing "still
  // checking" from "confirmed off" is what stops the warning from flashing
  // for a frame on every load before the status comes back.
  const [smp, setSmp] = useState<SmpStatus | "checking" | null>("checking");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [registeredUnavailable, setRegisteredUnavailable] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [launchingId, setLaunchingId] = useState<string | null>(null);
  // Games currently running on the console (title_id → handle), refreshed on a
  // poll while this screen is open. Drives the "Playing" badge + Stop button.
  const [running, setRunning] = useState<Map<string, RunningGame>>(new Map());
  const [stoppingId, setStoppingId] = useState<string | null>(null);
  const [smpSending, setSmpSending] = useState(false);
  const [smpMsg, setSmpMsg] = useState<string | null>(null);
  // Native window.confirm() is a no-op in Tauri's webview; use the in-tree
  // modal instead (see ConfirmDialog.tsx).
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();

  const refresh = useCallback(async () => {
    if (!host?.trim()) return;
    const probe = guard.capture();
    setLoading(true);
    setError(null);
    try {
      const res = await appsInstalled(transferAddr(probe.host));
      if (probe.isStale()) return;
      setTitles(res.titles);
      setRegisteredUnavailable(res.registeredUnavailable);
      // ShadowMount+ status — best-effort, never blocks the app list.
      try {
        const s = await smpStatus(mgmtAddr(probe.host));
        if (!probe.isStale()) setSmp(s);
      } catch {
        if (!probe.isStale()) setSmp(null);
      }
    } catch (e) {
      if (probe.isStale()) return;
      const raw = e instanceof Error ? e.message : String(e);
      setError(humanizePs5Error(raw));
      setTitles(null);
    } finally {
      setLoading(false);
    }
  }, [host, guard]);

  // Drop the previous console's grid the moment the active host changes, so a
  // Launch/Uninstall click during the switch window can't fire A's titleId at
  // B (the handlers capture the live host but the row's titleId is from the
  // still-rendered old list — an uninstall would then remove that title from
  // the WRONG console). The async guard prevents stale writes; this prevents
  // stale *clicks*. Same reset-on-[host] pattern as DiskUsage / FileSystem.
  useEffect(() => {
    setTitles(null);
    setSmp("checking");
    setError(null);
    setRunning(new Map());
  }, [host]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Poll which games are running so cards can show "Playing" + a Stop button,
  // and a just-launched title flips from "Starting…" to "Playing" on its own.
  // Read-only (process list) — it never touches a running game, so it can't
  // disturb a title that's still coming up. Runs only while the screen is open.
  useEffect(() => {
    if (!host?.trim()) return;
    let cancelled = false;
    const addr = mgmtAddr(host);
    const tick = async () => {
      // Don't add mgmt-port load while an upload to THIS console is running.
      // A folder reconcile (e.g. a many-chunk exfat.ffpfsc game) fires a
      // per-file connection burst at the payload's mgmt port; injecting a
      // process-list poll every 3s onto that contends with each file's
      // finalize round-trip and can collapse effective throughput on a
      // many-file upload (a single .pkg is one finalize, so it's immune).
      // The running-game badge can wait until the upload finishes.
      if (transferScreenBusy(host)) return;
      try {
        const r = await fetchRunningGames(addr);
        if (!cancelled) setRunning(r);
      } catch {
        // Transient (console busy/offline) — keep the last known set rather
        // than flicker every card's state on one failed poll.
      }
    };
    void tick();
    const id = setInterval(() => void tick(), 3000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [host]);

  const handleLaunch = useCallback(
    async (t: InstalledTitle) => {
      if (!host?.trim()) return;
      const probe = guard.capture();
      // Hold the "Starting…" state for the whole come-up window (not just the
      // launch RPC) so Play stays disabled and the user can't fire a second
      // launch into a game that's still starting — re-launching a half-started
      // title is exactly how it gets killed. We only watch (read-only) for the
      // game's process to appear; we never act on a starting game.
      setLaunchingId(t.titleId);
      try {
        await appLaunch(transferAddr(probe.host), t.titleId);
        if (probe.isStale()) return;
        // Patiently wait for the title to actually come up. A first launch
        // (just-installed, cold cache, disc image) can take a while; the launch
        // RPC only means "Sony accepted it," not "it's running." Poll the
        // process list until the title appears, then the card shows "Playing".
        const addr = mgmtAddr(probe.host);
        const deadline = Date.now() + LAUNCH_CONFIRM_TIMEOUT_MS;
        let confirmed = false;
        while (Date.now() < deadline) {
          await new Promise((r) => setTimeout(r, LAUNCH_CONFIRM_POLL_MS));
          if (probe.isStale()) return;
          try {
            const r = await fetchRunningGames(addr);
            if (probe.isStale()) return;
            setRunning(r);
            if (r.has(t.titleId)) {
              confirmed = true;
              break;
            }
          } catch {
            // Console momentarily busy/recovering while the game spins up —
            // keep waiting rather than giving up.
          }
        }
        if (confirmed) {
          pushNotification("success", withConsolePrefix(probe.host, t.titleName), {
            body: tr("installed_now_playing", undefined, "Now playing"),
          });
        } else {
          // Not seen yet — do NOT treat this as a failure (and never kill it).
          // The launch was accepted; a slow first start just hasn't surfaced in
          // the process list yet.
          pushNotification("info", withConsolePrefix(probe.host, t.titleName), {
            body: tr(
              "installed_launch_slow",
              undefined,
              "Launch sent — first starts can take a while. Give it a moment and check your PS5.",
            ),
          });
        }
      } catch (e) {
        if (probe.isStale()) return;
        const raw = e instanceof Error ? e.message : String(e);
        pushNotification("error", withConsolePrefix(probe.host, t.titleName), {
          body: humanizePs5Error(raw),
        });
      } finally {
        setLaunchingId(null);
      }
    },
    [host, guard, tr],
  );

  const handleStop = useCallback(
    async (t: InstalledTitle) => {
      if (!host?.trim()) return;
      const game = running.get(t.titleId);
      if (!game) return;
      const probe = guard.capture();
      const ok = await confirmDialog({
        title: tr(
          "installed_stop_confirm_title",
          { name: t.titleName },
          `Close ${t.titleName}?`,
        ),
        message: tr(
          "installed_stop_confirm_body",
          undefined,
          "This closes the running game on the PS5. Any unsaved progress will be lost — the same as quitting from the console.",
        ),
        confirmLabel: tr("installed_stop", undefined, "Close game"),
        destructive: true,
      });
      if (!ok || probe.isStale()) return;
      setStoppingId(t.titleId);
      try {
        // Prefer Sony's clean app-kill (by app id); fall back to a SIGKILL of
        // the title's pid. CRUCIAL: appKill can *throw* (the engine bails when
        // the payload returns ok=false — observed on FW 12.20, where
        // sceApplicationKill rejects the app id), so the fallback must run on a
        // throw too, not only on a resolved ok=false. Treating both the same
        // way is what makes the SIGKILL path actually reachable — previously a
        // 12.20 appKill threw straight to the outer catch and the pid fallback
        // (which runs with the payload's elevated ucred) never fired, so the
        // Close button "did nothing".
        const addr = mgmtAddr(probe.host);
        let killed = false;
        if (game.appId) {
          try {
            killed = (await appKill(addr, game.appId)).ok;
          } catch {
            /* Sony's app-kill failed/threw — fall through to SIGKILL. */
          }
        }
        if (!killed && game.pid) {
          try {
            killed = (await processKill(addr, game.pid)).ok;
          } catch {
            /* SIGKILL also failed — reported as "couldn't close" below. */
          }
        }
        const ack = { ok: killed };
        if (probe.isStale()) return;
        if (ack.ok) {
          // Drop it from the running set immediately so the card flips back to
          // Play without waiting for the next poll.
          setRunning((cur) => {
            const next = new Map(cur);
            next.delete(t.titleId);
            return next;
          });
          pushNotification("info", withConsolePrefix(probe.host, t.titleName), {
            body: tr("installed_stopped", undefined, "Game closed"),
          });
        } else {
          pushNotification("error", withConsolePrefix(probe.host, t.titleName), {
            body: tr(
              "installed_stop_failed",
              undefined,
              "Couldn't close the game — it may have already exited.",
            ),
          });
        }
      } catch (e) {
        if (probe.isStale()) return;
        const raw = e instanceof Error ? e.message : String(e);
        pushNotification("error", withConsolePrefix(probe.host, t.titleName), {
          body: humanizePs5Error(raw),
        });
      } finally {
        setStoppingId(null);
      }
    },
    [host, guard, running, confirmDialog, tr],
  );

  const handleUninstall = useCallback(
    async (t: InstalledTitle) => {
      if (!host?.trim()) return;
      const probe = guard.capture();
      const ok = await confirmDialog({
        title: tr(
          "installed_uninstall_confirm_title",
          { name: t.titleName },
          `Uninstall ${t.titleName}?`,
        ),
        message: t.system
          ? tr(
              "installed_uninstall_confirm_system",
              { id: t.titleId },
              `${t.titleId} is a SYSTEM app. Removing it can destabilize the console and may require a reinstall to recover. Only continue if you know exactly what this package is.`,
            )
          : t.origin === "registered"
            ? tr(
                "installed_uninstall_confirm_registered",
                undefined,
                "This unmounts and removes the title from the home screen. Your source files/image on disk are not deleted.",
              )
            : tr(
                "installed_uninstall_confirm_pkg",
                undefined,
                "This removes the installed title from the PS5. You can reinstall it later from the package.",
              ),
        confirmLabel: tr("installed_uninstall", undefined, "Uninstall"),
        destructive: true,
      });
      if (!ok || probe.isStale()) return;
      setBusyId(t.titleId);
      setError(null);
      try {
        await appUnregister(transferAddr(probe.host), t.titleId);
        if (probe.isStale()) return;
        setTitles((cur) => cur?.filter((x) => x.titleId !== t.titleId) ?? cur);
        void refresh();
      } catch (e) {
        if (probe.isStale()) return;
        const raw = e instanceof Error ? e.message : String(e);
        setError(humanizePs5Error(raw));
      } finally {
        setBusyId(null);
      }
    },
    [host, guard, confirmDialog, tr, refresh],
  );

  const handleSendSmp = useCallback(async () => {
    if (!host?.trim()) return;
    const ip = hostOf(host);
    setSmpSending(true);
    setSmpMsg(null);
    try {
      const path = (await invoke("payloads_local_path", {
        id: "shadowmountplus",
      })) as string | null;
      if (!path) {
        setSmpMsg(
          tr(
            "installed_smp_not_downloaded",
            undefined,
            "ShadowMount+ isn't downloaded yet — grab it from the Payloads tab, then come back and send it.",
          ),
        );
        return;
      }
      const res = (await invoke("payload_send", {
        ip,
        path,
        port: null,
      })) as { ok?: boolean; error?: string };
      if (res?.ok) {
        setSmpMsg(
          tr(
            "installed_smp_sent",
            undefined,
            "Sent ShadowMount+ — give it a few seconds, then Refresh.",
          ),
        );
      } else {
        setSmpMsg(
          res?.error ||
            tr(
              "installed_smp_send_failed",
              undefined,
              "Couldn't send ShadowMount+.",
            ),
        );
      }
    } catch (e) {
      setSmpMsg(humanizePs5Error(e instanceof Error ? e.message : String(e)));
    } finally {
      setSmpSending(false);
    }
  }, [host, tr]);

  // Group by kind.
  const all = useMemo(() => titles ?? [], [titles]);
  const installed = useMemo(
    () => all.filter((t) => kindOf(t) === "installed"),
    [all],
  );
  const discs = useMemo(() => all.filter((t) => kindOf(t) === "disc"), [all]);
  const folders = useMemo(
    () => all.filter((t) => kindOf(t) === "folder"),
    [all],
  );
  const system = useMemo(
    () => all.filter((t) => kindOf(t) === "system"),
    [all],
  );

  // While the probe is still in flight we know neither "running" nor "off",
  // so the warning must stay hidden — surfacing it only once we've confirmed
  // ShadowMount+ is actually not running.
  const smpChecking = smp === "checking";
  const smpRunning = smp !== null && smp !== "checking" && smp.running === true;
  const discNeedsSmp = discs.length > 0 && !smpRunning && !smpChecking;

  // NOTE: `key` is passed explicitly at each call site (`<AppCard key={t.titleId}
  // {...cardProps(t)} />`) — never spread, or React warns that a key in a
  // spread object is ignored.
  const cardProps = (t: InstalledTitle) => ({
    host,
    title: t,
    busy: busyId === t.titleId,
    launching: launchingId === t.titleId,
    running: running.has(t.titleId),
    stopping: stoppingId === t.titleId,
    discNeedsSmp: kindOf(t) === "disc" && !smpRunning && !smpChecking,
    onUninstall: handleUninstall,
    onLaunch: handleLaunch,
    onStop: handleStop,
  });

  return (
    <div className="flex flex-col gap-5 p-6">
      <PageHeader
        icon={Gamepad2}
        title={tr("installed_apps_title", undefined, "Installed Apps")}
        loading={loading}
        description={tr(
          "installed_apps_subtitle",
          undefined,
          "Everything installed on the PS5, grouped by how it got there. Press Play to launch a title; Uninstall to remove it.",
        )}
        right={
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={() => void refresh()}
            disabled={loading || !host?.trim()}
            loading={loading}
          >
            {tr("refresh", undefined, "Refresh")}
          </Button>
        }
      />

      <ConnectionGate require="payload">
        {/* kstuff status — games can't install or launch without kernel R/W.
          The gate guarantees a connected console here, so we key off the
          probe result alone. */}
        {ucredElevated === false ? (
          <WarningCard
            title={tr(
              "installed_kstuff_off_title",
              undefined,
              "kstuff isn't active — games won't launch",
            )}
            detail={tr(
              "installed_kstuff_off_body",
              undefined,
              "The payload doesn't have kernel read/write, which means a jailbreak/kstuff entry point isn't loaded. Launching (and installing) fpkg games will fail until you load the console through kstuff and reconnect.",
            )}
          />
        ) : ucredElevated === true ? (
          <div className="flex items-center gap-2 rounded-lg border border-[var(--color-good)]/40 bg-[var(--color-good)]/5 px-3 py-2 text-xs text-[var(--color-good)]">
            <ShieldCheck size={14} className="shrink-0" />
            {tr(
              "installed_kstuff_on",
              undefined,
              "kstuff active (kernel R/W) — installs and launches are good to go.",
            )}
          </div>
        ) : null}

        {/* ShadowMount+ — required for disc-image titles. */}
        {discNeedsSmp ? (
          <WarningCard
            title={tr(
              "installed_smp_off_title",
              { count: discs.length },
              `${discs.length} disk image${discs.length === 1 ? "" : "s"} need ShadowMount+ running`,
            )}
            detail={tr(
              "installed_smp_off_body",
              undefined,
              "Disc-image titles are mounted from /mnt/shadowmnt by ShadowMount+. It doesn't look like it's running, so those titles won't mount or launch. Send it to the console, wait a few seconds, then Refresh.",
            )}
            action={
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  variant="primary"
                  size="sm"
                  disabled={smpSending}
                  onClick={() => void handleSendSmp()}
                >
                  {smpSending ? (
                    <Loader2 size={14} className="animate-spin" />
                  ) : (
                    <Download size={14} />
                  )}
                  {tr("installed_smp_send", undefined, "Send ShadowMount+")}
                </Button>
                {smpMsg ? (
                  <span className="text-xs text-[var(--color-muted)]">
                    {smpMsg}
                  </span>
                ) : null}
              </div>
            }
          />
        ) : null}

        {error ? (
          <ErrorCard
            title={tr(
              "installed_error_title",
              undefined,
              "Couldn't read installed apps",
            )}
            detail={error}
          />
        ) : loading && titles === null ? (
          // Skeleton tiles hold the grid's shape while /user/appmeta is
          // enumerated — arrival doesn't reflow the page, and the shimmer
          // says "working" without a modal spinner.
          <div
            aria-hidden
            // Must match the real Section grid breakpoints exactly (line ~308)
            // or the cards visibly reflow the moment data arrives.
            className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
          >
            {Array.from({ length: 10 }, (_, i) => (
              <Skeleton key={i} className="aspect-square" />
            ))}
          </div>
        ) : titles && titles.length === 0 ? (
          <EmptyState
            icon={Gamepad2}
            size="hero"
            title={tr(
              "installed_empty_title",
              undefined,
              "No installed titles found",
            )}
            message={tr(
              "installed_empty_body",
              undefined,
              "Nothing under /user/appmeta. Install a package or register a game first.",
            )}
          />
        ) : (
          <div className="flex flex-col gap-6">
            {registeredUnavailable ? (
              <WarningCard
                title={tr(
                  "installed_registered_unavailable",
                  undefined,
                  "Couldn't read the mounted/registered set from the payload — everything is shown under Installed. Reload the payload to fix grouping.",
                )}
              />
            ) : null}

            {installed.length > 0 ? (
              <Section
                icon={Package}
                title={tr(
                  "installed_section_installed",
                  undefined,
                  "Games & apps",
                )}
                hint={tr(
                  "installed_section_installed_hint",
                  undefined,
                  "Installed via Sony's installer from a .pkg (or shipped with the console). No source path.",
                )}
                count={installed.length}
              >
                {installed.map((t) => (
                  <AppCard key={t.titleId} {...cardProps(t)} />
                ))}
              </Section>
            ) : null}

            {discs.length > 0 ? (
              <Section
                icon={Disc3}
                title={tr("installed_section_disc", undefined, "Disk images")}
                hint={tr(
                  "installed_section_disc_hint",
                  undefined,
                  "Mounted from /mnt/shadowmnt by ShadowMount+. They only mount + launch while ShadowMount+ is running.",
                )}
                count={discs.length}
              >
                {discs.map((t) => (
                  <AppCard key={t.titleId} {...cardProps(t)} />
                ))}
              </Section>
            ) : null}

            {folders.length > 0 ? (
              <Section
                icon={FolderOpen}
                title={tr(
                  "installed_section_folder",
                  undefined,
                  "Folder homebrew",
                )}
                hint={tr(
                  "installed_section_folder_hint",
                  undefined,
                  "Registered from a /data/homebrew/<id>-app folder on the console.",
                )}
                count={folders.length}
              >
                {folders.map((t) => (
                  <AppCard key={t.titleId} {...cardProps(t)} />
                ))}
              </Section>
            ) : null}

            {system.length > 0 ? (
              <Section
                icon={AlertTriangle}
                title={tr("installed_section_system", undefined, "System")}
                hint={tr(
                  "installed_section_system_hint",
                  undefined,
                  "Sony system apps. Don't remove these unless you know exactly what they are.",
                )}
                count={system.length}
              >
                {system.map((t) => (
                  <AppCard key={t.titleId} {...cardProps(t)} />
                ))}
              </Section>
            ) : null}
          </div>
        )}
      </ConnectionGate>

      {confirmDialogNode}
    </div>
  );
}
