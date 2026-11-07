import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowDown, ArrowUp, Download, Play, Plus, RefreshCw, Trash2, Upload } from "lucide-react";

import { Badge, Button, Callout, ErrorCard, Select, Spinner, Toggle } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import {
  autoloadKey,
  pmDelete,
  pmLaunch,
  pmOverview,
  pmSetAutoload,
  pmStore,
  pmStoreBytes,
  type PmOverview,
  type PmStoreResult,
  type PmStoredPayload,
} from "../../api/payloadManager";
import {
  payloadsCatalog,
  payloadsDownload,
  payloadsLocalInventory,
  payloadsReleases,
  type PayloadInfo,
  type PayloadLocalEntry,
  type PayloadReleaseInfo,
} from "../../api/ps5";
import { hostOf } from "../../lib/addr";
import { defaultRelease } from "../../lib/payloadVersions";
import {
  catalogueMatch,
  forgetPmStore,
  isRepoManaged,
  pmRecords,
  pmUpdateFor,
  recordPmStore,
  swapAutoloadKey,
} from "../../lib/pmUpdates";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { humanizeCaught } from "../../lib/humanizeError";
import { listsHelper, loadBootRecord, storedHelper } from "../../lib/helperAtBoot";
import { turnOffHelperAtBoot, turnOnHelperAtBoot } from "../../state/helperAtBootRuntime";

/** Move `name` within an autoload list, keeping pauses (`!ms`) where they are. */
export function moveEntry(list: string[], name: string, dir: -1 | 1): string[] {
  const i = list.indexOf(name);
  if (i < 0) return list;
  const names = list.map((e, idx) => ({ e, idx })).filter((x) => !x.e.startsWith("!"));
  const pos = names.findIndex((x) => x.idx === i);
  const swapWith = names[pos + dir];
  if (!swapWith) return list;
  const out = [...list];
  out[i] = swapWith.e;
  out[swapWith.idx] = name;
  return out;
}

/** The payloads stored in the console's Payload Manager (:8084): launch, delete, and the
 *  boot autoload list. Storing into it is on the Catalog tab's cards. */
export default function PayloadManagerPanel() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const { confirm, dialog } = useConfirm();
  const [view, setView] = useState<PmOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  /** The catalogue's current release for each payload this app stored here (native builds:
   *  the catalogue talks to GitHub from the app). */
  const [latest, setLatest] = useState<Record<string, PayloadReleaseInfo | null>>({});
  /** Catalogue payloads downloaded to this computer (native builds), to store or match. */
  const [inventory, setInventory] = useState<PayloadLocalEntry[]>([]);
  const [catalog, setCatalog] = useState<PayloadInfo[]>([]);
  const [pick, setPick] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);

  const refresh = useCallback(async () => {
    const h = host.trim();
    if (!h) return;
    setLoading(true);
    setError(null);
    try {
      setView(await pmOverview(h));
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setLoading(false);
    }
  }, [host]);

  useEffect(() => {
    setView(null);
    void refresh();
  }, [refresh]);

  const present = !!view?.present;
  useEffect(() => {
    if (!isTauriEnv() || !present) return;
    let live = true;
    payloadsLocalInventory()
      .then((v) => live && setInventory(v))
      .catch(() => undefined);
    payloadsCatalog()
      .then((v) => live && setCatalog(v))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [present]);

  useEffect(() => {
    if (!isTauriEnv() || !view?.present) return;
    const ids = new Set(Object.values(pmRecords(hostOf(host))).map((r) => r.id));
    for (const p of view.payloads) {
      const id = catalogueMatch(p, inventory);
      if (id) ids.add(id);
    }
    let live = true;
    for (const id of ids) {
      if (id in latest) continue;
      payloadsReleases(id)
        .then((list) => live && setLatest((m) => ({ ...m, [id]: defaultRelease(list) })))
        .catch(() => live && setLatest((m) => ({ ...m, [id]: null })));
    }
    return () => {
      live = false;
    };
  }, [view, host, latest, inventory]);

  const run = async (key: string, work: () => Promise<void>, failTitle: string) => {
    setBusy(key);
    try {
      await work();
      await refresh();
    } catch (e) {
      pushNotification("error", withConsolePrefix(host, failTitle), {
        body: humanizeCaught(e),
      });
    } finally {
      setBusy(null);
    }
  };

  const launch = (p: PmStoredPayload) =>
    run(
      `launch:${p.path}`,
      async () => {
        await pmLaunch(host, p.path);
        pushNotification(
          "success",
          withConsolePrefix(host, tr("pm_launched", { name: p.filename }, `Launched ${p.filename}`)),
          { body: tr("pm_launched_body", undefined, "Check the PS5 for its notification.") },
        );
      },
      tr("pm_launch_failed", { name: p.filename }, `Couldn't launch ${p.filename}`),
    );

  const remove = async (p: PmStoredPayload) => {
    const ok = await confirm({
      title: tr("pm_delete_confirm_title", { name: p.filename }, `Delete ${p.filename} from Payload Manager?`),
      message: tr(
        "pm_delete_confirm_body",
        undefined,
        "The stored file and its details are removed from the console. A copy downloaded to this computer stays.",
      ),
      confirmLabel: tr("delete", undefined, "Delete"),
      destructive: true,
    });
    if (!ok) return;
    await run(
      `delete:${p.path}`,
      () => pmDelete(host, p.filename),
      tr("pm_delete_failed", { name: p.filename }, `Couldn't delete ${p.filename}`),
    );
  };

  /** Downloads the newer release, stores it in Payload Manager (which replaces the old file in
   *  its folder), keeps its place in the boot list, and drops an old copy left under another name. */
  const update = (p: PmStoredPayload, id: string, to: PayloadReleaseInfo) =>
    run(
      `update:${p.path}`,
      async () => {
        const h = hostOf(host);
        const local = await payloadsDownload(id, to.picked_asset_url, to.tag);
        const name = local.path.split(/[\\/]/).pop() || p.filename;
        const r = await pmStore(h, name, local.path, true);
        if (!r.stored) throw new Error("Payload Manager did not store it");
        const after = await pmOverview(h);
        if (after.present) {
          const fresh = after.payloads.find((x) => x.filename === name && x.path !== p.path) ??
            after.payloads.find((x) => x.filename === name);
          const oldKey = autoloadKey(p);
          if (fresh && after.autoload?.list.includes(oldKey)) {
            await pmSetAutoload(h, after.autoload.enabled, swapAutoloadKey(after.autoload.list, oldKey, autoloadKey(fresh)));
          }
          if (name !== p.filename && after.payloads.some((x) => x.path === p.path)) await pmDelete(h, p.filename);
        }
        if (name !== p.filename) forgetPmStore(h, p.filename);
        recordPmStore(h, name, id, to.tag);
        pushNotification(
          "success",
          withConsolePrefix(host, tr("pm_updated", { name, tag: to.tag }, `Updated ${name} to ${to.tag}`)),
        );
      },
      tr("pm_update_failed", { name: p.filename }, `Couldn't update ${p.filename}`),
    );

  /** Stores under `filename`, asking first when Payload Manager already has that folder
   *  (storing into it removes the folder's other files). False when the user said no. */
  const storeConfirmed = async (
    filename: string,
    store: (confirmed: boolean) => Promise<PmStoreResult>,
  ): Promise<boolean> => {
    let r = await store(false);
    if (!r.stored) {
      const ok = await confirm({
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
      if (!ok) return false;
      r = await store(true);
      if (!r.stored) return false;
    }
    pushNotification(
      "success",
      withConsolePrefix(host, tr("payloads_pm_stored_title", { name: filename }, `Stored ${filename} in Payload Manager`)),
    );
    return true;
  };

  /** A payload file picked on this computer (any build: the bytes go to the engine). */
  const addLocal = (file: File) =>
    run(
      "add:local",
      async () => {
        const bytes = await file.arrayBuffer();
        const h = hostOf(host);
        if (await storeConfirmed(file.name, (c) => pmStoreBytes(h, file.name, bytes, c))) {
          // Not a catalogue payload: no update tracking for it.
          forgetPmStore(h, file.name);
        }
      },
      tr("pm_add_failed", { name: file.name }, `Couldn't store ${file.name} in Payload Manager`),
    );

  /** A catalogue payload already downloaded to this computer; remembered so updates are offered. */
  const addCatalogue = (id: string) => {
    const local = inventory.find((e) => e.payload_id === id);
    if (!local) return;
    const filename = local.path.split(/[\\/]/).pop() || `${id}.elf`;
    return run(
      "add:catalogue",
      async () => {
        const h = hostOf(host);
        if (await storeConfirmed(filename, (c) => pmStore(h, filename, local.path, c))) {
          recordPmStore(h, filename, id, local.version);
          setPick("");
        }
      },
      tr("pm_add_failed", { name: filename }, `Couldn't store ${filename} in Payload Manager`),
    );
  };

  const setAutoload = (enabled: boolean, list: string[]) =>
    run(
      "autoload",
      () => pmSetAutoload(host, enabled, list),
      tr("pm_autoload_failed", undefined, "Couldn't update the autoload list"),
    );

  /** "Load the PS5Upload helper at boot" (lib/helperAtBoot). */
  const setHelperAtBoot = (on: boolean) =>
    run(
      "helper-boot",
      async () => {
        if (!on) {
          await turnOffHelperAtBoot(host);
          return;
        }
        const done = await turnOnHelperAtBoot(host, (folder) =>
          confirm({
            title: tr(
              "payloads_pm_store_confirm_title",
              { folder },
              `Replace Payload Manager's "${folder}" folder?`,
            ),
            message: tr(
              "payloads_pm_store_confirm_other",
              { folder },
              `Payload Manager already has a "${folder}" folder. Storing into it removes the other files there (an older version, usually).`,
            ),
            confirmLabel: tr("payloads_pm_store", undefined, "Store in Payload Manager"),
            destructive: true,
          }),
        );
        if (done)
          pushNotification(
            "success",
            withConsolePrefix(host, tr("helper_boot_on_title", undefined, "The helper now loads at boot")),
          );
      },
      tr("helper_boot_failed", undefined, "Couldn't change whether the helper loads at boot"),
    );

  if (!host.trim()) {
    return (
      <ErrorCard title={tr("payloads_no_host", undefined, "No PS5 host set. Connect on the Connection screen first.")} />
    );
  }
  if (error) {
    return (
      <ErrorCard
        title={tr("pm_error_title", undefined, "Couldn't reach Payload Manager")}
        detail={error}
        onRetry={() => void refresh()}
      />
    );
  }
  if (!view) {
    return (
      <div className="flex items-center gap-2 text-sm text-[var(--color-muted)]">
        <Spinner size={14} />
        {tr("pm_probing", undefined, "Looking for Payload Manager on the PS5…")}
      </div>
    );
  }
  if (!view.present) {
    return (
      <Callout tone="info" title={tr("pm_absent_title", undefined, "Payload Manager isn't running on this PS5")}>
        {tr(
          "pm_absent_body",
          undefined,
          "Payload Manager (itsPLK) keeps payloads on the console and can load them at boot. Send it from the Catalog tab, or start it on the PS5, then come back here.",
        )}
        <p className="mt-1">
          {tr(
            "helper_boot_needs_pm",
            undefined,
            "Loading the PS5Upload helper at boot needs Payload Manager: it is what starts the helper when the PS5 starts.",
          )}
        </p>
      </Callout>
    );
  }

  const autoload = view.autoload;
  const helperCopy = storedHelper(view.payloads);
  const helperListed = !!helperCopy && listsHelper(autoload, autoloadKey(helperCopy));
  const bootRecord = loadBootRecord(host);
  const inList = (p: PmStoredPayload) => !!autoload?.list.includes(autoloadKey(p));
  const stored = view.payloads.filter((p) => p.path.startsWith("/data/pldmgr/"));
  const onUsb = view.payloads.filter((p) => !p.path.startsWith("/data/pldmgr/"));

  const records = pmRecords(hostOf(host));
  const nameOf = (id: string) => catalog.find((c) => c.id === id)?.display_name ?? id;
  const row = (p: PmStoredPayload) => {
    const recorded = isRepoManaged(p) ? undefined : records[p.filename];
    // Stored by hand under the same name as a catalogue download: offer the catalogue's release
    // (Payload Manager keeps no version for it, so "newer" cannot be told).
    const matchId = recorded ? null : catalogueMatch(p, inventory);
    const matchTo = matchId ? latest[matchId] : null;
    const rec = recorded ?? (matchId && matchTo?.picked_asset_url ? { id: matchId, tag: "" } : undefined);
    const to = recorded ? pmUpdateFor(recorded, latest[recorded.id]) : (rec && matchTo) || null;
    const listed = inList(p);
    const key = autoloadKey(p);
    const order = autoload ? autoload.list.filter((e) => !e.startsWith("!")) : [];
    const pos = order.indexOf(key);
    return (
      <li
        key={p.path}
        className="flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3.5 py-2.5 text-sm"
      >
        <div className="min-w-[min(100%,14rem)] flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{p.name || p.filename}</span>
            {p.version && (
              <Badge tone="neutral" variant="outline">
                {p.version}
              </Badge>
            )}
            {to && recorded && (
              <Badge tone="warn" variant="outline">
                {tr("pm_update_available", { tag: to.tag }, `${to.tag} available`)}
              </Badge>
            )}
            {to && !recorded && matchId && (
              <Badge tone="neutral" variant="outline">
                {tr("pm_catalogue_match", { name: nameOf(matchId) }, `Catalogue: ${nameOf(matchId)}`)}
              </Badge>
            )}
            {listed && autoload && (
              <Badge tone="good" variant="outline">
                {tr("pm_autoload_badge", { n: pos + 1 }, `boot #${pos + 1}`)}
              </Badge>
            )}
          </div>
          <div className="mt-0.5 truncate font-mono text-xs text-[var(--color-muted)]" title={p.path}>
            {p.path}
          </div>
          {p.source && (
            <div className="text-xs text-[var(--color-muted)]">
              {tr("pm_source", { source: p.source }, `from ${p.source}`)}
            </div>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {autoload && (
            <>
              <Button
                variant={listed ? "secondary" : "ghost"}
                size="sm"
                disabled={busy !== null}
                onClick={() =>
                  void setAutoload(
                    autoload.enabled,
                    listed ? autoload.list.filter((e) => e !== key) : [...autoload.list, key],
                  )
                }
                title={tr(
                  "pm_autoload_toggle_tooltip",
                  undefined,
                  "Payload Manager loads the listed payloads, in order, when the PS5 starts.",
                )}
              >
                {listed
                  ? tr("pm_autoload_remove", undefined, "Don't load at boot")
                  : tr("pm_autoload_add", undefined, "Load at boot")}
              </Button>
              {listed && (
                <>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy !== null || pos <= 0}
                    onClick={() => void setAutoload(autoload.enabled, moveEntry(autoload.list, key, -1))}
                    title={tr("pm_autoload_earlier", undefined, "Load earlier")}
                  >
                    <ArrowUp size={12} />
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy !== null || pos >= order.length - 1}
                    onClick={() => void setAutoload(autoload.enabled, moveEntry(autoload.list, key, 1))}
                    title={tr("pm_autoload_later", undefined, "Load later")}
                  >
                    <ArrowDown size={12} />
                  </Button>
                </>
              )}
            </>
          )}
          {to && rec && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={busy === `update:${p.path}` ? <Spinner size={12} tone="inherit" /> : <Download size={12} />}
              disabled={busy !== null}
              onClick={() => void update(p, rec.id, to)}
              title={
                recorded
                  ? tr(
                      "pm_update_tooltip",
                      undefined,
                      "Downloads the newer release and stores it in Payload Manager in place of this one, keeping its place in the boot list.",
                    )
                  : tr(
                      "pm_replace_tooltip",
                      undefined,
                      "Payload Manager keeps no version for a payload stored by hand, so ps5upload cannot tell whether this one is older. Replacing stores the catalogue's release in its place, keeping its place in the boot list; after that, updates are offered here.",
                    )
              }
            >
              {recorded
                ? tr("pm_update", undefined, "Update")
                : tr("pm_replace_with", { tag: to.tag }, `Replace with ${to.tag}`)}
            </Button>
          )}
          <Button
            variant="primary"
            size="sm"
            leftIcon={busy === `launch:${p.path}` ? <Spinner size={12} tone="inherit" /> : <Play size={12} />}
            disabled={busy !== null}
            onClick={() => void launch(p)}
          >
            {tr("pm_launch", undefined, "Launch")}
          </Button>
          {p.path.startsWith("/data/pldmgr/") && (
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<Trash2 size={12} />}
              disabled={busy !== null}
              onClick={() => void remove(p)}
            >
              {tr("delete", undefined, "Delete")}
            </Button>
          )}
        </div>
      </li>
    );
  };

  return (
    <div className="space-y-5">
      {dialog}
      <div className="flex flex-wrap items-center justify-between gap-3 text-sm">
        <div className="text-[var(--color-muted)]">
          {tr("pm_detected", { version: view.version }, `Payload Manager ${view.version} on the PS5`)}
        </div>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={loading ? <Spinner size={12} tone="inherit" /> : <RefreshCw size={12} />}
          disabled={loading}
          onClick={() => void refresh()}
        >
          {tr("refresh", undefined, "Refresh")}
        </Button>
      </div>

      {autoload ? (
        <section className="surface-panel p-4">
          <Toggle
            checked={autoload.enabled}
            disabled={busy !== null}
            onChange={(on) => void setAutoload(on, autoload.list)}
            label={tr("pm_autoload_title", undefined, "Load payloads at boot")}
            hint={tr(
              "pm_autoload_hint",
              { n: autoload.list.filter((e) => !e.startsWith("!")).length, delay: autoload.delay_s },
              `Payload Manager starts the marked payloads in order ${autoload.delay_s}s after the PS5 boots. ${autoload.list.filter((e) => !e.startsWith("!")).length} listed. Pauses between them are kept as set in Payload Manager.`,
            )}
          />
        </section>
      ) : (
        view.autoloadError && (
          <ErrorCard
            title={tr("pm_autoload_read_failed", undefined, "Couldn't read the autoload list")}
            detail={view.autoloadError}
          />
        )
      )}

      {autoload && (
        <section className="surface-panel space-y-2 p-4">
          <Toggle
            checked={helperListed}
            disabled={busy !== null}
            onChange={(on) => void setHelperAtBoot(on)}
            label={tr("helper_boot_label", undefined, "Load the PS5Upload helper at boot")}
            hint={tr(
              "helper_boot_hint",
              undefined,
              "Payload Manager starts the helper after everything above it in its boot list, so Minion and this app work after a restart with no PC. Each time this app connects it brings that copy up to its own helper and keeps the previous one; if a new copy does not start after a restart, the previous one is put back.",
            )}
          />
          {helperListed && !autoload.enabled && (
            <p className="text-xs text-[var(--color-warn)]">
              {tr(
                "helper_boot_list_off",
                undefined,
                "Payload Manager's boot list is turned off above, so nothing starts at boot until you turn it on.",
              )}
            </p>
          )}
          {helperListed && bootRecord?.badVersion && (
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "helper_boot_bad_version",
                { version: bootRecord.badVersion },
                `Helper ${bootRecord.badVersion} did not start at boot on this PS5, so the previous copy was put back. A newer app version tries again.`,
              )}
            </p>
          )}
        </section>
      )}

      <section className="surface-panel space-y-3 p-4">
        <header className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
          {tr("pm_add_header", undefined, "Add to Payload Manager")}
        </header>
        <div className="flex flex-wrap items-end gap-2">
          <input
            ref={fileInput}
            type="file"
            accept=".elf,.bin"
            className="hidden"
            data-testid="pm-add-file"
            onChange={(e) => {
              const f = e.currentTarget.files?.[0];
              e.currentTarget.value = "";
              if (f) void addLocal(f);
            }}
          />
          <Button
            variant="secondary"
            size="sm"
            leftIcon={busy === "add:local" ? <Spinner size={12} tone="inherit" /> : <Upload size={12} />}
            disabled={busy !== null}
            onClick={() => fileInput.current?.click()}
          >
            {tr("pm_add_local", undefined, "From this computer…")}
          </Button>
          {inventory.length > 0 && (
            <>
              <div className="min-w-48">
                <Select
                  aria-label={tr("pm_add_catalogue_label", undefined, "Downloaded catalogue payload")}
                  value={pick}
                  onChange={(e) => setPick(e.currentTarget.value)}
                  disabled={busy !== null}
                  options={[
                    { value: "", label: tr("pm_add_catalogue_pick", undefined, "Catalogue payload…") },
                    ...inventory.map((e) => ({ value: e.payload_id, label: `${nameOf(e.payload_id)} ${e.version}` })),
                  ]}
                />
              </div>
              <Button
                variant="secondary"
                size="sm"
                leftIcon={busy === "add:catalogue" ? <Spinner size={12} tone="inherit" /> : <Plus size={12} />}
                disabled={busy !== null || !pick}
                onClick={() => void addCatalogue(pick)}
              >
                {tr("payloads_pm_store", undefined, "Store in Payload Manager")}
              </Button>
            </>
          )}
        </div>
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "pm_add_hint",
            undefined,
            "Payload Manager keeps the file on the PS5, launches it from here or at boot, and lists it in its own menu. A payload stored from the catalogue is offered here when the catalogue has a newer release.",
          )}
        </p>
      </section>

      {view.payloadsError && (
        <ErrorCard
          title={tr("pm_list_failed", undefined, "Couldn't list the stored payloads")}
          detail={view.payloadsError}
        />
      )}

      <section>
        <header className="mb-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
          {tr("pm_stored_header", { n: stored.length }, `Stored on the PS5 · ${stored.length}`)}
        </header>
        {stored.length === 0 ? (
          <p className="text-sm text-[var(--color-muted)]">
            {tr(
              "pm_stored_empty",
              undefined,
              "Nothing stored yet. On the Catalog tab, “Store in Payload Manager” keeps a payload on the console.",
            )}
          </p>
        ) : (
          <ul className="grid gap-1.5">{stored.map(row)}</ul>
        )}
      </section>

      {onUsb.length > 0 && (
        <section>
          <header className="mb-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            {tr("pm_usb_header", { n: onUsb.length }, `On USB · ${onUsb.length}`)}
          </header>
          <ul className="grid gap-1.5">{onUsb.map(row)}</ul>
        </section>
      )}
    </div>
  );
}
