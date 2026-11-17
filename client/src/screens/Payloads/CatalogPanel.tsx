import { Link, useNavigate } from "react-router";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Send,
  ExternalLink,
  CheckCircle2,
  HardDrive,
  AlertTriangle,
  Trash2,
  X,
  PackagePlus,
  ListPlus,
  FileUp,
} from "lucide-react";
import { openExternalUrl as openExternal } from "../../lib/openExternalUrl";
import { PPR_PATCH_REPO } from "../../lib/fpkgPrereqs";
import { useConnectionStore } from "../../state/connection";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import {
  payloadsCatalog,
  payloadsLocalInventory,
  payloadsRemoveCustomRepo,
  payloadsSources,
  payloadsRemoveSource,
  sendPayloadChecked,
  type PayloadSourceInfo,
  type PayloadInfo,
  type PayloadLocalEntry,
} from "../../api/ps5";
import { payloadSendRefusalText } from "../../lib/payloadSendRefusal";
import { Button, ErrorCard, Badge, Input } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { pmOverview, pmStore } from "../../api/payloadManager";
import { recordPmStore } from "../../lib/pmUpdates";
import { hostOf } from "../../lib/addr";
import { payloadErrorText } from "../../lib/payloadErrors";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useTr } from "../../state/lang";
import { catalogText } from "../../lib/payloadCatalogText";
import UsbAutoloaderModal from "./UsbAutoloaderModal";
import PayloadUpdatesPanel from "./PayloadUpdatesPanel";
import { payloadCardLink } from "./payloadTabs";

/** The Send payload tab: the one place a file from this device is picked and sent. */
const SEND_TAB = "/payloads?tab=send";

/**
 * Catalog tab of the Payloads screen: a guide to third-party PS5 payloads.
 *
 * ps5upload doesn't download payloads. Each card says what the payload does and links to its
 * project page, where the user gets the file; "Send a file…" opens the Send payload tab to
 * send it. A copy an older version of the app downloaded into its library stays usable: it can
 * still be sent or stored in Payload Manager from its card.
 *
 * Pre-2.12.0 this lived at /payloads as a standalone screen. The
 * Payloads shell now owns the page header + tab strip; this panel
 * just renders content.
 */
export default function CatalogPanel() {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const [catalog, setCatalog] = useState<PayloadInfo[] | null>(null);
  const [inventory, setInventory] = useState<PayloadLocalEntry[]>([]);
  const [busyIds, setBusyIds] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [usbWizardOpen, setUsbWizardOpen] = useState(false);
  const [query, setQuery] = useState("");
  // Whether the console's Payload Manager (:8084) answers: it adds a "Store in Payload
  // Manager" action to each saved copy. Probed once per console; a miss hides it.
  const [pmPresent, setPmPresent] = useState(false);
  const { confirm: confirmStore, dialog: storeDialog } = useConfirm();
  useEffect(() => {
    const h = host.trim();
    setPmPresent(false);
    if (!h) return;
    let cancelled = false;
    pmOverview(h)
      .then((v) => {
        if (!cancelled) setPmPresent(v.present);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [host]);

  // Client-side filter over the already-loaded catalog (name / role /
  // description / repo). Cheap — the catalog is ~two dozen entries.
  const visibleCatalog = (catalog ?? []).filter((p) => {
    const q = query.trim().toLowerCase();
    if (!q) return true;
    const text = catalogText(tr, p);
    return [p.display_name, text.role, text.description, p.role, p.description, p.repo_owner, p.repo_name]
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
    // Mount-once: `refreshInventory` is identity-stable.
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

  // ── Payload lists added before (#829): shown so they can be removed; none can be added. ──
  const [sourcesOpen, setSourcesOpen] = useState(false);
  const [sources, setSources] = useState<PayloadSourceInfo[]>([]);
  const [sourceBusy, setSourceBusy] = useState(false);
  const [sourceError, setSourceError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    payloadsSources()
      .then((list) => {
        if (!cancelled) setSources(list);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  async function handleRemoveSource(s: PayloadSourceInfo) {
    if (sourceBusy) return;
    setSourceBusy(true);
    setSourceError(null);
    try {
      await payloadsRemoveSource(s.id);
      setSources(await payloadsSources());
      await reloadCatalog();
      await refreshInventory();
      pushNotification("info", tr("payloads_custom_removed", { name: s.name }, `Removed ${s.name}`));
    } catch (e) {
      setSourceError(payloadErrorText(tr, e));
    } finally {
      setSourceBusy(false);
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
      setErrors((prev) => ({ ...prev, [id]: payloadErrorText(tr, e) }));
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

  /** Keep a cached payload on the console, in Payload Manager's own storage, under the
   *  file's name (so it lands in the same folder Payload Manager's repository install would
   *  use). An existing folder is confirmed first: storing removes that folder's other files. */
  async function handleStore(id: string) {
    const local = inventoryById[id];
    const h = host.trim();
    if (!local || !h) return;
    const filename = local.path.split(/[\\/]/).pop() || `${id}.elf`;
    setBusy(id, true);
    try {
      let r = await pmStore(h, filename, local.path, false);
      if (!r.stored) {
        const ok = await confirmStore({
          title: tr(
            "payloads_pm_store_confirm_title",
            { folder: r.folder },
            `Replace Payload Manager's "${r.folder}" folder?`,
          ),
          message: r.fileExists
            ? tr(
                "payloads_pm_store_confirm_same",
                { name: filename },
                `Payload Manager already has ${filename}. Storing replaces it, and removes any other file in that folder.`,
              )
            : tr(
                "payloads_pm_store_confirm_other",
                { folder: r.folder },
                `Payload Manager already has a "${r.folder}" folder. Storing into it removes the other files there (an older version, usually).`,
              ),
          confirmLabel: tr("payloads_pm_store", undefined, "Store in Payload Manager"),
          destructive: true,
        });
        if (!ok) return;
        r = await pmStore(h, filename, local.path, true);
        if (!r.stored) return;
      }
      // So the Payload Manager tab can offer this one's updates later.
      recordPmStore(hostOf(h), filename, id, local.version);
      pushNotification(
        "success",
        withConsolePrefix(h, tr("payloads_pm_stored_title", { name: filename }, `Stored ${filename} in Payload Manager`)),
        {
          body: tr(
            "payloads_pm_stored_body_v2",
            { folder: r.folder },
            `It is in Payload Manager's "${r.folder}" folder. Payload Manager on the On your PS5 tab launches it or loads it at boot.`,
          ),
          link: payloadCardLink("payload-manager"),
        },
      );
    } catch (e) {
      setErrors((prev) => ({ ...prev, [id]: String(e) }));
      pushNotification(
        "error",
        withConsolePrefix(h, tr("payloads_pm_store_failed", { id }, `Couldn't store ${id} in Payload Manager`)),
        { body: String(e), link: "/payloads" },
      );
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
      const { viaPayloadManager } = await sendPayloadChecked(host.trim(), local.path);
      const version = local.version || "?";
      pushNotification(
        "success",
        withConsolePrefix(
          host,
          tr("payloads_sent_title", { id, host: host.trim() }, `Sent ${id} → ${host.trim()}`),
        ),
        {
          // Say which way it went: when :9021 is dead the file is launched
          // through Payload Manager (:8084) instead.
          body: viaPayloadManager
            ? tr(
                "payloads_sent_body_pm",
                { version },
                `Port 9021 didn't answer, so payload v${version} was launched through Payload Manager (:8084) instead. Check the PS5 for its notification.`,
              )
            : tr(
                "payloads_sent_body",
                { version },
                `Payload v${version} sent to the loader on :9021. Check the PS5 for its notification.`,
              ),
        },
      );
    } catch (e) {
      const why = payloadSendRefusalText(e);
      setErrors((prev) => ({ ...prev, [id]: why }));
      pushNotification("error", withConsolePrefix(host, tr("payloads_send_failed", { id }, `Send failed: ${id}`)), {
        body: why,
        link: "/payloads",
      });
    } finally {
      setBusy(id, false);
    }
  }

  if (!catalog) {
    // Loading vs failure: distinguish so the user isn't stuck on
    // "Loading catalogue…" forever when payloadsCatalog() rejected.
    if (errors._global) {
      return (
        <div className="w-full">
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
    <div className="w-full">
      <div className="space-y-3">
        {/* Essentials. The catalogue is a long alphabetical list, which is no
            help to someone who has just jailbroken a console and does not yet
            know which of these they actually need. Naming the three that pair
            with ps5upload is the guidance that was missing — kept as plain
            text rather than buttons so it never implies these are installed
            or endorsed beyond "start here". */}
        <div className="rounded-[var(--radius-card)] border border-[var(--color-accent)] bg-[var(--color-surface)] p-3">
          <p className="text-xs font-semibold">
            {tr("payloads_essentials_title", undefined, "Recommended alongside ps5upload")}
          </p>
          <p className="mt-1 text-xs text-[var(--color-muted)]">
            {tr(
              "payloads_essentials_body",
              undefined,
              "If you are setting up a console for the first time, these three cover most of what ps5upload expects to find: kstuff-lite (kernel patches most homebrew relies on), ShadowMount+ (mount game images from storage) and nanoDNS (keep the console off Sony's update path). Everything else in this list is optional.",
            )}
          </p>
          <p className="mt-1 text-xs text-[var(--color-muted)]">
            {tr(
              "payloads_fpkg_ppr_note_v2",
              undefined,
              "Installing fake packages also needs a53_ppr_install_fast.elf, which is not in this list: no download is published for it, and it is built from the source on GitHub. Upload & Install lists everything fake packages need, in order.",
            )}{" "}
            <button
              type="button"
              className="underline"
              onClick={() => void openExternal(PPR_PATCH_REPO)}
            >
              {"drakmor/ppr-patch"}
            </button>
          </p>
        </div>
        <p className="text-xs text-[var(--color-muted)]">
          <Link to="/connection" className="underline">
            {tr("connection_elfldr_first_title", undefined, "Load elfldr first.")}
          </Link>{" "}
          {tr("connection_elfldr_first_short", undefined, "With an autoloader or PLDMGR, put elfldr before ps5upload.elf, or the helper drops after a few seconds.")}
        </p>
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
          <div className="text-xs text-[var(--color-muted)]">
            {tr(
              "payloads_guide_blurb",
              { count: catalog.filter((p) => !p.is_custom && !p.source_id).length },
              `${catalog.filter((p) => !p.is_custom && !p.source_id).length} payloads that work with ps5upload. ps5upload doesn't download them: get a payload from its project page, then send the file with Send a file…`,
            )}
          </div>
          {/* Wraps on a phone: the search box takes its own full-width row
              instead of being squeezed to nothing beside the buttons. */}
          <div className="flex flex-wrap items-center gap-2">
            <div className="w-full sm:w-44">
              <Input
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={tr(
                  "payloads_search_placeholder",
                  undefined,
                  "Search payloads…",
                )}
                aria-label={tr("payloads_search", undefined, "Search payloads")}
                className="text-xs"
              />
            </div>
            {sources.length > 0 && (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<ListPlus size={12} />}
                onClick={() => {
                  setSourcesOpen((v) => !v);
                  setSourceError(null);
                }}
              >
                {tr("payloads_sources_button", undefined, "Payload sources")}
              </Button>
            )}
            {/* It writes to a stick plugged into this computer: not something a browser can do. */}
            {isTauriEnv() && (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<HardDrive size={12} />}
                onClick={() => setUsbWizardOpen(true)}
              >
                {tr(
                  "payloads_open_usb_wizard_v2",
                  undefined,
                  "Make a USB payload stick",
                )}
              </Button>
            )}
          </div>
        </div>
        {/* Updates are checked for copies saved in the app's library, which only an older
            version filled (the app no longer downloads payloads): with none, there is nothing
            to check, sync or roll back. */}
        {inventory.length > 0 && (
          <PayloadUpdatesPanel host={host} onChanged={() => void refreshInventory()} />
        )}
        {sourcesOpen && sources.length > 0 && (
          <div className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
            <div className="mb-2 flex items-center justify-between">
              <span className="text-xs font-medium text-[var(--color-text)]">
                {tr("payloads_sources_button", undefined, "Payload sources")}
              </span>
              <button
                type="button"
                onClick={() => setSourcesOpen(false)}
                className="text-[var(--color-muted)] hover:text-[var(--color-text)]"
                aria-label={tr("close", undefined, "Close")}
              >
                <X size={14} />
              </button>
            </div>
            <p className="mb-2 text-xs text-[var(--color-muted)]">
              {tr(
                "payloads_sources_hint_removed",
                undefined,
                "Payload lists you added before. ps5upload no longer reads them or downloads from them, and new ones can't be added. Copies already saved on this computer can still be sent.",
              )}
            </p>
            {sourceError && (
              <div className="mt-2 text-xs text-[var(--color-bad)]">{sourceError}</div>
            )}
            <ul className="mt-3 space-y-2">
              {sources.map((s) => (
                <li
                  key={s.id}
                  className="flex flex-wrap items-start justify-between gap-2 rounded-[var(--radius-card)] border border-[var(--color-border)] p-2"
                >
                  <div className="min-w-0 flex-1 text-xs">
                    <div className="font-medium text-[var(--color-text)]">{s.name}</div>
                    <div className="break-all text-[var(--color-muted)]">{s.url}</div>
                  </div>
                  <button
                    type="button"
                    onClick={() => void handleRemoveSource(s)}
                    disabled={sourceBusy}
                    className="inline-flex items-center gap-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-bad)] disabled:opacity-50"
                  >
                    <Trash2 size={11} />
                    {tr("payloads_custom_remove_short", undefined, "Remove")}
                  </button>
                </li>
              ))}
            </ul>
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
          // Without this banner an unreadable library would show every saved copy as missing.
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
          <div className="rounded-[var(--radius-card)] border border-dashed border-[var(--color-border)] p-6 text-center text-xs text-[var(--color-muted)]">
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
            local={inventoryById[p.id]}
            busy={busyIds.has(p.id)}
            error={errors[p.id]}
            onSend={() => handleSend(p.id)}
            onSendFile={() => navigate(SEND_TAB)}
            onStore={pmPresent ? () => handleStore(p.id) : undefined}
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
      {storeDialog}
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
  local,
  busy,
  error,
  onSend,
  onSendFile,
  onStore,
  onOpenHomepage,
  onRemove,
}: {
  info: PayloadInfo;
  /** The copy saved in the app's library (downloaded by an older version), if any. */
  local: PayloadLocalEntry | undefined;
  busy: boolean;
  error: string | undefined;
  onSend: () => void;
  /** Open the Send payload tab, to send a file the user got themselves. */
  onSendFile: () => void;
  /** Present when the console's Payload Manager answers: keep the saved copy there. */
  onStore?: () => void;
  onOpenHomepage: () => void;
  /** Present only for a user-added custom repo (#93) — renders a Remove
   *  control. Undefined for curated catalogue entries. */
  onRemove?: () => void;
}) {
  const tr = useTr();
  return (
    <section className="surface-panel p-5">
      <div className="flex flex-wrap items-start gap-3">
        {/* Floor the text column's min-width to min(100%,16rem), so at a large text size the
            buttons wrap onto a new line instead of squeezing the name to one letter a line. */}
        <div className="min-w-[min(100%,16rem)] flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="text-sm font-semibold">{info.display_name}</h3>
            {info.is_custom && (
              <Badge tone="neutral" variant="outline">
                {tr("payloads_custom_badge", undefined, "Custom")}
              </Badge>
            )}
            {info.source_name && (
              <Badge tone="neutral" variant="outline">
                {tr("payloads_source_badge", { name: info.source_name }, `From ${info.source_name}`)}
              </Badge>
            )}
            <span className="text-xs text-[var(--color-muted)]" title={info.homepage}>
              {info.repo_owner
                ? `${info.repo_owner}/${info.repo_name}`
                : info.homepage.replace(/^https:\/\/(www\.)?/, "").replace(/\/releases\/?$/, "")}
            </span>
            {onRemove && (
              <button
                type="button"
                onClick={onRemove}
                className="inline-flex items-center gap-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-bad)]"
                title={tr("payloads_custom_remove", undefined, "Remove this custom repo")}
              >
                <Trash2 size={11} />
                {tr("payloads_custom_remove_short", undefined, "Remove")}
              </button>
            )}
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {catalogText(tr, info).role}
          </div>
          <p className="mt-2 text-xs leading-relaxed text-[var(--color-text)]">
            {catalogText(tr, info).description}
          </p>
        </div>
        <div className="flex flex-col items-end gap-2">
          {local && (
            <div className="flex items-center gap-2 text-xs">
              <span className="inline-flex items-center gap-1 text-[var(--color-good)]">
                <CheckCircle2 size={11} />
                {tr(
                  "payloads_saved_copy",
                  { version: local.version || "—" },
                  `saved copy ${local.version || "—"}`,
                )}
              </span>
            </div>
          )}
          <div className="flex flex-wrap items-center justify-end gap-2">
            {info.homepage && (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<ExternalLink size={11} />}
                onClick={onOpenHomepage}
                title={info.homepage}
              >
                {tr("payloads_project_page", undefined, "Project page")}
              </Button>
            )}
            <Button
              variant={local ? "secondary" : "primary"}
              size="sm"
              leftIcon={<FileUp size={11} />}
              onClick={onSendFile}
              title={tr(
                "payloads_send_file_tooltip",
                undefined,
                "Send a payload file you got from the project page.",
              )}
            >
              {tr("payloads_send_file", undefined, "Send a file…")}
            </Button>
            {local && (
              <Button
                variant="primary"
                size="sm"
                leftIcon={<Send size={11} />}
                onClick={onSend}
                disabled={busy}
              >
                {tr("payloads_send_saved", undefined, "Send saved copy")}
              </Button>
            )}
            {local && onStore && (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<PackagePlus size={11} />}
                onClick={onStore}
                disabled={busy}
                title={tr(
                  "payloads_pm_store_tooltip",
                  undefined,
                  "Keep this file on the PS5 in Payload Manager, so it can be launched there or loaded at boot.",
                )}
              >
                {tr("payloads_pm_store", undefined, "Store in Payload Manager")}
              </Button>
            )}
          </div>
        </div>
      </div>

      {error && (
        <div className="mt-3 flex items-start gap-2 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-3 text-xs text-[var(--color-muted)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          <span className="min-w-0 break-words">{error}</span>
        </div>
      )}
    </section>
  );
}
