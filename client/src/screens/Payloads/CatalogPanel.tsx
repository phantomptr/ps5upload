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
  Plus,
  Trash2,
  X,
} from "lucide-react";
import { openExternalUrl as openExternal } from "../../lib/openExternalUrl";
import { useConnectionStore } from "../../state/connection";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import {
  payloadsCatalog,
  payloadsReleases,
  payloadsLocalInventory,
  payloadsDownload,
  payloadsAddCustomRepo,
  payloadsRemoveCustomRepo,
  sendPayload,
  type PayloadInfo,
  type PayloadReleaseInfo,
  type PayloadLocalEntry,
} from "../../api/ps5";
import {
  defaultRelease,
  downloadableReleases,
  isLatestTag,
} from "../../lib/payloadVersions";
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
  // `releases[id]` is the currently-SELECTED release for each payload (drives
  // the Download button + info box). `releaseLists[id]` is the full version
  // history for the picker; `selectedTag[id]` is the user's chosen version.
  const [releases, setReleases] = useState<Record<string, PayloadReleaseInfo>>(
    {},
  );
  const [releaseLists, setReleaseLists] = useState<
    Record<string, PayloadReleaseInfo[]>
  >({});
  const [selectedTag, setSelectedTag] = useState<Record<string, string>>({});
  const [busyIds, setBusyIds] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [usbWizardOpen, setUsbWizardOpen] = useState(false);
  const [query, setQuery] = useState("");

  // Client-side filter over the already-loaded catalog (name / role /
  // description / repo). Cheap — the catalog is ~two dozen entries.
  const visibleCatalog = (catalog ?? []).filter((p) => {
    const q = query.trim().toLowerCase();
    if (!q) return true;
    return [p.display_name, p.role, p.description, p.repo_owner, p.repo_name]
      .join(" ")
      .toLowerCase()
      .includes(q);
  });

  useEffect(() => {
    payloadsCatalog()
      .then(setCatalog)
      .catch((e) => {
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

  const reloadCatalog = useCallback(async () => {
    try {
      setCatalog(await payloadsCatalog());
    } catch (e) {
      setErrors((prev) => ({ ...prev, _global: String(e) }));
    }
  }, []);

  // ── Add-a-custom-repo form (#93) ────────────────────────────────────
  const [addOpen, setAddOpen] = useState(false);
  const [addRepo, setAddRepo] = useState(""); // "owner/name" or a full URL
  const [addHost, setAddHost] = useState("github.com");
  const [addBusy, setAddBusy] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);

  /** Parse "owner/name", "github.com/owner/name", or a full repo URL into
   *  { host, owner, name }. Returns null if it doesn't look like a repo ref. */
  function parseRepoRef(
    raw: string,
    fallbackHost: string,
  ): { host: string; owner: string; name: string } | null {
    let s = raw.trim();
    if (!s) return null;
    s = s.replace(/^https?:\/\//i, "").replace(/\.git$/i, "").replace(/\/+$/, "");
    const parts = s.split("/").filter(Boolean);
    if (parts.length >= 3) {
      // host/owner/name (extra path segments like /releases are ignored)
      return { host: parts[0].toLowerCase(), owner: parts[1], name: parts[2] };
    }
    if (parts.length === 2) {
      // owner/name — use the chosen host field
      return { host: fallbackHost.trim().toLowerCase(), owner: parts[0], name: parts[1] };
    }
    return null;
  }

  async function handleAddCustomRepo() {
    const ref = parseRepoRef(addRepo, addHost);
    if (!ref) {
      setAddError(
        tr(
          "payloads_custom_parse_error",
          undefined,
          "Enter owner/name (e.g. EchoStretch/kstuff-lite) or a full repo URL.",
        ),
      );
      return;
    }
    setAddBusy(true);
    setAddError(null);
    try {
      const info = await payloadsAddCustomRepo({
        host: ref.host,
        owner: ref.owner,
        name: ref.name,
      });
      await reloadCatalog();
      await refreshInventory();
      setAddRepo("");
      setAddOpen(false);
      pushNotification(
        "success",
        tr("payloads_custom_added", { name: info.display_name }, `Added ${info.display_name}`),
        { body: tr("payloads_custom_added_body", undefined, "Check its releases and send it like any other payload.") },
      );
    } catch (e) {
      setAddError(e instanceof Error ? e.message : String(e));
    } finally {
      setAddBusy(false);
    }
  }

  async function handleRemoveCustomRepo(id: string, name: string) {
    try {
      await payloadsRemoveCustomRepo(id);
      await reloadCatalog();
      await refreshInventory();
      pushNotification(
        "info",
        tr("payloads_custom_removed", { name }, `Removed ${name}`),
      );
    } catch (e) {
      setErrors((prev) => ({ ...prev, [id]: String(e) }));
    }
  }

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
      // Fetch the full release history (newest-first) so the user can pick
      // a version / downgrade — and default the selection to the newest
      // STABLE build (the latest tag is often a fast-moving pre-release).
      const list = await payloadsReleases(id, force);
      setReleaseLists((prev) => ({ ...prev, [id]: list }));
      const def = defaultRelease(list);
      const tag = def?.tag ?? list[0]?.tag ?? "";
      setSelectedTag((prev) => ({ ...prev, [id]: tag }));
      const sel = list.find((r) => r.tag === tag) ?? list[0];
      if (sel) setReleases((prev) => ({ ...prev, [id]: sel }));
    } catch (e) {
      setErrors((prev) => ({ ...prev, [id]: String(e) }));
    } finally {
      setBusy(id, false);
    }
  }

  // User picked a specific version in the dropdown — switch the selected
  // release (drives the Download button + info box).
  function handleSelectTag(id: string, tag: string) {
    setSelectedTag((prev) => ({ ...prev, [id]: tag }));
    const sel = releaseLists[id]?.find((r) => r.tag === tag);
    if (sel) setReleases((prev) => ({ ...prev, [id]: sel }));
  }

  async function handleDownload(id: string) {
    const release = releases[id];
    if (!release || !release.picked_asset_url) return;
    setBusy(id, true);
    try {
      const local = await payloadsDownload(
        id,
        release.picked_asset_url,
        release.tag,
      );
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
      pushNotification(
        "success",
        withConsolePrefix(host, `Sent ${id} → ${host.trim()}`),
        {
          body: `Payload v${local.version || "?"} streamed to :9021. Check the PS5 for the boot toast.`,
        },
      );
    } catch (e) {
      setErrors((prev) => ({ ...prev, [id]: String(e) }));
      pushNotification("error", withConsolePrefix(host, `Send failed: ${id}`), {
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
            title={tr(
              "payloads_load_failed",
              undefined,
              "Could not load catalogue",
            )}
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
          <div className="flex items-center gap-2">
            <input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={tr(
                "payloads_search_placeholder",
                undefined,
                "Search payloads…",
              )}
              aria-label={tr("payloads_search", undefined, "Search payloads")}
              className="w-44 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2.5 py-1.5 text-xs outline-none focus:border-[var(--color-accent)]"
            />
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Plus size={12} />}
              onClick={() => {
                setAddOpen((v) => !v);
                setAddError(null);
              }}
            >
              {tr("payloads_add_repo", undefined, "Add repo")}
            </Button>
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
        </div>
        {addOpen && (
          <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
            <div className="mb-2 flex items-center justify-between">
              <span className="text-xs font-medium text-[var(--color-text)]">
                {tr("payloads_add_repo_title", undefined, "Track a custom payload repo")}
              </span>
              <button
                type="button"
                onClick={() => setAddOpen(false)}
                className="text-[var(--color-muted)] hover:text-[var(--color-text)]"
                aria-label={tr("close", undefined, "Close")}
              >
                <X size={14} />
              </button>
            </div>
            <p className="mb-2 text-[11px] text-[var(--color-muted)]">
              {tr(
                "payloads_add_repo_hint",
                undefined,
                "Paste owner/name (e.g. EchoStretch/kstuff-lite) or a full repo URL. Releases are fetched the same way as the curated list; the same ELF/zip checks apply on download. Only add repos you trust — ps5upload doesn't vet what a custom repo ships.",
              )}
            </p>
            <div className="flex flex-wrap items-center gap-2">
              <input
                type="text"
                value={addRepo}
                onChange={(e) => setAddRepo(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !addBusy) void handleAddCustomRepo();
                }}
                placeholder={tr(
                  "payloads_add_repo_placeholder",
                  undefined,
                  "owner/name or https://github.com/owner/name",
                )}
                spellCheck={false}
                className="min-w-[260px] flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2.5 py-1.5 text-xs outline-none focus:border-[var(--color-accent)]"
              />
              <label className="flex items-center gap-1 text-[11px] text-[var(--color-muted)]">
                {tr("payloads_add_repo_host", undefined, "Host")}
                <input
                  type="text"
                  value={addHost}
                  onChange={(e) => setAddHost(e.target.value)}
                  spellCheck={false}
                  title={tr(
                    "payloads_add_repo_host_hint",
                    undefined,
                    "Used only when you enter a bare owner/name. GitHub or a Gitea/Forgejo host.",
                  )}
                  className="w-36 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-xs outline-none focus:border-[var(--color-accent)]"
                />
              </label>
              <Button
                variant="primary"
                size="sm"
                loading={addBusy}
                disabled={addBusy || !addRepo.trim()}
                onClick={() => void handleAddCustomRepo()}
              >
                {tr("payloads_add_repo_submit", undefined, "Add")}
              </Button>
            </div>
            {addError && (
              <div className="mt-2 text-[11px] text-[var(--color-danger)]">{addError}</div>
            )}
          </div>
        )}
        {errors._global && (
          <ErrorCard
            title={tr(
              "payloads_load_failed",
              undefined,
              "Could not load catalogue",
            )}
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
        {query.trim() && visibleCatalog.length === 0 && (
          <div className="rounded-md border border-dashed border-[var(--color-border)] p-6 text-center text-xs text-[var(--color-muted)]">
            {tr(
              "payloads_search_no_match",
              { query: query.trim() },
              `No payloads match "${query.trim()}".`,
            )}
          </div>
        )}
        {visibleCatalog.map((p) => (
          <PayloadCard
            key={p.id}
            info={p}
            release={releases[p.id]}
            releaseList={releaseLists[p.id]}
            selectedTag={selectedTag[p.id]}
            onSelectTag={(tag) => handleSelectTag(p.id, tag)}
            local={inventoryById[p.id]}
            busy={busyIds.has(p.id)}
            error={errors[p.id]}
            onCheck={(force) => handleCheck(p.id, force)}
            onDownload={() => handleDownload(p.id)}
            onSend={() => handleSend(p.id)}
            onOpenHomepage={() => {
              void openExternal(p.homepage);
            }}
            onRemove={
              p.is_custom
                ? () => void handleRemoveCustomRepo(p.id, p.display_name)
                : undefined
            }
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
  releaseList,
  selectedTag,
  onSelectTag,
  local,
  busy,
  error,
  onCheck,
  onDownload,
  onSend,
  onOpenHomepage,
  onRemove,
}: {
  info: PayloadInfo;
  release: PayloadReleaseInfo | undefined;
  releaseList: PayloadReleaseInfo[] | undefined;
  selectedTag: string | undefined;
  onSelectTag: (tag: string) => void;
  local: PayloadLocalEntry | undefined;
  busy: boolean;
  error: string | undefined;
  onCheck: (force: boolean) => void;
  onDownload: () => void;
  onSend: () => void;
  onOpenHomepage: () => void;
  /** Present only for a user-added custom repo (#93) — renders a Remove
   *  control. Undefined for curated catalogue entries. */
  onRemove?: () => void;
}) {
  const tr = useTr();
  // Versions the picker offers (only those with a downloadable asset).
  const pickable = releaseList ? downloadableReleases(releaseList) : [];
  const selectedIsPrerelease = !!release?.prerelease;
  const selectedIsLatest =
    !!release && isLatestTag(releaseList ?? [], release.tag);
  const upToDate =
    !!local && !!release && !!release.tag && local.version === release.tag;
  const updateAvailable =
    !!local && !!release && !!release.tag && local.version !== release.tag;
  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <div className="flex flex-wrap items-start gap-3">
        {/* Floor the text column's min-width to min(100%,16rem). With `flex-1`
            (basis 0%) + `min-w-0`, the name column shrank to nothing at large
            Text size while the version-select + Download button held the row
            on one line — wrapping the name "k/l/o/g/s/r/v" one letter per
            line. A real min-width makes flex-wrap push the controls onto a
            new line instead; min(100%,…) keeps a lone column from overflowing
            a narrow viewport. */}
        <div className="min-w-[min(100%,16rem)] flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="text-sm font-semibold">{info.display_name}</h3>
            {info.is_custom && (
              <span className="rounded-full border border-[var(--color-border)] px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-[var(--color-muted)]">
                {tr("payloads_custom_badge", undefined, "Custom")}
              </span>
            )}
            <button
              type="button"
              onClick={onOpenHomepage}
              className="inline-flex items-center gap-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-accent)]"
              title={info.homepage}
            >
              <ExternalLink size={11} />
              {info.repo_owner}/{info.repo_name}
            </button>
            {onRemove && (
              <button
                type="button"
                onClick={onRemove}
                className="inline-flex items-center gap-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-danger)]"
                title={tr("payloads_custom_remove", undefined, "Remove this custom repo")}
              >
                <Trash2 size={11} />
                {tr("payloads_custom_remove_short", undefined, "Remove")}
              </button>
            )}
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {info.role}
          </div>
          <p className="mt-2 text-xs leading-relaxed text-[var(--color-text)]">
            {info.description}
          </p>
        </div>
        <div className="flex flex-col items-end gap-2">
          <div className="flex items-center gap-2 text-xs">
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
            {pickable.length > 1 && selectedTag && (
              <label className="flex items-center gap-1">
                <span className="sr-only">
                  {tr("payloads_version", undefined, "Version")}
                </span>
                <select
                  value={selectedTag}
                  onChange={(e) => onSelectTag(e.target.value)}
                  disabled={busy}
                  className="max-w-[12rem] rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-1.5 py-1 text-xs"
                  title={tr(
                    "payloads_version_tooltip",
                    undefined,
                    "Pick a version. Older versions let you downgrade if the latest is unstable.",
                  )}
                >
                  {pickable.map((r) => (
                    <option key={r.tag} value={r.tag}>
                      {r.tag}
                      {isLatestTag(releaseList ?? [], r.tag)
                        ? ` (${tr("payloads_latest", undefined, "latest")})`
                        : ""}
                      {r.prerelease
                        ? ` — ${tr("payloads_prerelease", undefined, "pre-release")}`
                        : ""}
                    </option>
                  ))}
                </select>
              </label>
            )}
            {release && release.picked_asset_url && (
              <Button
                variant={updateAvailable || !local ? "primary" : "secondary"}
                size="sm"
                leftIcon={<Download size={11} />}
                onClick={onDownload}
                disabled={busy}
              >
                {/* Wording: "Update to" only when picking the genuine latest
                    and it's newer than cached; otherwise neutral "Download"
                    (covers re-download AND downgrade to an older tag). */}
                {updateAvailable && selectedIsLatest
                  ? tr(
                      "payloads_update",
                      { version: release.tag },
                      `Update to ${release.tag}`,
                    )
                  : local && local.version === release.tag
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
        <div className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[var(--color-muted)]">
            <span>
              {tr(
                "payloads_release_info",
                { tag: release.tag, when: release.published_at || "—" },
                `${release.tag} · published ${release.published_at || "—"}`,
              )}
            </span>
            {selectedIsPrerelease && (
              <span className="inline-flex items-center gap-1 rounded-full border border-[var(--color-warn)] px-1.5 py-0.5 font-medium text-[var(--color-warn)]">
                <AlertTriangle size={10} />
                {tr(
                  "payloads_prerelease_warn",
                  undefined,
                  "pre-release — may be unstable",
                )}
              </span>
            )}
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
            <div className="mt-2 flex items-start gap-1.5 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-warn)]">
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
        // Muted, NOT alarming red. A source failing to fetch its release list
        // (a renamed/deleted repo → 404, or a down Forgejo host) is not a
        // user error — the row stays present and usable later. The loud red
        // card made users think the payload was "broken" or "removed".
        <div className="mt-3 flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-muted)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          <span className="min-w-0 break-words">
            {tr(
              "payloads_source_unreachable",
              undefined,
              "Couldn't reach this source right now — try Refresh later.",
            )}
            <span className="mt-0.5 block break-words opacity-70">{error}</span>
          </span>
        </div>
      )}
    </section>
  );
}
