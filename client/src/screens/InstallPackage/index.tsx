import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
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
} from "lucide-react";

import { PageHeader, Button, WarningCard } from "../../components";
import { humanizePs5Error } from "../../lib/humanizeError";
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

function toMgmtAddr(host: string): string {
  return `${host}:9114`;
}

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
  const tr = useTr();
  const t = (k: string, fb: string) => tr(k, {}, fb);
  // Same as `t` but accepts variable substitutions; used where the
  // string contains an interpolation like "{n} total".
  const tv = (k: string, vars: Record<string, string | number>, fb: string) =>
    tr(k, vars, fb);
  const host = useConnectionStore((s) => s.host);
  const items = useInstallQueue((s) => s.items);
  const isRunning = useInstallQueue((s) => s.isRunning);
  const hydrate = useInstallQueue((s) => s.hydrate);
  const start = useInstallQueue((s) => s.start);
  const stop = useInstallQueue((s) => s.stop);
  const remove = useInstallQueue((s) => s.remove);
  const cancel = useInstallQueue((s) => s.cancel);
  const retry = useInstallQueue((s) => s.retry);
  const clearFinished = useInstallQueue((s) => s.clearFinished);
  const add = useInstallQueue((s) => s.add);

  const [pickError, setPickError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);

  useEffect(() => {
    hydrate();
  }, [hydrate]);

  // Drag-drop active state for the visual cue. Same shape as
  // Upload screen's drop handler.
  const [dropActive, setDropActive] = useState(false);

  // Subscribe to webview drag-drop. Tauri delivers paths already
  // resolved to the host filesystem; we filter to .pkg / .ffpkg /
  // .ffpfs and route each through addPkgPath. Non-pkg files surface
  // a clear error rather than silently being ignored.
  //
  // Cleanup mirrors Upload's pattern: a `cancelled` flag handles
  // the "promise resolves after unmount" race so the listener
  // unregisters cleanly even if the user navigates away mid-resolve.
  useEffect(() => {
    // Use `host` directly here rather than `hostReady` (declared
    // further down in the component body, would TDZ). Same idea.
    if (!host) return;
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
        const pkgPaths = paths.filter((p) => /\.(pkg|ffpkg|ffpfs)$/i.test(p));
        if (paths.length > 0 && pkgPaths.length === 0) {
          setPickError(
            t(
              "install.error.notPkg",
              "Dropped files aren't .pkg / .ffpkg / .ffpfs. Only PS5 package formats are supported here.",
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
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    });
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
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
    const addr = toMgmtAddr(host);
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
        warnings = meta.head.warnings ?? [];
        if (meta.head.title) displayName = meta.head.title;
      }
    } catch (e) {
      warnings = [`could not read header: ${e}`];
    }

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
                onCancel={() => cancel(it.id)}
                onRetry={() => retry(it.id)}
              />
            ))}
          </ul>
        )}
      </section>
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
}: {
  item: InstallQueueItem;
  t: (k: string, fb: string) => string;
  onRemove: () => void;
  onCancel: () => void;
  onRetry: () => void;
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
            {item.warnings.map((w, i) => (
              <div key={i}>{w}</div>
            ))}
          </div>
        </div>
      )}

      {/* Failure */}
      {item.errMessage && item.status === "failed" && (
        <div className="mt-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-2)] p-2 text-[11px] text-[var(--color-bad)]">
          <div className="break-words">
            {humanizePs5Error(item.errMessage, item.errCode)}
          </div>
          {item.errCode > 0 && (
            <code className="mt-1 inline-block font-mono text-[10px] opacity-75">
              0x{item.errCode.toString(16).padStart(8, "0")}
            </code>
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
            Install-start diagnostics
          </summary>
          <dl className="mt-2 grid grid-cols-[max-content_1fr] gap-x-3 gap-y-1 font-mono text-[10px]">
            <dt className="text-[var(--color-muted)]">register path</dt>
            <dd>
              {item.diag.registerPath || "—"}
              {item.diag.registerPath === "regular" && (
                <span className="ml-2 text-[var(--color-warn)]">
                  (entitlement-checked — fakepkg may fail)
                </span>
              )}
              {item.diag.registerPath === "shellui-rpc" && (
                <span className="ml-2 text-[var(--color-good)]">
                  (Tier-1 — install routed through ShellUI's process)
                </span>
              )}
            </dd>
            <dt className="text-[var(--color-muted)]">intdebug avail</dt>
            <dd>
              {item.diag.intdebugAvail ? "yes" : "no"}
              {!item.diag.intdebugAvail && (
                <span className="ml-2 text-[var(--color-warn)]">
                  (FW does not expose the fakepkg-friendly Register variant)
                </span>
              )}
            </dd>
            <dt className="text-[var(--color-muted)]">kernel R/W</dt>
            <dd>
              {item.diag.kernelRw ? "yes" : "no"}
              {!item.diag.kernelRw && (
                <span className="ml-2 text-[var(--color-warn)]">
                  (no cred elevation — load via :9021)
                </span>
              )}
            </dd>
            {/* 2.2.52-fix per-tier err breakdown — null = tier didn't run,
             *  0 = tier completed cleanly, otherwise Sony's err_code (or
             *  our 0xE000_xxxx machinery code). Lets the user see WHERE
             *  Tier 1 / Tier 2 broke when registerPath="none". */}
            {item.diag.shelluiErr !== null && (
              <>
                <dt className="text-[var(--color-muted)]">tier 1 (shellui)</dt>
                <dd>
                  {item.diag.shelluiErr === 0 ? (
                    <span className="text-[var(--color-good)]">accepted</span>
                  ) : (
                    <code className="text-[var(--color-bad)]">
                      0x{item.diag.shelluiErr.toString(16).padStart(8, "0")}
                    </code>
                  )}
                  {item.diag.shelluiErr === 0xe0000002 && (
                    <span className="ml-2 text-[var(--color-warn)]">
                      (libSceAppInstUtil not in ShellUI's address space)
                    </span>
                  )}
                </dd>
              </>
            )}
            {item.diag.appinstErr !== null && (
              <>
                <dt className="text-[var(--color-muted)]">tier 2 (appinst)</dt>
                <dd>
                  {item.diag.appinstErr === 0 ? (
                    <span className="text-[var(--color-good)]">accepted</span>
                  ) : (
                    <code className="text-[var(--color-bad)]">
                      0x{item.diag.appinstErr.toString(16).padStart(8, "0")}
                    </code>
                  )}
                  {item.diag.appinstErr === 0x80b22404 && (
                    <span className="ml-2 text-[var(--color-warn)]">
                      (PlayGo HTTP-404 — process-context reject; expected on
                      FW 9.60+)
                    </span>
                  )}
                </dd>
              </>
            )}
            {item.stagingPath && (
              <>
                <dt className="text-[var(--color-muted)]">staging path</dt>
                <dd className="break-all">
                  {item.stagingPath}
                  <span className="ml-2 text-[var(--color-muted)]">
                    (auto-deleted after install; payload sweeps stale &gt;24h)
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
