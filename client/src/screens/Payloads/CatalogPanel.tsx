import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Send,
  Download,
  ExternalLink,
  RefreshCw,
  CheckCircle2,
  Loader2,
  HardDrive,
  AlertTriangle,
  Activity as ActivityIcon,
} from "lucide-react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useConnectionStore } from "../../state/connection";
import { pushNotification } from "../../state/notifications";
import {
  payloadsCatalog,
  payloadsRelease,
  payloadsLocalInventory,
  payloadsDownload,
  sendPayload,
  type PayloadInfo,
  type PayloadReleaseInfo,
  type PayloadLocalEntry,
} from "../../api/ps5";
import { Button, ErrorCard } from "../../components";
import { useTr } from "../../state/lang";
import UsbAutoloaderModal from "./UsbAutoloaderModal";

/**
 * Catalog tab of the Payloads screen — curated catalogue of
 * third-party PS5 payloads.
 *
 * Each row shows: name, role, the latest GitHub release, a Download
 * button (or "up to date" once cached), and a Send button that
 * streams the cached ELF to the connected PS5's :9021 loader.
 *
 * State is split: catalogue + inventory load on mount; per-row release
 * info is fetched lazily as the user expands a row, so the page
 * doesn't fire 9 GitHub requests on every visit (we'd burn through
 * the 60/hr unauth rate limit on a lazy reload-spammer).
 *
 * Pre-2.12.0 this lived at /payloads as a standalone screen. The
 * Payloads shell now owns the page header + tab strip; this panel
 * just renders content.
 */
export default function CatalogPanel() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const [catalog, setCatalog] = useState<PayloadInfo[] | null>(null);
  const [inventory, setInventory] = useState<PayloadLocalEntry[]>([]);
  const [releases, setReleases] = useState<Record<string, PayloadReleaseInfo>>({});
  const [busyIds, setBusyIds] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [usbWizardOpen, setUsbWizardOpen] = useState(false);

  useEffect(() => {
    payloadsCatalog().then(setCatalog).catch((e) => {
      setErrors((prev) => ({ ...prev, _global: String(e) }));
    });
    refreshInventory();
    // Mount-once: `refreshInventory` is wrapped in useCallback with
    // an empty dep array, so it's identity-stable. Depending on it
    // here is safe but visually noisier than the empty array, and
    // the actual intent is "fetch on mount, no re-fetch."
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const refreshInventory = useCallback(async () => {
    try {
      const inv = await payloadsLocalInventory();
      setInventory(inv);
    } catch (e) {
      setErrors((prev) => ({ ...prev, _inventory: String(e) }));
    }
  }, []);

  const inventoryById = useMemo(() => {
    const m: Record<string, PayloadLocalEntry> = {};
    for (const i of inventory) m[i.payload_id] = i;
    return m;
  }, [inventory]);

  const setBusy = (id: string, busy: boolean) => {
    setBusyIds((prev) => {
      const next = new Set(prev);
      if (busy) next.add(id);
      else next.delete(id);
      return next;
    });
  };

  async function handleCheck(id: string, force: boolean) {
    setBusy(id, true);
    setErrors((prev) => {
      const { [id]: _drop, ...rest } = prev;
      void _drop;
      return rest;
    });
    try {
      const r = await payloadsRelease(id, force);
      setReleases((prev) => ({ ...prev, [id]: r }));
    } catch (e) {
      setErrors((prev) => ({ ...prev, [id]: String(e) }));
    } finally {
      setBusy(id, false);
    }
  }

  async function handleDownload(id: string) {
    const release = releases[id];
    if (!release || !release.picked_asset_url) return;
    setBusy(id, true);
    try {
      const local = await payloadsDownload(id, release.picked_asset_url, release.tag);
      await refreshInventory();
      pushNotification("success", `Cached ${id} ${release.tag}`, {
        body: `${(local.size / 1024).toFixed(1)} KB downloaded to local cache.`,
        link: "/payloads",
      });
    } catch (e) {
      setErrors((prev) => ({ ...prev, [id]: String(e) }));
      pushNotification("error", `Download failed: ${id}`, {
        body: String(e),
        link: "/payloads",
      });
    } finally {
      setBusy(id, false);
    }
  }

  async function handleSend(id: string) {
    const local = inventoryById[id];
    if (!local) return;
    if (!host?.trim()) {
      setErrors((prev) => ({
        ...prev,
        [id]: tr(
          "payloads_no_host",
          undefined,
          "No PS5 host set. Connect on the Connection screen first.",
        ),
      }));
      return;
    }
    setBusy(id, true);
    try {
      await sendPayload(host.trim(), local.path);
      pushNotification("success", `Sent ${id} → ${host.trim()}`, {
        body: `Payload v${local.version || "?"} streamed to :9021. Check the PS5 for the boot toast.`,
      });
    } catch (e) {
      setErrors((prev) => ({ ...prev, [id]: String(e) }));
      pushNotification("error", `Send failed: ${id}`, {
        body: String(e),
        link: "/payloads",
      });
    } finally {
      setBusy(id, false);
    }
  }

  if (!catalog) {
    // Loading vs failure: distinguish so the user isn't stuck on
    // "Loading catalogue…" forever when payloadsCatalog() rejected
    // (network down, JSON parse error, missing config). Without
    // this branch the error banner would only be rendered AFTER
    // `catalog` populated, which never happens on failure.
    if (errors._global) {
      return (
        <div className="mx-auto w-full max-w-5xl">
          <ErrorCard
            title={tr("payloads_load_failed", undefined, "Could not load catalogue")}
            detail={errors._global}
          />
        </div>
      );
    }
    return (
      <div className="text-sm text-[var(--color-muted)]">
        {tr("payloads_loading", undefined, "Loading catalogue…")}
      </div>
    );
  }

  return (
    <div className="mx-auto w-full max-w-5xl">
      <div className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
          <div className="text-xs text-[var(--color-muted)]">
            {tr(
              "payloads_curated_blurb",
              { count: catalog.length },
              `${catalog.length} curated payloads. Releases are fetched from GitHub on demand and cached for 1 hour.`,
            )}
          </div>
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<HardDrive size={12} />}
            onClick={() => setUsbWizardOpen(true)}
          >
            {tr(
              "payloads_open_usb_wizard",
              undefined,
              "Set up USB autoloader",
            )}
          </Button>
        </div>
        {errors._global && (
          <ErrorCard
            title={tr("payloads_load_failed", undefined, "Could not load catalogue")}
            detail={errors._global}
          />
        )}
        {errors._inventory && (
          // Inventory load failure: without this banner, a corrupted
          // or unreachable local cache would show every payload as
          // "not downloaded" — user would re-download and silently
          // overwrite, or worse, never realise their cached files
          // weren't being found.
          <ErrorCard
            title={tr(
              "payloads_inventory_failed",
              undefined,
              "Could not read local payload cache",
            )}
            detail={errors._inventory}
          />
        )}
        {catalog.map((p) => (
          <PayloadCard
            key={p.id}
            info={p}
            release={releases[p.id]}
            local={inventoryById[p.id]}
            busy={busyIds.has(p.id)}
            error={errors[p.id]}
            onCheck={(force) => handleCheck(p.id, force)}
            onDownload={() => handleDownload(p.id)}
            onSend={() => handleSend(p.id)}
            onOpenHomepage={() => {
              void openExternal(p.homepage);
            }}
          />
        ))}
      </div>
      {usbWizardOpen && (
        <UsbAutoloaderModal
          catalog={catalog}
          inventoryById={inventoryById}
          onClose={() => setUsbWizardOpen(false)}
        />
      )}
    </div>
  );
}

function PayloadCard({
  info,
  release,
  local,
  busy,
  error,
  onCheck,
  onDownload,
  onSend,
  onOpenHomepage,
}: {
  info: PayloadInfo;
  release: PayloadReleaseInfo | undefined;
  local: PayloadLocalEntry | undefined;
  busy: boolean;
  error: string | undefined;
  onCheck: (force: boolean) => void;
  onDownload: () => void;
  onSend: () => void;
  onOpenHomepage: () => void;
}) {
  const tr = useTr();
  const upToDate =
    !!local &&
    !!release &&
    !!release.tag &&
    local.version === release.tag;
  const updateAvailable =
    !!local && !!release && !!release.tag && local.version !== release.tag;
  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="text-sm font-semibold">{info.display_name}</h3>
            <button
              type="button"
              onClick={onOpenHomepage}
              className="inline-flex items-center gap-1 text-[11px] text-[var(--color-muted)] hover:text-[var(--color-accent)]"
              title={info.homepage}
            >
              <ExternalLink size={11} />
              {info.repo_owner}/{info.repo_name}
            </button>
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {info.role}
          </div>
          <p className="mt-2 text-xs leading-relaxed text-[var(--color-text)]">
            {info.description}
          </p>
        </div>
        <div className="flex flex-col items-end gap-2">
          <div className="flex items-center gap-2 text-[11px]">
            {local ? (
              upToDate ? (
                <span className="inline-flex items-center gap-1 text-[var(--color-good)]">
                  <CheckCircle2 size={11} />
                  {tr(
                    "payloads_up_to_date",
                    { version: local.version },
                    `cached ${local.version}`,
                  )}
                </span>
              ) : (
                <span className="inline-flex items-center gap-1 text-[var(--color-muted)]">
                  <ActivityIcon size={11} />
                  {tr(
                    "payloads_cached_version",
                    { version: local.version || "—" },
                    `cached ${local.version || "—"}`,
                  )}
                </span>
              )
            ) : (
              <span className="text-[var(--color-muted)]">
                {tr("payloads_not_cached", undefined, "not downloaded")}
              </span>
            )}
          </div>
          <div className="flex flex-wrap items-center justify-end gap-2">
            <Button
              variant="ghost"
              size="sm"
              leftIcon={
                busy ? (
                  <Loader2 size={11} className="animate-spin" />
                ) : (
                  <RefreshCw size={11} />
                )
              }
              onClick={() => onCheck(true)}
              disabled={busy}
              title={tr(
                "payloads_check_tooltip",
                undefined,
                "Check the latest GitHub release (force refresh)",
              )}
            >
              {release
                ? tr("payloads_refresh", undefined, "Refresh")
                : tr("payloads_check", undefined, "Check release")}
            </Button>
            {release && release.picked_asset_url && (
              <Button
                variant={updateAvailable || !local ? "primary" : "secondary"}
                size="sm"
                leftIcon={<Download size={11} />}
                onClick={onDownload}
                disabled={busy}
              >
                {updateAvailable
                  ? tr(
                      "payloads_update",
                      { version: release.tag },
                      `Update to ${release.tag}`,
                    )
                  : local
                    ? tr("payloads_redownload", undefined, "Re-download")
                    : tr(
                        "payloads_download",
                        { version: release.tag },
                        `Download ${release.tag}`,
                      )}
              </Button>
            )}
            {local && (
              <Button
                variant="primary"
                size="sm"
                leftIcon={<Send size={11} />}
                onClick={onSend}
                disabled={busy}
              >
                {tr("payloads_send", undefined, "Send to PS5")}
              </Button>
            )}
          </div>
        </div>
      </div>

      {release && (
        <div className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-[11px]">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[var(--color-muted)]">
            <span>
              {tr(
                "payloads_release_info",
                { tag: release.tag, when: release.published_at || "—" },
                `${release.tag} · published ${release.published_at || "—"}`,
              )}
            </span>
            {release.picked_asset_name && (
              <span title={release.picked_asset_url}>
                {release.picked_asset_name} ·{" "}
                {(release.picked_asset_size / 1024).toFixed(1)} KB
              </span>
            )}
            {release.cached_age_secs > 0 && (
              <span>
                {tr(
                  "payloads_cached_ago",
                  { age: release.cached_age_secs },
                  `cached ${release.cached_age_secs}s ago`,
                )}
              </span>
            )}
          </div>
          {release.refresh_error && (
            // Live fetch failed (GitHub down, rate-limited, etc.)
            // but we have a cached snapshot. Show that explicitly
            // instead of swallowing it — without this banner the
            // user has no way to tell live data from N-day-stale
            // cache, and might wonder why the "latest" version
            // doesn't match a release they just saw on GitHub.
            <div className="mt-2 flex items-start gap-1.5 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-warn)]">
              <AlertTriangle size={11} className="mt-0.5 shrink-0" />
              <span>
                {tr(
                  "payloads_refresh_error_banner",
                  undefined,
                  "Couldn't refresh from GitHub — showing cached snapshot. The latest tag on GitHub may be newer than what's shown here.",
                )}{" "}
                <span className="text-[var(--color-muted)]">
                  ({release.refresh_error})
                </span>
              </span>
            </div>
          )}
          {release.body && (
            <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-words text-[var(--color-text)]">
              {release.body}
            </pre>
          )}
        </div>
      )}

      {error && (
        <div className="mt-3 flex items-start gap-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          <span className="break-words">{error}</span>
        </div>
      )}
    </section>
  );
}
