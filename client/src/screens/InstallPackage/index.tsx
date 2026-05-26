import { useEffect, useMemo, useState } from "react";
import { useLocation } from "react-router-dom";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isTauriEnv, safeUnlisten } from "../../lib/tauriEnv";
import {
  PackageOpen,
  Plus,
  Play,
  Square,
  Trash2,
  AlertTriangle,
  CheckCircle2,
  XCircle,
  Loader2,
  RotateCcw,
  X,
  Download,
  HardDriveDownload,
  Server,
  CircleDot,
  Package,
  Info,
} from "lucide-react";

import { PageHeader, Button, WarningCard } from "../../components";
import { humanizePs5Error } from "../../lib/humanizeError";
import { isNpxsContentId } from "../../lib/npxs";
import { useConfirm } from "../../components/ConfirmDialog";
import {
  useConnectionStore,
  PS5_PAYLOAD_PORT,
} from "../../state/connection";
import {
  useInstallQueue,
  type InstallQueueItem,
  type InstallPhase,
  type InstallStatus,
} from "../../state/installQueue";
import {
  useInstallSettingsStore,
  type InstallMethod,
} from "../../state/installSettings";
import { useTr } from "../../state/lang";

interface SplitParseResponse {
  parts?: string[];
  part_sizes?: number[];
  total_size?: number;
  head?: {
    size?: number;
    kind?: { kind?: string; magic_hex?: string } | string;
    content_id?: string;
    title?: string;
    category?: string;
    package_type?: string | null;
    warnings?: string[];
  };
}

// 2.12.0 — replaced local toMgmtAddr (signature: bare host) with the
// canonical `mgmtAddr` from lib/addr. Side effect: this file's
// helper had the OPPOSITE signature of FileSystem's same-named
// helper (which took a transfer-port addr), a real footgun if any
// code crossed paths. `mgmtAddr` accepts ANY shape and always
// returns `host:9114`.
import { mgmtAddr as toMgmtAddr } from "../../lib/addr";

function basename(p: string): string {
  const norm = p.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx < 0 ? norm : norm.slice(idx + 1);
}

function fmtBytes(n: number): string {
  if (!n) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2)} ${units[i]}`;
}

function fmtDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const min = Math.floor(seconds / 60);
  const s = seconds % 60;
  if (min < 60) return `${min}m ${s}s`;
  const hr = Math.floor(min / 60);
  return `${hr}h ${min % 60}m`;
}

export default function InstallPackageScreen() {
  // `tr(key, fallback)` and `tr(key, vars, fallback)` are both supported
  // by the `useTr` overloads — see `state/lang.ts::Translator`. We alias
  // to `t` / `tv` purely to keep historical call sites compact.
  const tr = useTr();
  const t = tr;
  const tv = tr;
  const host = useConnectionStore((s) => s.host);
  const items = useInstallQueue((s) => s.items);
  const isRunning = useInstallQueue((s) => s.isRunning);
  const preflightStatus = useInstallQueue((s) => s.preflightStatus);
  const hydrate = useInstallQueue((s) => s.hydrate);
  const start = useInstallQueue((s) => s.start);
  const stop = useInstallQueue((s) => s.stop);
  const remove = useInstallQueue((s) => s.remove);
  const cancel = useInstallQueue((s) => s.cancel);
  const retry = useInstallQueue((s) => s.retry);
  const retryWith = useInstallQueue((s) => s.retryWith);
  const clearFinished = useInstallQueue((s) => s.clearFinished);
  const add = useInstallQueue((s) => s.add);
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();

  // Cancel-with-confirm wrapper. Two distinct scenarios:
  //
  //  1. Mid-flight Stream / Staged install with bytes flowing — the
  //     PS5 may discard partial downloaded bytes and the engine drops
  //     the streaming session. Confirm to defend against misclick.
  //
  //  2. shellui-rpc dispatch — the install has been HANDED OFF to
  //     SceShellUI's process and will run to completion there. We
  //     can't actually cancel it from outside; clicking Cancel just
  //     hides the row locally while the PS5 keeps installing.
  //     Different message so the user knows the install isn't
  //     actually stopping (and where to cancel it if they want to).
  //
  // Skip the dialog for queued / failed items — cancelling those
  // costs nothing.
  const cancelWithConfirm = async (id: string) => {
    const item = useInstallQueue.getState().items.find((x) => x.id === id);
    if (!item) return;
    const isShelluiDispatched =
      item.diag?.registerPath === "shellui-rpc" &&
      (item.status === "running" || item.status === "done");
    if (isShelluiDispatched) {
      const ok = await confirmDialog({
        title: t(
          "install.cancel_shellui_title",
          "Already dispatched to PS5",
        ),
        message: t(
          "install.cancel_shellui_body",
          { name: item.displayName ?? "this pkg" },
          `${item.displayName ?? "This pkg"} has already been handed off to the PS5's installer and will continue running there even if you cancel here. To actually stop it, open the PS5's Notifications → Downloads panel. Hide this row anyway?`,
        ),
        destructive: false,
        confirmLabel: t("install.cancel_shellui_yes", "Hide row"),
        cancelLabel: t("install.cancel_shellui_no", "Keep row"),
      });
      if (!ok) return;
      await cancel(id);
      return;
    }
    const isMidInstall =
      item.status === "running" && (item.bytesDownloaded ?? 0) > 0;
    if (isMidInstall) {
      const ok = await confirmDialog({
        title: t("install.cancel_confirm_title", "Cancel install?"),
        message: t(
          "install.cancel_confirm_body",
          { name: item.displayName ?? "this pkg" },
          `Cancel ${item.displayName ?? "this pkg"}? The PS5 may discard what's been downloaded so far and you'll need to restart from zero.`,
        ),
        destructive: true,
        confirmLabel: t("install.cancel_confirm_yes", "Cancel install"),
        cancelLabel: t("install.cancel_confirm_no", "Keep installing"),
      });
      if (!ok) return;
    }
    await cancel(id);
  };

  const [pickError, setPickError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);

  useEffect(() => {
    hydrate();
  }, [hydrate]);

  // Auto-route from Phase 61's app-wide drag-drop: when a user drops
  // a .pkg from any other screen, AppShell navigates here with the
  // path tucked into location state. Pull it out + auto-enqueue.
  const location = useLocation();
  useEffect(() => {
    const dropped = (location.state as { droppedPath?: string } | null)
      ?.droppedPath;
    if (dropped) {
      void addPkgPath(dropped);
      // Clear the state so a refresh doesn't re-add.
      window.history.replaceState({}, "");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [location.key]);

  // Drag-drop active state for the visual cue. Same shape as
  // Upload screen's drop handler.
  const [dropActive, setDropActive] = useState(false);

  // Subscribe to webview drag-drop. Tauri delivers paths already
  // resolved to the host filesystem; we filter to .pkg ONLY (the
  // BGFT installer doesn't accept .ffpkg / .ffpfs — those are
  // mountable images, not installable packages, and queuing one
  // here would always fail at the pkg-parse step). Non-pkg files
  // surface a clear error rather than silently being ignored.
  //
  // Cleanup mirrors Upload's pattern: a `cancelled` flag handles
  // the "promise resolves after unmount" race so the listener
  // unregisters cleanly even if the user navigates away mid-resolve.
  useEffect(() => {
    // Use `host` directly here rather than `hostReady` (declared
    // further down in the component body, would TDZ). Same idea.
    if (!host) return;
    if (!isTauriEnv()) return; // browser dev/test contexts skip Tauri-only APIs
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    const p = getCurrentWebview().onDragDropEvent(async (e) => {
      if (cancelled) return;
      if (e.payload.type === "enter" || e.payload.type === "over") {
        setDropActive(true);
      } else if (e.payload.type === "leave") {
        setDropActive(false);
      } else if (e.payload.type === "drop") {
        setDropActive(false);
        const paths = e.payload.paths ?? [];
        const pkgPaths = paths.filter((p) => /\.pkg$/i.test(p));
        if (paths.length > 0 && pkgPaths.length === 0) {
          setPickError(
            t(
              "install.error.notPkg",
              "Only .pkg files are installable here. .ffpkg / .ffpfs are mountable images — open them from the File System tab instead.",
            ),
          );
          return;
        }
        for (const p of pkgPaths) {
          if (cancelled) return;
          await addPkgPath(p);
        }
      }
    });
    p.then((fn) => {
      // Tauri unlisten can throw — sync or async — if the webview tore
      // down its listener table between subscribe + cleanup (HMR,
      // parent destroyed). `safeUnlisten` swallows both paths.
      if (cancelled) safeUnlisten(fn);
      else unlisten = fn;
    }).catch(() => { /* subscribe-time rejection: nothing to clean */ });
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
    // addPkgPath is stable across renders (defined inside the
    // component but doesn't depend on props/state that change), so
    // re-subscribing on every render isn't needed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host]);

  const counts = useMemo(() => {
    const c = { pending: 0, running: 0, done: 0, failed: 0, cancelled: 0 };
    for (const it of items) c[it.status] += 1;
    return c;
  }, [items]);

  const hasPending = counts.pending > 0;
  const hasFinished = counts.done + counts.failed + counts.cancelled > 0;
  const hostReady = !!host;

  async function handlePickPkg() {
    setPickError(null);
    if (!hostReady) {
      setPickError(
        t(
          "install.error.noHost",
          "Set a PS5 host on the Connection tab first.",
        ),
      );
      return;
    }
    setPicking(true);
    try {
      const sel = await openDialog({
        multiple: true,
        filters: [{ name: "PS5 Package", extensions: ["pkg"] }],
      });
      const paths = Array.isArray(sel) ? sel : sel ? [sel] : [];
      for (const p of paths) {
        await addPkgPath(p as string);
      }
    } catch (e) {
      setPickError(`${e}`);
    } finally {
      setPicking(false);
    }
  }

  async function addPkgPath(path: string) {
    let displayName = basename(path);
    let totalBytes = 0;
    let contentId = "";
    let packageType = "";
    let warnings: string[] = [];
    let isSplit = false;

    try {
      const meta = (await invoke("pkg_metadata_split", {
        path,
      })) as SplitParseResponse;
      totalBytes = meta.total_size ?? 0;
      isSplit = (meta.parts?.length ?? 1) > 1;
      if (meta.head) {
        contentId = meta.head.content_id ?? "";
        packageType = meta.head.package_type ?? "";
        warnings = [...(meta.head.warnings ?? [])];
        if (meta.head.title) displayName = meta.head.title;
      }
    } catch (e) {
      warnings = [`could not read header: ${e}`];
    }
    // Re-read host AFTER the metadata parse (2.9.0). Split-pkg
    // parsing sha256s the headers and can take seconds. Capturing
    // `addr` BEFORE the await meant a roster switch during parse
    // would queue the item with the OLD addr — subsequent install
    // would silently target the wrong console. Reading after the
    // parse means the queue item is stamped with whatever host the
    // user is looking at when the row appears in the queue (the
    // intuitive contract).
    const addr = toMgmtAddr(useConnectionStore.getState().host || host);
    // 2.2.58: NPXS-prefix system pkgs were getting a noisy per-row
    // pre-flight warning here — replaced with a single page-level
    // note (see the InfoCard near the page header) plus the
    // post-register green "verify on PS5" panel that already shows
    // for these in the done state. The row-level warning was
    // redundant once both of those landed.

    add({
      pkgPath: path,
      isSplit,
      displayName,
      contentId,
      totalBytes,
      packageType,
      addr,
      warnings,
    });
  }

  return (
    <div className="p-6">
      <PageHeader
        icon={PackageOpen}
        title={t("install.title", "Install Package")}
        count={items.length || undefined}
        description={t(
          "install.description",
          "Install .pkg files via Sony's BGFT service. The PS5 fetches each file from this PC over HTTP and installs it.",
        )}
      />

      {/* Page-level note about system pkgs. Updated v2.16.1 after
        * hardware testing showed that NPXS pkgs CAN install via the
        * Staged + ShellUI-RPC path (with the screen flash), contrary
        * to the previous "typically can't complete here" wording. */}
      <div className="mb-4 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-[12px] text-[var(--color-muted)]">
        <Info size={13} className="mt-0.5 shrink-0" />
        <div>
          <span className="font-medium text-[var(--color-text)]">
            {t("install.note.gamePkgsOnly.title", "Game pkgs work best")}
          </span>
          {" — "}
          {t(
            "install.note.gamePkgsOnly.body",
            "this installer is built around Sony's game-pkg API. System pkgs (NPXS-prefix — Store updates, Settings, built-in apps) often need Staged mode + a brief screen flash to complete; if they still fail, install them via the PS5's own Settings → Debug Settings → Game → Package Installer.",
          )}
        </div>
      </div>

      <div className="mb-4">
        <WarningCard
          title={t(
            "install.blackScreen.title",
            "Your PS5 screen may briefly go black during install — that's normal.",
          )}
          detail={t(
            "install.blackScreen.body",
            "The flash only happens on Staged installs where we route through ShellUI's process; Sony's watchdog notices and respawns ShellUI, which looks like a screen flash. The console stays on and the install keeps running. The picture comes back when it finishes.",
          )}
        />
      </div>

      {/* Preflight banner — surfaces ensurePayloadCurrent progress so
        * the user doesn't watch a silent ~30s wait after Start.
        * Cleared by the queue worker once the first install actually
        * begins. */}
      {preflightStatus && (
        <div className="mb-4 flex items-start gap-2 rounded-md border border-blue-500/30 bg-blue-500/5 p-3 text-[12px]">
          <Loader2 size={13} className="mt-0.5 shrink-0 animate-spin text-blue-500" />
          {/* Strip the zero-width-space runId tag the queue worker
            * appends for ownership tracking — see preflightStatus
            * note in installQueue.ts. The tag is everything from the
            * ZWSP (​) onwards. */}
          <div className="text-[var(--color-text)]">
            {preflightStatus.split("​")[0]}
          </div>
        </div>
      )}

      {pickError && (
        <div className="mb-4">
          <WarningCard
            title={t("install.pickError", "Could not add file")}
            detail={pickError}
          />
        </div>
      )}

      {/* Action bar — matches the Upload screen's QueuePanel pattern.
          Section card wraps Add button + queue controls + target line
          so the header stays compact when the queue is empty. */}
      <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
        <header className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2 text-sm font-semibold">
            <Package size={14} />
            <span>{t("install.queueTitle", "Install queue")}</span>
            {items.length > 0 && (
              <span className="text-xs font-normal text-[var(--color-muted)]">
                ·{" "}
                {tv(
                  "install.counts.total",
                  { n: items.length },
                  "{n} total",
                )}
                {counts.done > 0 && (
                  <>
                    {" · "}
                    <span className="tabular-nums">{counts.done}</span>{" "}
                    {t("install.counts.done", "done")}
                  </>
                )}
                {counts.pending > 0 && (
                  <>
                    {" · "}
                    <span className="tabular-nums">{counts.pending}</span>{" "}
                    {t("install.counts.pending", "pending")}
                  </>
                )}
                {counts.failed > 0 && (
                  <>
                    {" · "}
                    <span className="tabular-nums text-[var(--color-bad)]">
                      {counts.failed}
                    </span>{" "}
                    {t("install.counts.failed", "failed")}
                  </>
                )}
              </span>
            )}
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant="primary"
              size="sm"
              leftIcon={<Plus size={12} />}
              onClick={handlePickPkg}
              loading={picking}
              disabled={!hostReady}
              title={
                !hostReady
                  ? t(
                      "install.add.disabledHint",
                      "Set a PS5 host on the Connection tab first",
                    )
                  : undefined
              }
            >
              {t("install.add", "Add .pkg")}
            </Button>
            {hasFinished && !isRunning && (
              <Button
                variant="ghost"
                size="sm"
                leftIcon={<Trash2 size={12} />}
                onClick={() => clearFinished()}
              >
                {t("install.clearFinished", "Clear finished")}
              </Button>
            )}
            {isRunning ? (
              <Button
                variant="danger"
                size="sm"
                leftIcon={<Square size={12} />}
                onClick={() => stop()}
              >
                {t("install.stop", "Stop")}
              </Button>
            ) : (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<Play size={12} />}
                onClick={() => start()}
                disabled={!hasPending}
                title={
                  !hasPending
                    ? t(
                        "install.start.disabledHint",
                        "Add a .pkg file first",
                      )
                    : undefined
                }
              >
                {t("install.start", "Start")}
              </Button>
            )}
          </div>
        </header>

        <div className="mt-2.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-[var(--color-muted)]">
          {hostReady ? (
            <>
              <span>{t("install.target", "Target")}:</span>
              <code className="rounded bg-[var(--color-surface)] px-1.5 py-0.5 font-mono">
                {host}:{PS5_PAYLOAD_PORT}
              </code>
              <span className="opacity-70">·</span>
              <span>
                {t(
                  "install.targetHint",
                  "queue runs sequentially, one install at a time",
                )}
              </span>
            </>
          ) : (
            <span className="text-[var(--color-bad)]">
              {t(
                "install.noTarget",
                "No PS5 host set — open the Connection tab to set one.",
              )}
            </span>
          )}
        </div>

        <InstallMethodPicker t={t} />


        {items.length === 0 ? (
          <EmptyInstallPanel t={t} dropActive={dropActive} />
        ) : (
          <ul className="mt-4 grid gap-2">
            {items.map((it) => (
              <InstallRow
                key={it.id}
                item={it}
                t={t}
                onRemove={() => remove(it.id)}
                onCancel={() => void cancelWithConfirm(it.id)}
                onRetry={() => retry(it.id)}
                onRetryAsStaged={() => retryWith(it.id, "stage")}
              />
            ))}
          </ul>
        )}
      </section>
      {confirmDialogNode}
    </div>
  );
}

/* ─── Install method picker ───────────────────────────────────────── */

/** Compact two-option segmented control for choosing the default
 *  install method. The selection writes to useInstallSettingsStore;
 *  the install queue reads the setting at item-add time and locks it
 *  into the queue row so toggling here doesn't disturb in-flight
 *  installs.
 *
 *  Stream (DPI 2.0) is highlighted as the recommended default — no
 *  2× disk space, no upload step, native BGFT pause/resume. The
 *  staging path stays available for users whose LAN topology
 *  prevents the PS5 from reaching the desktop's HTTP port
 *  (segregated VLAN, host firewall). */
function InstallMethodPicker({
  t,
}: {
  t: (k: string, fb: string) => string;
}) {
  const method = useInstallSettingsStore((s) => s.installMethod);
  const setMethod = useInstallSettingsStore((s) => s.setInstallMethod);
  const options: Array<{ value: InstallMethod; label: string; hint: string }> = [
    {
      value: "stream",
      label: t("install.method.stream", "Stream (DPI 2.0)"),
      hint: t(
        "install.method.streamHint",
        "BGFT pulls bytes from this PC and installs in one pass. No 2× disk space, native pause/resume.",
      ),
    },
    {
      value: "stage",
      label: t("install.method.stage", "Upload then install"),
      hint: t(
        "install.method.stageHint",
        "Upload .pkg to PS5 disk first, then install from there. Use when the PS5 can't reach this PC over HTTP.",
      ),
    },
  ];
  const active = options.find((o) => o.value === method) ?? options[0];
  return (
    <div className="mt-3 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-[var(--color-muted)]">
      <span>{t("install.method.label", "Install method")}:</span>
      <div
        role="radiogroup"
        aria-label={t("install.method.label", "Install method")}
        className="inline-flex overflow-hidden rounded-md border border-[var(--color-border)]"
      >
        {options.map((opt) => {
          const selected = opt.value === method;
          return (
            <button
              key={opt.value}
              type="button"
              role="radio"
              aria-checked={selected}
              onClick={() => setMethod(opt.value)}
              title={opt.hint}
              className={
                "px-2.5 py-1 text-[11px] transition-colors " +
                (selected
                  ? "bg-[var(--color-accent)] text-white"
                  : "bg-[var(--color-surface)] text-[var(--color-text)] hover:bg-[var(--color-surface-hover)]")
              }
            >
              {opt.label}
            </button>
          );
        })}
      </div>
      <span className="opacity-70">·</span>
      <span className="leading-snug">{active.hint}</span>
    </div>
  );
}

/* ─── Empty state ─────────────────────────────────────────────────── */

function EmptyInstallPanel({
  t,
  dropActive,
}: {
  t: (k: string, fb: string) => string;
  dropActive: boolean;
}) {
  return (
    <div
      className={`mt-4 rounded-md border-2 border-dashed px-6 py-8 text-center transition-colors ${
        dropActive
          ? "border-[var(--color-accent)] bg-[var(--color-accent)]/5"
          : "border-[var(--color-border)] bg-[var(--color-surface)]"
      }`}
    >
      <div
        className={`mx-auto mb-3 flex h-12 w-12 items-center justify-center rounded-full transition-colors ${
          dropActive
            ? "bg-[var(--color-accent)]/20"
            : "bg-[var(--color-surface-2)]"
        }`}
      >
        <PackageOpen
          size={22}
          className={
            dropActive
              ? "text-[var(--color-accent)]"
              : "text-[var(--color-muted)]"
          }
        />
      </div>
      <h3 className="text-sm font-medium">
        {dropActive
          ? t("install.empty.dropActive", "Drop to add")
          : t("install.empty.title", "No installs queued")}
      </h3>
      <p className="mx-auto mt-1.5 max-w-md text-xs leading-relaxed text-[var(--color-muted)]">
        {t(
          "install.empty.description",
          "Drag .pkg files here, or click Add. Split-pkg sets are detected automatically — pick the lead .pkg.",
        )}
      </p>

      <div className="mx-auto mt-7 flex max-w-xl items-stretch justify-between gap-3 sm:gap-4">
        <FlowStep
          n={1}
          icon={Server}
          label={t("install.flow.host", "PC hosts")}
        />
        <FlowConnector />
        <FlowStep
          n={2}
          icon={Download}
          label={t("install.flow.download", "PS5 downloads")}
        />
        <FlowConnector />
        <FlowStep
          n={3}
          icon={HardDriveDownload}
          label={t("install.flow.install", "PS5 installs")}
        />
      </div>

      <p className="mx-auto mt-7 max-w-md text-[11px] text-[var(--color-muted)]">
        {t(
          "install.empty.requirements",
          "Requires kstuff + ps5upload payload. No third-party loader needed.",
        )}
      </p>
    </div>
  );
}

function FlowStep({
  n,
  icon: Icon,
  label,
}: {
  n: number;
  icon: typeof Server;
  label: string;
}) {
  return (
    <div className="flex flex-1 flex-col items-center gap-1.5">
      <div className="relative flex h-8 w-8 items-center justify-center rounded-full border border-[var(--color-border)] bg-[var(--color-surface-2)]">
        <Icon size={14} className="text-[var(--color-muted)]" />
        <span className="absolute -top-1 -right-1 flex h-4 w-4 items-center justify-center rounded-full bg-[var(--color-accent)] text-[10px] font-semibold text-[var(--color-accent-contrast)]">
          {n}
        </span>
      </div>
      <span className="text-[11px] text-[var(--color-muted)]">{label}</span>
    </div>
  );
}

function FlowConnector() {
  return (
    <div className="mt-4 h-px flex-1 self-start bg-[var(--color-border)]" />
  );
}

/* ─── Install row ─────────────────────────────────────────────────── */

function InstallRow({
  item,
  t,
  onRemove,
  onCancel,
  onRetry,
  onRetryAsStaged,
}: {
  item: InstallQueueItem;
  t: (k: string, fb: string) => string;
  onRemove: () => void;
  onCancel: () => void;
  onRetry: () => void;
  /** Re-queue this item with installMethod="stage" (Tier-1 upload-
   *  then-install). Shown only on a failed stream-mode row — the
   *  fallback for "the PS5 couldn't reach this PC's HTTP port". */
  onRetryAsStaged: () => void;
}) {
  const isActive = item.status === "running";
  const pct =
    item.totalBytes > 0
      ? Math.min(100, (item.bytesDownloaded / item.totalBytes) * 100)
      : 0;
  const rate = useRollingRate(item.bytesDownloaded, isActive);
  const remaining = Math.max(0, item.totalBytes - item.bytesDownloaded);
  const etaSec =
    isActive && rate > 0 && remaining > 0
      ? Math.round(remaining / rate)
      : null;

  return (
    <li
      className={`rounded-md border p-3 text-sm transition-colors ${
        item.status === "failed"
          ? "border-[var(--color-bad)]"
          : item.status === "done"
            ? "border-[var(--color-good)]"
            : isActive
              ? "border-[var(--color-accent)]"
              : "border-[var(--color-border)] hover:border-[var(--color-surface-3)]"
      } bg-[var(--color-surface)]`}
    >
      <div className="flex items-start gap-3">
        <StatusIcon status={item.status} />
        <div className="min-w-0 flex-1">
          {/* Title + tags */}
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className="truncate font-medium">{item.displayName}</span>
            {item.isSplit && <Tag>{t("install.tag.split", "split")}</Tag>}
            {item.packageType && <Tag>{item.packageType}</Tag>}
            {/* Install-method badge so the user sees at-a-glance how
                this row is being installed, especially useful when
                the queue mixes legacy + DPI 2.0 rows (e.g. a retry of
                an old persisted item runs as "stage" while new adds
                run as "stream"). */}
            <Tag>
              {item.installMethod === "stream"
                ? t("install.tag.stream", "stream")
                : t("install.tag.staged", "staged")}
            </Tag>
            {/* NPXS-prefix detection — proactively flag system pkgs at
                queue time so the user isn't surprised by the post-fail
                explainer. The page-level info card covers the "why"
                already; this per-row amber badge is the "this specific
                row applies to that warning" affordance — most
                noticeable when the queue mixes game + system pkgs. */}
            {isNpxsContentId(item.contentId) && (
              <span
                title={t(
                  "install.tag.system_pkg_tooltip",
                  "System pkg (NPXS-prefix). Will likely register but may not complete here — use the PS5's Debug Settings → Game → Package Installer instead.",
                )}
                aria-label={t(
                  "install.tag.system_pkg_aria",
                  "System pkg — likely will not complete via this installer",
                )}
                className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-medium text-amber-500"
              >
                {t("install.tag.system_pkg", "system pkg")}
              </span>
            )}
          </div>

          {/* content_id */}
          {item.contentId && (
            <div className="mt-0.5 truncate font-mono text-[11px] text-[var(--color-muted)]">
              {item.contentId}
            </div>
          )}

          {/* Meta line */}
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] text-[var(--color-muted)]">
            <span className="tabular-nums">{fmtBytes(item.totalBytes)}</span>
            {isActive && rate > 0 && (
              <span className="tabular-nums">{fmtBytes(rate)}/s</span>
            )}
            {isActive && etaSec !== null && (
              <span className="tabular-nums">
                {t("install.eta", "ETA")} {fmtDuration(etaSec)}
              </span>
            )}
            {item.status === "done" &&
              item.startedAt &&
              item.finishedAt && (
                <span className="tabular-nums">
                  {t("install.took", "took")}{" "}
                  {fmtDuration(
                    Math.max(
                      1,
                      Math.round((item.finishedAt - item.startedAt) / 1000),
                    ),
                  )}
                </span>
              )}
            {item.status === "pending" && (
              <span>{t("install.statusPending", "pending")}</span>
            )}
            {item.status === "cancelled" && (
              <span>{t("install.statusCancelled", "cancelled")}</span>
            )}
          </div>
        </div>

        {/* Right-side icon actions — match QueuePanel pattern */}
        <div className="flex shrink-0 items-center gap-1">
          {isActive && (
            <button
              type="button"
              onClick={onCancel}
              title={t("install.cancel", "Cancel")}
              className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-bad)] hover:text-[var(--color-accent-contrast)]"
            >
              <X size={14} />
            </button>
          )}
          {item.status === "failed" && (
            <button
              type="button"
              onClick={onRetry}
              title={t("install.retry", "Retry")}
              className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
            >
              <RotateCcw size={14} />
            </button>
          )}
          {!isActive && (
            <button
              type="button"
              onClick={onRemove}
              title={t("install.remove", "Remove")}
              className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-bad)] hover:text-[var(--color-accent-contrast)]"
            >
              <Trash2 size={14} />
            </button>
          )}
        </div>
      </div>

      {/* Phase tracker — only while running or just-done */}
      {(isActive || item.status === "done") && (
        <div className="mt-3">
          <PhaseTracker phase={item.phase} status={item.status} t={t} />
        </div>
      )}

      {/* Tier-1 staging upload progress. Shown only while phase=staging,
       *  i.e. before the actual install kicks off. Once the worker
       *  transitions to queued/download/install, the regular progress
       *  bar below takes over. */}
      {isActive && item.phase === "staging" && item.totalBytes > 0 && (
        <div className="mt-2">
          <div className="mb-1 flex items-baseline justify-between text-[11px] text-[var(--color-muted)]">
            <span>
              {t(
                "install.staging",
                "Uploading to PS5 staging…",
              )}
            </span>
            <span className="tabular-nums">
              {fmtBytes(item.stagingBytes)} / {fmtBytes(item.totalBytes)}
            </span>
          </div>
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
            <div
              className="h-full bg-[var(--color-accent)] transition-[width] duration-300"
              style={{
                width: `${
                  item.totalBytes > 0
                    ? Math.min(100, (item.stagingBytes / item.totalBytes) * 100)
                    : 0
                }%`,
              }}
            />
          </div>
        </div>
      )}

      {/* Progress bar */}
      {isActive && item.phase !== "staging" && item.totalBytes > 0 && (
        <div className="mt-2">
          <div className="mb-1 flex items-baseline justify-between text-[11px] text-[var(--color-muted)]">
            <span className="tabular-nums">
              {fmtBytes(item.bytesDownloaded)} / {fmtBytes(item.totalBytes)}
            </span>
            <span className="tabular-nums">{pct.toFixed(0)}%</span>
          </div>
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
            <div
              className="h-full bg-[var(--color-accent)] transition-[width] duration-300"
              style={{ width: `${pct}%` }}
            />
          </div>
        </div>
      )}

      {/* Pre-install warnings */}
      {item.warnings.length > 0 && item.status === "pending" && (
        <div className="mt-2 flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/5 p-2 text-[11px] text-amber-700 dark:text-amber-400">
          <AlertTriangle size={12} className="mt-0.5 shrink-0" />
          <div className="space-y-0.5">
            {item.warnings.map((w) => (
              <div key={w}>{w}</div>
            ))}
          </div>
        </div>
      )}

      {/* 2.2.57: NPXS post-register success message. The fast-path in
        * installQueue.ts marks NPXS pkgs done immediately after Sony
        * accepts the register (status polling is unreliable for
        * system pkgs); make sure the user knows they need to verify
        * on the PS5 itself rather than seeing a generic "Done"
        * checkmark and assuming progress is observable from the
        * desktop. */}
      {/* Hand-off banner: shown for installs dispatched via shellui-rpc
       * (Tier 1) — the actual install runs in ShellUI's process via
       * Sony's queue, and we deliberately don't poll Sony's status API
       * from our process to avoid the cross-process segfault / hang
       * documented at bgft.c:159. The row is marked done immediately
       * once shellui-rpc accepts the dispatch; the user verifies the
       * real completion via the PS5's on-screen notification.
       *
       * Covers DLC, regular game pkgs, AND system NPXS pkgs (all of
       * which route through shellui-rpc when the fakepkg-friendly
       * register variant is unavailable). */}
      {item.status === "done" &&
        (item.diag.registerPath === "shellui-rpc" ||
          isNpxsContentId(item.contentId)) && (
          <div className="mt-2 flex items-start gap-2 rounded-md border border-emerald-500/40 bg-emerald-500/5 p-2 text-[11px] text-emerald-700 dark:text-emerald-400">
            <CheckCircle2 size={12} className="mt-0.5 shrink-0" />
            <div>
              {t(
                "install.dispatched.body",
                "Install dispatched. Check the PS5's notification panel (top-right) to confirm it finished.",
              )}
            </div>
          </div>
        )}

      {/* Failure */}
      {item.errMessage && item.status === "failed" && (
        <div className="mt-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-2)] p-2 text-[11px] text-[var(--color-bad)]">
          <div className="break-words">
            {humanizePs5Error(item.errMessage, item.errCode, item.contentId)}
          </div>
          {item.errCode > 0 && (
            <code className="mt-1 inline-block font-mono text-[10px] opacity-75">
              0x{item.errCode.toString(16).padStart(8, "0")}
            </code>
          )}
          {/* DPI 2.0 → legacy fallback. We can't always tell from the
              err_code alone whether the PS5 failed to fetch the URL
              vs Sony's installer rejecting the bytes — but offering
              the fallback as a one-click action is harmless either
              way, and it's the single most likely fix for a
              stream-mode failure on a LAN where the PS5 can't reach
              the desktop's HTTP port (firewall, segregated VLAN). */}
          {item.installMethod === "stream" && (
            <div className="mt-2">
              <button
                type="button"
                onClick={onRetryAsStaged}
                className="inline-flex items-center gap-1 rounded border border-[var(--color-bad)] bg-[var(--color-surface)] px-2 py-1 text-[11px] font-medium text-[var(--color-text)] hover:bg-[var(--color-surface-hover)]"
              >
                <RotateCcw size={12} />
                {t(
                  "install.retryAsStaged",
                  "Retry as Upload then install",
                )}
              </button>
              <div className="mt-1 text-[10px] text-[var(--color-muted)]">
                {t(
                  "install.retryAsStagedHint",
                  "Uses the legacy path: uploads the .pkg to the PS5 first, then installs. Try this if the PS5 couldn't reach this PC over HTTP (firewall, VLAN).",
                )}
              </div>
            </div>
          )}
        </div>
      )}

      {/* 2.2.52 install-start diagnostics. Rendered on any row whose
       *  diag was populated (i.e. install/start has run at least
       *  once — pending rows that haven't started yet skip it). The
       *  three fields capture the most common failure modes:
       *    - register_path tells whether the fakepkg-friendly Debug
       *      Register variant was actually used.
       *    - intdebug_avail tells whether that variant even resolved.
       *    - kernel_rw tells whether the loader granted us the
       *      kernel R/W primitive needed for the privileged call.
       *  Wrapped in <details> so users only see it when they want to. */}
      {(item.diag.registerPath !== "" ||
        item.stagingPath ||
        item.diag.shelluiErr !== null ||
        item.diag.appinstErr !== null) && (
        <details
          // Auto-expand on failure so the user doesn't have to click
          // to see context for the error. On non-failed rows the
          // disclosure stays collapsed (it's diagnostic-only — most
          // of the time you don't need to look at it).
          open={item.status === "failed"}
          className="mt-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-2 text-[11px]"
        >
          <summary className="cursor-pointer text-[var(--color-muted)]">
            {t("install_start_diagnostics", "Install-start diagnostics")}
          </summary>
          <dl className="mt-2 grid grid-cols-[max-content_1fr] gap-x-3 gap-y-1 font-mono text-[10px]">
            <dt className="text-[var(--color-muted)]">
              {t("install_register_path_label", "register path")}
            </dt>
            <dd>
              {item.diag.registerPath || "—"}
              {item.diag.registerPath === "regular" && (
                <span className="ml-2 text-[var(--color-warn)]">
                  {t(
                    "install_register_path_entitlement_checked",
                    "(entitlement-checked — fakepkg may fail)",
                  )}
                </span>
              )}
              {item.diag.registerPath === "shellui-rpc" && (
                <span className="ml-2 text-[var(--color-good)]">
                  {t(
                    "install_register_path_tier1",
                    "(Tier-1 — install routed through ShellUI's process)",
                  )}
                </span>
              )}
              {item.diag.registerPath === "tier0-worker" && (
                <span className="ml-2 text-[var(--color-good)]">
                  {t(
                    "install_register_path_tier0",
                    "(Tier-0 worker — opt-in, PS5UPLOAD_TIER0_WORKER=1)",
                  )}
                </span>
              )}
              {item.diag.registerPath === "appinst" && (
                <span className="ml-2 text-[var(--color-good)]">
                  {t(
                    "install_register_path_appinst",
                    "(in-process AppInstUtil — no ShellUI flash)",
                  )}
                </span>
              )}
            </dd>
            <dt className="text-[var(--color-muted)]">
              {t("install_intdebug_avail_label", "intdebug avail")}
            </dt>
            <dd>
              {item.diag.intdebugAvail ? "yes" : "no"}
              {!item.diag.intdebugAvail && (
                <span className="ml-2 text-[var(--color-warn)]">
                  {t(
                    "install_intdebug_no_register_variant",
                    "(FW does not expose the fakepkg-friendly Register variant)",
                  )}
                </span>
              )}
            </dd>
            <dt className="text-[var(--color-muted)]">
              {t("install_kernel_rw_label", "kernel R/W")}
            </dt>
            <dd>
              {item.diag.kernelRw ? "yes" : "no"}
              {!item.diag.kernelRw && (
                <span className="ml-2 text-[var(--color-warn)]">
                  {t(
                    "install_kernel_rw_no_cred",
                    "(no cred elevation — load via :9021)",
                  )}
                </span>
              )}
            </dd>
            {/* 2.2.52-fix per-tier err breakdown — null = tier didn't run,
             *  0 = tier completed cleanly, otherwise Sony's err_code (or
             *  our 0xE000_xxxx machinery code). Lets the user see WHERE
             *  Tier 1 / Tier 2 broke when registerPath="none". */}
            {item.diag.shelluiErr !== null && (
              <>
                <dt className="text-[var(--color-muted)]">
                  {t("install_tier1_shellui_label", "tier 1 (shellui)")}
                </dt>
                <dd>
                  {item.diag.shelluiErr === 0 ? (
                    <span className="text-[var(--color-good)]">
                      {t("install_shellui_accepted", "accepted")}
                    </span>
                  ) : (
                    <code className="text-[var(--color-bad)]">
                      0x{item.diag.shelluiErr.toString(16).padStart(8, "0")}
                    </code>
                  )}
                  {item.diag.shelluiErr === 0xe0000002 && (
                    <span className="ml-2 text-[var(--color-warn)]">
                      {t(
                        "install_shellui_no_appinstutil",
                        "(libSceAppInstUtil not in ShellUI's address space)",
                      )}
                    </span>
                  )}
                </dd>
              </>
            )}
            {item.diag.appinstErr !== null && (
              <>
                <dt className="text-[var(--color-muted)]">
                  {t("install_tier2_appinst_label", "tier 2 (appinst)")}
                </dt>
                <dd>
                  {item.diag.appinstErr === 0 ? (
                    <span className="text-[var(--color-good)]">
                      {t("install_appinst_accepted", "accepted")}
                    </span>
                  ) : (
                    <code className="text-[var(--color-bad)]">
                      0x{item.diag.appinstErr.toString(16).padStart(8, "0")}
                    </code>
                  )}
                  {item.diag.appinstErr === 0x80b22404 && (
                    <span className="ml-2 text-[var(--color-warn)]">
                      {t(
                        "install_appinst_playgo_404",
                        "(PlayGo HTTP-404 — process-context reject; expected on FW 9.60+)",
                      )}
                    </span>
                  )}
                </dd>
              </>
            )}
            {item.stagingPath && (
              <>
                <dt className="text-[var(--color-muted)]">
                  {t("install_staging_path_label", "staging path")}
                </dt>
                <dd className="break-all">
                  {item.stagingPath}
                  <span className="ml-2 text-[var(--color-muted)]">
                    {t(
                      "install_staging_path_note",
                      "(auto-deleted after install; payload sweeps stale >24h)",
                    )}
                  </span>
                </dd>
              </>
            )}
          </dl>
        </details>
      )}
    </li>
  );
}

function Tag({ children }: { children: React.ReactNode }) {
  return (
    <span className="rounded bg-[var(--color-surface-2)] px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-[var(--color-muted)]">
      {children}
    </span>
  );
}

function PhaseTracker({
  phase,
  status,
  t,
}: {
  phase: InstallPhase;
  status: InstallStatus;
  t: (k: string, fb: string) => string;
}) {
  // Phase → completed-step mapping. The PhaseDot at step N shows:
  //   - CheckCircle (done)   when completed > N
  //   - Loader2 spinning     when completed == N (in-progress, "active")
  //   - CircleDot dim        when completed < N
  // So for phase=install, completed=4 means dots 1-3 are CheckCircle
  // (done) and dot 4 (Installing) shows the spinner.
  //   staging  → step 1 spinning  (uploading bytes to PS5)
  //   queued   → step 2 spinning  (Sony queued; no fetch yet)
  //   download → step 3 spinning  (Sony fetching from us)
  //   install  → step 4 spinning  (Sony decrypting + writing)
  //   done     → all CheckCircles (status===done flips dot 4)
  const completed: number =
    status === "done"
      ? 4
      : phase === "install"
        ? 4
        : phase === "download"
          ? 3
          : phase === "queued"
            ? 2
            : phase === "staging"
              ? 1
              : 0;
  return (
    <div className="flex items-center gap-1.5">
      <PhaseDot
        active={completed >= 1}
        done={completed > 1}
        label={t("install.phase.staging", "Uploading")}
      />
      <PhaseLine done={completed > 1} />
      <PhaseDot
        active={completed >= 2}
        done={completed > 2}
        label={t("install.phase.host", "Ready")}
      />
      <PhaseLine done={completed > 2} />
      <PhaseDot
        active={completed >= 3}
        done={completed > 3}
        label={t("install.phase.download", "Downloading")}
      />
      <PhaseLine done={completed > 3} />
      <PhaseDot
        active={completed >= 4}
        done={status === "done"}
        label={t("install.phase.install", "Installing")}
      />
    </div>
  );
}

function PhaseDot({
  active,
  done,
  label,
}: {
  active: boolean;
  done: boolean;
  label: string;
}) {
  return (
    <div className="flex items-center gap-1.5" title={label}>
      {done ? (
        <CheckCircle2 size={12} className="shrink-0 text-[var(--color-good)]" />
      ) : active ? (
        <Loader2
          size={12}
          className="shrink-0 animate-spin text-[var(--color-accent)]"
        />
      ) : (
        <CircleDot
          size={12}
          className="shrink-0 text-[var(--color-muted)] opacity-40"
        />
      )}
      <span
        className={
          done || active
            ? "text-[11px] text-[var(--color-text)]"
            : "text-[11px] text-[var(--color-muted)] opacity-60"
        }
      >
        {label}
      </span>
    </div>
  );
}

function PhaseLine({ done }: { done: boolean }) {
  return (
    <div
      className={
        "h-px flex-1 " +
        (done ? "bg-[var(--color-good)]" : "bg-[var(--color-border)]")
      }
    />
  );
}

function StatusIcon({ status }: { status: InstallStatus }) {
  switch (status) {
    case "running":
      return (
        <Loader2
          size={16}
          className="mt-0.5 shrink-0 animate-spin text-[var(--color-accent)]"
        />
      );
    case "done":
      return (
        <CheckCircle2
          size={16}
          className="mt-0.5 shrink-0 text-[var(--color-good)]"
        />
      );
    case "failed":
      return (
        <XCircle size={16} className="mt-0.5 shrink-0 text-[var(--color-bad)]" />
      );
    case "cancelled":
      return (
        <X size={16} className="mt-0.5 shrink-0 text-[var(--color-muted)]" />
      );
    default:
      return (
        <Package
          size={16}
          className="mt-0.5 shrink-0 text-[var(--color-muted)]"
        />
      );
  }
}

/* ─── Helpers ─────────────────────────────────────────────────────── */

function useRollingRate(bytes: number, live: boolean): number {
  // State-based (not ref-based) so the displayed rate updates in the
  // same render cycle that mutates the sample window. Reading ref
  // values during render trips react-hooks/refs in CI; using state
  // also avoids the one-frame-stale rate that the ref version had.
  const [samples, setSamples] = useState<{ t: number; b: number }[]>([]);
  useEffect(() => {
    if (!live) {
      setSamples([]);
      return;
    }
    const now = Date.now();
    setSamples((prev) =>
      [...prev, { t: now, b: bytes }].filter((s) => now - s.t < 2500),
    );
  }, [bytes, live]);
  if (!live || samples.length < 2) return 0;
  const first = samples[0];
  const last = samples[samples.length - 1];
  const dt = (last.t - first.t) / 1000;
  if (dt <= 0) return 0;
  return Math.max(0, (last.b - first.b) / dt);
}
