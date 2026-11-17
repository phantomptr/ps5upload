import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { ArrowDown, ArrowUp, ExternalLink, Play, Plus, RefreshCw, Skull, Trash2, Upload } from "lucide-react";

import { Badge, Button, Callout, ErrorCard, Select, Spinner, Toggle } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import {
  autoloadKey,
  autoloadKeys,
  listedEntry,
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
  payloadsLocalInventory,
  payloadsReleases,
  type PayloadInfo,
  type PayloadLocalEntry,
  type PayloadReleaseInfo,
} from "../../api/ps5";
import { hostOf } from "../../lib/addr";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { defaultRelease } from "../../lib/payloadVersions";
import {
  catalogueMatch,
  forgetPmStore,
  isRepoManaged,
  pmRecords,
  pmUpdateFor,
  recordPmStore,
} from "../../lib/pmUpdates";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useConnectionStore } from "../../state/connection";
import { useTr, type Translator } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { humanizeCaught } from "../../lib/humanizeError";
import { listsHelper, loadBootRecord, storedHelper } from "../../lib/helperAtBoot";
import { turnOffHelperAtBoot, turnOnHelperAtBoot } from "../../state/helperAtBootRuntime";
import { usePmView, usePmViewStore } from "./hub/pmViewStore";

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

/** A saved payload that is running right now, as "On your PS5" matched it to its process. */
export interface PmRunning {
  pid: number;
  memoryMib: number;
  /** PS5Upload's own helper (it can't be stopped from here). */
  ours: boolean;
  /** Stop offered (the card's flow: confirm, then kill). */
  canStop: boolean;
  busy: boolean;
  onStop: () => void;
  /** What the console can't tell apart (kstuff's builds), as the card listed them. */
  variants: string[];
  /** Its web page, when it serves one. */
  webUrl: string | null;
  /** Anything else the card showed under its line (Kraken Mount's update note). */
  note?: ReactNode;
}

/** Boot-list entries that name no payload Payload Manager lists (a file deleted since, or one
 *  on a USB stick that isn't plugged in), with their place in the boot order (1-based, pauses
 *  not counted). Without this they would be invisible, and still take a boot number. */
export function unmatchedBootEntries(
  list: readonly string[],
  payloads: ReadonlyArray<Pick<PmStoredPayload, "slot" | "filename" | "path">>,
): Array<{ entry: string; n: number }> {
  const known = new Set(payloads.flatMap((p) => autoloadKeys(p)));
  return list
    .filter((e) => !e.startsWith("!"))
    .map((entry, i) => ({ entry, n: i + 1 }))
    .filter(({ entry }) => !known.has(entry));
}

/** A running saved row's origin line. `bootN`: its place in the boot list when that list is
 *  on (a list turned off starts nothing at boot, so no number is promised). */
export function runningOrigin(tr: Translator, ours: boolean, bootN: number | null): string {
  if (ours) return tr("hub_origin_ours", undefined, "PS5Upload's own helper");
  if (bootN !== null) {
    return tr("pm_origin_list_boot", { n: bootN }, `From your Payload Manager list, starts at boot #${bootN}`);
  }
  return tr("pm_origin_list", undefined, "From your Payload Manager list");
}

/** The payloads stored in the console's Payload Manager (:8084): launch, delete, and the
 *  boot autoload list. Storing into it is on the Catalog tab's cards. `running` (by stored
 *  path) marks the ones running now, with their pid and Stop. `onChanged` runs after a
 *  change here (launch, delete, store, boot list), so what runs is read again. */
export default function PayloadManagerPanel({
  running,
  onChanged,
}: { running?: Readonly<Record<string, PmRunning>>; onChanged?: () => void } = {}) {
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
  /** Ids whose newest release was asked for on this console. */
  const asked = useRef(new Set<string>());

  // Another console's answer is not this one's: a slow reply from the console picked before
  // must not fill this one's list.
  const genRef = useRef(0);
  const refresh = useCallback(async () => {
    const h = host.trim();
    if (!h) return;
    const gen = genRef.current;
    setLoading(true);
    setError(null);
    try {
      const v = await pmOverview(h);
      if (gen === genRef.current) {
        setView(v);
        usePmViewStore.getState().put(h, v);
      }
    } catch (e) {
      if (gen === genRef.current) setError(humanizeCaught(e));
    } finally {
      if (gen === genRef.current) setLoading(false);
    }
  }, [host]);

  useEffect(() => {
    genRef.current++;
    setView(null);
    setLatest({});
    asked.current.clear();
    void refresh();
  }, [refresh]);

  // "On your PS5" reads the same overview on each refresh: a newer one replaces this.
  const shared = usePmView(host);
  useEffect(() => {
    if (shared) setView((cur) => (cur === shared ? cur : shared));
  }, [shared]);

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
    // Only what this app stored has a known version to compare. Each id is asked once per
    // console: the answers it fills are not what it waits on.
    const ids = new Set(Object.values(pmRecords(hostOf(host))).map((r) => r.id));
    const gen = genRef.current;
    for (const id of ids) {
      if (asked.current.has(id)) continue;
      asked.current.add(id);
      payloadsReleases(id)
        .then((list) => gen === genRef.current && setLatest((m) => ({ ...m, [id]: defaultRelease(list) })))
        .catch(() => gen === genRef.current && setLatest((m) => ({ ...m, [id]: null })));
    }
  }, [view, host]);

  const run = async (key: string, work: () => Promise<void>, failTitle: string) => {
    setBusy(key);
    try {
      await work();
      await refresh();
      onChanged?.();
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
        "pm_delete_confirm_body_v2",
        undefined,
        "The stored file and its details are removed from the console. Files on this computer aren't touched.",
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

  /** Stores under `filename`, asking first when Payload Manager already has that folder
   *  (storing into it removes the folder's other files). False when the user said no. */
  const storeConfirmed = async (
    filename: string,
    store: (confirmed: boolean) => Promise<PmStoreResult>,
    quiet = false,
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
    if (!quiet)
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
      async () => {
        await pmSetAutoload(host, enabled, list);
      },
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
        const done = await turnOnHelperAtBoot(
          host,
          (folder) =>
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
          () =>
            confirm({
              title: tr("helper_boot_list_off_title", undefined, "Payload Manager's boot list is off"),
              message: tr(
                "helper_boot_list_off_confirm",
                undefined,
                "While its boot list is off, Payload Manager starts nothing at boot, the helper included. Turn it on?",
              ),
              confirmLabel: tr("helper_boot_turn_on_list", undefined, "Turn on the boot list"),
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
          "pm_absent_body_v2",
          undefined,
          "Payload Manager (itsPLK) keeps payloads on the console and can load them at boot. Get it from its project page (on the Catalog tab) and send it with Send payload, or start it on the PS5, then come back here.",
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
  const helperListed = listsHelper(autoload, helperCopy);
  const bootRecord = loadBootRecord(host);
  // On here and stored, but not in the list right now: the next connection puts it back.
  const helperOn = helperListed || (!!bootRecord && !!helperCopy);
  const inList = (p: PmStoredPayload) => listedEntry(autoload?.list, p) !== null;
  const stored = view.payloads.filter((p) => p.path.startsWith("/data/pldmgr/"));
  const onUsb = view.payloads.filter((p) => !p.path.startsWith("/data/pldmgr/"));
  // Only against a list read in full: otherwise every entry would look like it names nothing.
  const unmatched = autoload && !view.payloadsError ? unmatchedBootEntries(autoload.list, view.payloads) : [];

  const records = pmRecords(hostOf(host));
  const nameOf = (id: string) => catalog.find((c) => c.id === id)?.display_name ?? id;
  /** What the tab says about a stored copy: a newer release of the catalogue payload this app
   *  stored there (linked to its page: the app doesn't download payloads), or for one stored by
   *  hand, which catalogue payload it is (Payload Manager keeps no version for it). */
  const offerFor = (p: PmStoredPayload) => {
    const recorded = isRepoManaged(p) ? undefined : records[p.filename];
    const matchId = recorded ? null : catalogueMatch(p, inventory, catalog);
    const to = recorded ? pmUpdateFor(recorded, latest[recorded.id]) : null;
    return { recorded, matchId, to };
  };
  const row = (p: PmStoredPayload) => {
    const { recorded, matchId, to } = offerFor(p);
    const listed = inList(p);
    // The entry that names it now (slot, file name or path), else the one to add.
    const key = listedEntry(autoload?.list, p) ?? autoloadKey(p);
    const keys = autoloadKeys(p);
    const order = autoload ? autoload.list.filter((e) => !e.startsWith("!")) : [];
    const pos = order.indexOf(key);
    // The helper's own entry goes through "Load the PS5Upload helper at boot", so taking it
    // out here is the switch turned off, not something the next connection undoes.
    const isHelper = !!helperCopy && p.path === helperCopy.path;
    const live = running?.[p.path];
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
            {!recorded && matchId && (
              <Badge tone="neutral" variant="outline">
                {tr("pm_catalogue_match", { name: nameOf(matchId) }, `Catalogue: ${nameOf(matchId)}`)}
              </Badge>
            )}
            {listed && autoload && (
              <Badge tone="good" variant="outline">
                {tr("pm_autoload_badge", { n: pos + 1 }, `boot #${pos + 1}`)}
              </Badge>
            )}
            {live && (
              <Badge tone="good" dot>
                {tr("pm_running_badge", { pid: live.pid }, `Running · PID ${live.pid}`)}
              </Badge>
            )}
          </div>
          {live && (
            <div className="mt-0.5 text-xs text-[var(--color-muted)]" data-testid="pm-running-origin">
              {runningOrigin(tr, live.ours, listed && autoload?.enabled ? pos + 1 : null)}
              {` · ${live.memoryMib.toFixed(1)} MB`}
            </div>
          )}
          {live && live.variants.length > 1 && (
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "hub_variants",
                { names: live.variants.join(", ") },
                `One of: ${live.variants.join(", ")} — the console can't tell which.`,
              )}
            </div>
          )}
          {live?.note}
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
          {live?.webUrl && (
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<ExternalLink size={12} />}
              onClick={() => void openExternalUrl(live.webUrl!)}
              title={live.webUrl}
            >
              {tr("hub_open_page", undefined, "Open web page")}
            </Button>
          )}
          {autoload && (
            <>
              <Button
                variant={listed ? "secondary" : "ghost"}
                size="sm"
                disabled={busy !== null}
                onClick={() =>
                  void (isHelper
                    ? setHelperAtBoot(!listed)
                    : setAutoload(
                        autoload.enabled,
                        listed ? autoload.list.filter((e) => !keys.includes(e)) : [...autoload.list, key],
                      ))
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
          {to && recorded && to.html_url && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<ExternalLink size={12} />}
              onClick={() => void openExternalUrl(to.html_url)}
              title={tr(
                "pm_update_page_tooltip",
                undefined,
                "ps5upload doesn't download payloads. Get the newer file from its project page, then store it here.",
              )}
            >
              {tr("pu_out_open_page", { version: to.tag }, `${to.tag} is out — open project page`)}
            </Button>
          )}
          {/* Running already: launching starts a second copy, so it is not the row's main action. */}
          <Button
            variant={live ? "ghost" : "primary"}
            size="sm"
            leftIcon={busy === `launch:${p.path}` ? <Spinner size={12} tone="inherit" /> : <Play size={12} />}
            disabled={busy !== null || !!live?.busy}
            onClick={() => void launch(p)}
          >
            {tr("pm_launch", undefined, "Launch")}
          </Button>
          {live?.canStop && (
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<Skull size={12} />}
              disabled={live.busy}
              onClick={live.onStop}
            >
              {tr("hub_stop", undefined, "Stop")}
            </Button>
          )}
          {p.path.startsWith("/data/pldmgr/") && (
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<Trash2 size={12} />}
              disabled={busy !== null || !!live?.busy}
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
            checked={helperOn}
            disabled={busy !== null}
            onChange={(on) => void setHelperAtBoot(on)}
            label={tr("helper_boot_label", undefined, "Load the PS5Upload helper at boot")}
            hint={tr(
              "helper_boot_hint",
              undefined,
              "Payload Manager starts the helper after everything above it in its boot list, so Minion and this app work after a restart with no PC. Each time this app connects it brings that copy up to its own helper and keeps the previous one; if a new copy does not start after a restart, the previous one is put back.",
            )}
          />
          {helperOn && !helperListed && (
            <div className="flex flex-wrap items-center gap-2">
              <p className="text-xs text-[var(--color-warn)]">
                {tr(
                  "helper_boot_not_listed",
                  undefined,
                  "The helper is stored but not in Payload Manager's boot list. This app puts it back the next time it connects.",
                )}
              </p>
              <Button
                variant="secondary"
                size="sm"
                disabled={busy !== null}
                onClick={() => void setHelperAtBoot(true)}
              >
                {tr("pm_autoload_add", undefined, "Load at boot")}
              </Button>
            </div>
          )}
          {helperOn && !autoload.enabled && (
            <div className="flex flex-wrap items-center gap-2">
              <p className="text-xs text-[var(--color-warn)]">
                {tr(
                  "helper_boot_list_off",
                  undefined,
                  "Payload Manager's boot list is turned off above, so nothing starts at boot until you turn it on.",
                )}
              </p>
              <Button
                variant="secondary"
                size="sm"
                disabled={busy !== null}
                onClick={() => void setAutoload(true, autoload.list)}
              >
                {tr("helper_boot_turn_on_list", undefined, "Turn on the boot list")}
              </Button>
            </div>
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
                  aria-label={tr("pm_add_saved_label", undefined, "Saved catalogue payload")}
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
        <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
          <header className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            {tr("pm_stored_header", { n: stored.length }, `Stored on the PS5 · ${stored.length}`)}
          </header>
        </div>
        {stored.length === 0 ? (
          <p className="text-sm text-[var(--color-muted)]">
            {tr(
              "pm_stored_empty_v2",
              undefined,
              "Nothing stored yet. “From this computer…” above keeps a payload file on the console.",
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

      {autoload && unmatched.length > 0 && (
        <section data-testid="pm-boot-unmatched">
          <header className="mb-1 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            {tr("pm_boot_unmatched_header", { n: unmatched.length }, `In the boot list, not found · ${unmatched.length}`)}
          </header>
          <p className="mb-2 text-xs text-[var(--color-muted)]">
            {tr(
              "pm_boot_unmatched_hint",
              undefined,
              "These boot-list entries name no payload listed above: a file deleted since, or one on a USB stick that isn't plugged in.",
            )}
          </p>
          <ul className="grid gap-1.5">
            {unmatched.map(({ entry, n }) => (
              <li
                key={`${n}:${entry}`}
                className="flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-dashed border-[var(--glass-edge)] px-3.5 py-2 text-sm"
              >
                <span className="min-w-0 flex-1 break-all font-mono text-xs">{entry}</span>
                <Badge tone="neutral" variant="outline">
                  {tr("pm_autoload_badge", { n }, `boot #${n}`)}
                </Badge>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={busy !== null}
                  onClick={() =>
                    void setAutoload(
                      autoload.enabled,
                      autoload.list.filter((e) => e !== entry),
                    )
                  }
                >
                  {tr("pm_autoload_remove", undefined, "Don't load at boot")}
                </Button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}
