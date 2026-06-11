import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
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
} from "lucide-react";

import { useConnectionStore } from "../../state/connection";
import {
  appsInstalled,
  appUnregister,
  appLaunch,
  appIconUrl,
  smpStatus,
  type InstalledTitle,
  type SmpStatus,
} from "../../api/ps5";
import {
  PageHeader,
  EmptyState,
  ErrorCard,
  WarningCard,
  Button,
  ConnectionGate,
  Skeleton,
} from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { humanizePs5Error } from "../../lib/humanizeError";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { useTr } from "../../state/lang";
import { transferAddr, mgmtAddr, hostOf } from "../../lib/addr";
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

function PlatformBadge({ title }: { title: InstalledTitle }) {
  const tr = useTr();
  const p = platformOf(title);
  if (p === "ps4")
    return (
      <span className="shrink-0 whitespace-nowrap rounded bg-[var(--color-ps4-soft)] px-1.5 py-0.5 text-xs font-semibold text-[var(--color-ps4)]">
        {tr("installed_badge_ps4", undefined, "PS4")}
      </span>
    );
  if (p === "ps5")
    return (
      <span className="shrink-0 whitespace-nowrap rounded bg-[var(--color-ps5-soft)] px-1.5 py-0.5 text-xs font-semibold text-[var(--color-ps5)]">
        {tr("installed_badge_ps5", undefined, "PS5")}
      </span>
    );
  return null;
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
        {tr("installed_badge_image", undefined, "Disc image")}
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

// ── App card ─────────────────────────────────────────────────────────────────

function AppCard({
  host,
  title,
  busy,
  launching,
  discNeedsSmp,
  onUninstall,
  onLaunch,
}: {
  host: string;
  title: InstalledTitle;
  busy: boolean;
  launching: boolean;
  /** True for a disc-image title while ShadowMount+ isn't running. */
  discNeedsSmp: boolean;
  onUninstall: (t: InstalledTitle) => void;
  onLaunch: (t: InstalledTitle) => void;
}) {
  const tr = useTr();
  const canPlay = !title.system;
  return (
    <div className="group flex flex-col overflow-hidden rounded-xl border border-[var(--color-border)] bg-[var(--color-surface-2)]">
      {/* Cover with corner overlays: platform (top-left), SMP warning
          (top-right) — keeps the body clean + every card the same height. */}
      <div className="relative">
        <Cover host={host} title={title} />
        <div className="absolute left-2 top-2 drop-shadow">
          <PlatformBadge title={title} />
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
      </div>

      {/* Body: name → type + id → actions pinned to the bottom (mt-auto) so
          rows of cards line their buttons up regardless of name length. */}
      <div className="flex flex-1 flex-col gap-2 p-3">
        <div className="min-w-0">
          <div
            className="truncate text-sm font-semibold"
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

        <div className="mt-auto flex items-center gap-2 pt-1">
          {canPlay ? (
            <Button
              variant="primary"
              size="md"
              loading={launching}
              leftIcon={<Play size={15} />}
              className="flex-1"
              onClick={() => onLaunch(title)}
              title={tr(
                "installed_play_tooltip",
                undefined,
                "Launch this title on the PS5",
              )}
            >
              {launching
                ? tr("installed_launching", undefined, "Launching…")
                : tr("installed_play", undefined, "Play")}
            </Button>
          ) : (
            <span className="flex-1 truncate text-xs text-[var(--color-muted)]">
              {tr("installed_badge_system", undefined, "System")}
            </span>
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
  const [smp, setSmp] = useState<SmpStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [registeredUnavailable, setRegisteredUnavailable] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [launchingId, setLaunchingId] = useState<string | null>(null);
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
    setSmp(null);
    setError(null);
  }, [host]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const handleLaunch = useCallback(
    async (t: InstalledTitle) => {
      if (!host?.trim()) return;
      const probe = guard.capture();
      setLaunchingId(t.titleId);
      try {
        await appLaunch(transferAddr(probe.host), t.titleId);
        if (probe.isStale()) return;
        // Toast (not inline) so the card grid stays a uniform height.
        pushNotification("info", withConsolePrefix(probe.host, t.titleName), {
          body: tr(
            "installed_launch_sent",
            undefined,
            "Launch sent — check your PS5",
          ),
        });
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

  const smpRunning = smp?.running === true;
  const discNeedsSmp = discs.length > 0 && !smpRunning;

  // NOTE: `key` is passed explicitly at each call site (`<AppCard key={t.titleId}
  // {...cardProps(t)} />`) — never spread, or React warns that a key in a
  // spread object is ignored.
  const cardProps = (t: InstalledTitle) => ({
    host,
    title: t,
    busy: busyId === t.titleId,
    launching: launchingId === t.titleId,
    discNeedsSmp: kindOf(t) === "disc" && !smpRunning,
    onUninstall: handleUninstall,
    onLaunch: handleLaunch,
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
              `${discs.length} disc image${discs.length === 1 ? "" : "s"} need ShadowMount+ running`,
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
            className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5"
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
                title={tr("installed_section_disc", undefined, "Disc images")}
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
