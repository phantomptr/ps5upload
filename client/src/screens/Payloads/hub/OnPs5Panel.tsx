import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useNavigate, useSearchParams } from "react-router";
import {
  Boxes,
  ChevronDown,
  ChevronUp,
  Cpu,
  Download,
  ExternalLink,
  RefreshCw,
  Rocket,
  Skull,
} from "lucide-react";

import { Badge, Button, ConnectionGate, EmptyState, ErrorCard, SkeletonRows } from "../../../components";
import { useConfirm } from "../../../components/ConfirmDialog";
import {
  fsListDir,
  payloadsCatalog,
  payloadsDownload,
  payloadsLocalInventory,
  payloadsRelease,
  processKill,
  processList,
  sendPayload,
  type PayloadInfo,
  type ProcessInfo,
} from "../../../api/ps5";
import { mgmtAddr, withPort } from "../../../lib/addr";
import { openExternalUrl } from "../../../lib/openExternalUrl";
import { compareVersions } from "../../../lib/semver";
import { isTauriEnv } from "../../../lib/tauriEnv";
import { usePoll, useTransferBusy } from "../../../lib/usePoll";
import { useDocumentVisible } from "../../../lib/visibility";
import { useConnectionStore } from "../../../state/connection";
import { useTr } from "../../../state/lang";
import { log } from "../../../state/logs";
import { pushNotification } from "../../../state/notifications";
import { withConsolePrefix } from "../../../state/roster";
import { killPrompt } from "../../Processes/killPrompt";
import NanoDnsScreen from "../../NanoDns";
import SmpPanel from "./SmpPanel";
import PayloadManagerPanel from "../PayloadManagerPanel";
import { PmSummary, SmpSummary } from "./summaries";
import { HbSummary, HearBridgePanel, useHearBridge } from "./hearbridge";
import { hbAct } from "../../../api/hearbridge";
import { HushPanel, HushSummary, useHush, useHushActions } from "../../Hush";
import {
  detectHubCards,
  hubEntries,
  markerFolders,
  type HubCard,
  type HubEntry,
  type IntegrationId,
  type IntegrationReport,
} from "./registry";

/** The process list is re-read this often while the tab is open. Slower than Processes'
 *  3 s: cards change state rarely, and this shares the helper with everything else. */
const REFRESH_MS = 10_000;

/** Which marker files exist. A folder is listed once; a folder whose parent listing lacks
 *  it is not asked for at all (most consoles have none of these). */
async function findMarkers(addr: string, entries: readonly HubEntry[]): Promise<Set<string>> {
  const found = new Set<string>();
  const listed = new Map<string, Set<string> | null>();
  const list = async (dir: string): Promise<Set<string> | null> => {
    if (listed.has(dir)) return listed.get(dir) ?? null;
    const slash = dir.lastIndexOf("/");
    if (slash > 0) {
      const parent = dir.slice(0, slash);
      const siblings = await list(parent);
      if (siblings && !siblings.has(dir.slice(slash + 1))) {
        listed.set(dir, null);
        return null;
      }
    }
    let names: Set<string> | null;
    try {
      names = new Set((await fsListDir(addr, dir)).map((e) => e.name));
    } catch {
      names = null;
    }
    listed.set(dir, names);
    return names;
  };
  const folders = [...markerFolders(entries).entries()].sort((a, b) => a[0].length - b[0].length);
  for (const [dir, paths] of folders) {
    const names = await list(dir);
    if (!names) continue;
    for (const p of paths) if (names.has(p.slice(p.lastIndexOf("/") + 1))) found.add(p);
  }
  return found;
}

/**
 * "On your PS5": one card per payload running or installed on the selected console, with
 * its web page, an update when the catalogue has a newer release, Stop, and (for
 * ShadowMount+, nanoDNS, Payload Manager, HearBridge and Hush) its own panel under Manage.
 */
export default function OnPs5Panel() {
  return (
    <ConnectionGate require="payload">
      <OnPs5Body />
    </ConnectionGate>
  );
}

function OnPs5Body() {
  const tr = useTr();
  const native = isTauriEnv();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const online = payloadStatus === "up";
  const addr = mgmtAddr(host);
  const windowVisible = useDocumentVisible();
  const [params, setParams] = useSearchParams();
  const openId = params.get("open");

  const [catalog, setCatalog] = useState<PayloadInfo[] | null>(null);
  const [procs, setProcs] = useState<ProcessInfo[]>([]);
  const [procsLoaded, setProcsLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [markers, setMarkers] = useState<Set<string>>(new Set());
  const [markersLoaded, setMarkersLoaded] = useState(false);
  const [reports, setReports] = useState<Record<string, IntegrationReport>>({});
  const [latest, setLatest] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();
  // Bumped after an Update, so the integrations' summaries read their version again.
  const [summaryRev, setSummaryRev] = useState(0);
  // The catalogue has been read (or failed): markers are checked once, against the full list.
  const [catalogDone, setCatalogDone] = useState(!native);

  useEffect(() => {
    if (!native) return;
    let cancelled = false;
    payloadsCatalog()
      .then((c) => {
        if (!cancelled) setCatalog(c);
      })
      .catch((e) => log.warn("payloads", `catalogue: ${String(e)}`))
      .finally(() => {
        if (!cancelled) setCatalogDone(true);
      });
    return () => {
      cancelled = true;
    };
  }, [native]);

  const entries = useMemo(() => hubEntries(catalog), [catalog]);

  // Another console's cards are not this one's.
  const genRef = useRef(0);
  useEffect(() => {
    genRef.current++;
    setProcs([]);
    setProcsLoaded(false);
    setMarkers(new Set());
    setMarkersLoaded(false);
    setReports({});
    setError(null);
  }, [addr]);

  const inFlight = useRef(false);
  const refreshProcs = useCallback(async () => {
    if (!online || inFlight.current) return;
    inFlight.current = true;
    const gen = genRef.current;
    try {
      const r = await processList(addr);
      if (gen !== genRef.current) return;
      setProcs(r.processes);
      setError(null);
    } catch (e) {
      if (gen !== genRef.current) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      inFlight.current = false;
      if (gen === genRef.current) setProcsLoaded(true);
    }
  }, [addr, online]);
  usePoll(refreshProcs, REFRESH_MS, { host: addr, enabled: online && windowVisible });

  // Marker checks list folders on the console: like the pollers, they stand aside while a
  // transfer to this console runs, and catch up when it ends.
  const transferring = useTransferBusy(addr);
  const refreshMarkers = useCallback(async () => {
    if (!online || !catalogDone || transferring) return;
    const gen = genRef.current;
    const found = await findMarkers(addr, entries);
    if (gen !== genRef.current) return;
    setMarkers(found);
    setMarkersLoaded(true);
  }, [addr, entries, online, catalogDone, transferring]);
  useEffect(() => {
    void refreshMarkers();
  }, [refreshMarkers]);

  // HearBridge's own API is read while its card is on screen (or a link opened it); an
  // answer also counts as running, should its process go by another name.
  const hbWanted =
    openId === "hearbridge" ||
    detectHubCards({ entries, processes: procs, markers }).some((c) => c.integration === "hearbridge");
  // Every status read lets HearBridge re-query the headset volume, so read slowly unless
  // its Manage panel is open.
  const hb = useHearBridge(host, online && hbWanted, openId === "hearbridge");
  const hbPresent = hb.view?.present === true;
  // Hush keeps its pid in /data/hush/pid.lock (elfldr lists it as payload.elf): its files are
  // read while its card is on screen.
  const hushWanted =
    openId === "hush" ||
    detectHubCards({ entries, processes: procs, markers }).some((c) => c.integration === "hush");
  const hush = useHush(host, online && hushWanted);
  const hushPid = hush.view?.lockPid ?? null;
  const hushActions = useHushActions(host, hush.refresh);
  const cards = useMemo(
    () =>
      detectHubCards({
        entries,
        processes: procs,
        markers,
        answering: hbPresent ? new Set(["hearbridge"]) : undefined,
        pids: hushPid ? new Map([["hush", [hushPid]]]) : undefined,
        forceShow: openId,
      }),
    [entries, procs, markers, openId, hbPresent, hushPid],
  );
  const hbVersion = hb.view?.present ? hb.view.status.version || null : null;
  const hbPort = hb.view?.present ? hb.view.port : null;

  const report = useCallback((id: string, r: IntegrationReport) => {
    setReports((prev) => {
      const cur = prev[id];
      if (cur && cur.version === r.version && cur.webPort === r.webPort) return prev;
      return { ...prev, [id]: r };
    });
  }, []);
  useEffect(() => {
    if (hbVersion || hbPort) report("hearbridge", { version: hbVersion, webPort: hbPort });
  }, [hbVersion, hbPort, report]);

  // The newest stable release of each card whose running version is known, so the card
  // can offer an update. Desktop only (the catalogue's GitHub lookups live there).
  const versionKey = cards
    .map((c) => `${c.id}=${reports[c.id]?.version ?? ""}`)
    .join("|");
  useEffect(() => {
    if (!native) return;
    let cancelled = false;
    for (const c of cards) {
      const v = reports[c.id]?.version;
      if (!v || c.entryIds.length !== 1 || latest[c.id] !== undefined) continue;
      payloadsRelease(c.id)
        .then((r) => {
          if (!cancelled && r.picked_asset_url) setLatest((prev) => ({ ...prev, [c.id]: r.tag }));
        })
        .catch(() => undefined);
    }
    return () => {
      cancelled = true;
    };
    // versionKey stands for cards + reports here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [native, versionKey]);

  // A link that opened a card (an old tab's URL, a notification) scrolls to it once.
  const scrolledTo = useRef<string | null>(null);
  const openShown = !!openId && cards.some((c) => c.id === openId);
  useEffect(() => {
    if (!openId || !openShown || scrolledTo.current === openId) return;
    scrolledTo.current = openId;
    document
      .querySelector(`[data-card="${CSS.escape(openId)}"]`)
      ?.scrollIntoView({ block: "start", behavior: "smooth" });
  }, [openId, openShown]);

  const setOpen = (id: string | null) => {
    const next = new URLSearchParams(params);
    if (id) next.set("open", id);
    else next.delete("open");
    setParams(next, { replace: true });
  };

  const stop = async (card: HubCard) => {
    if (card.integration === "hearbridge" && hbPresent) return stopHearBridge();
    const p = card.processes[0];
    if (!p) return;
    const kind = killPrompt(p);
    if (!kind) return;
    const ok = await confirm({
      title: tr("processes_kill_payload_title", { name: card.name }, `Stop payload "${card.name}"?`),
      message: tr(
        "processes_kill_payload_body",
        { name: card.name },
        `This kills the payload "${card.name}" (kstuff, etaHEN and the like). Whatever it provides stops until you send it again, and games that need it may stop starting.`,
      ),
      confirmLabel: tr("processes_kill", undefined, "Kill"),
      destructive: true,
    });
    if (!ok) return;
    setBusy(card.id);
    try {
      for (const proc of card.processes) {
        const ack = await processKill(addr, proc.pid);
        if (!ack.ok) throw new Error(ack.err || `pid ${proc.pid}`);
      }
      log.info("payloads", `stopped ${card.name}`);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
      await refreshProcs();
    }
  };

  /** HearBridge has a clean stop of its own (it closes the headset link first). */
  const stopHearBridge = async () => {
    const ok = await confirm({
      title: tr("hb_stop_title", undefined, "Stop HearBridge?"),
      message: tr(
        "hb_stop_body",
        undefined,
        "HearBridge closes and the headset disconnects. Send it again to start it.",
      ),
      confirmLabel: tr("hub_stop", undefined, "Stop"),
      destructive: true,
    });
    if (!ok) return;
    setBusy("hearbridge");
    try {
      await hbAct(host, { action: "stop" });
      log.info("payloads", "stopped HearBridge");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
      await hb.refresh();
      await refreshProcs();
    }
  };

  const update = async (card: HubCard, tag: string) => {
    const ok = await confirm({
      title: tr("hub_update_confirm_title", { name: card.name, version: tag }, `Update ${card.name} to ${tag}?`),
      message: tr(
        "hub_update_confirm_body",
        { version: tag },
        `Downloads ${tag} and sends it to the PS5. Check the PS5 for its notification.`,
      ),
      confirmLabel: tr("hub_update", { version: tag }, `Update to ${tag}`),
    });
    if (!ok) return;
    setBusy(card.id);
    try {
      const rel = await payloadsRelease(card.id);
      const inv = await payloadsLocalInventory();
      let local = inv.find((e) => e.payload_id === card.id && e.version === rel.tag);
      if (!local) local = await payloadsDownload(card.id, rel.picked_asset_url, rel.tag);
      await sendPayload(host.trim(), local.path);
      pushNotification(
        "success",
        withConsolePrefix(host, tr("hub_updated", { name: card.name, version: rel.tag }, `Sent ${card.name} ${rel.tag}`)),
      );
      setReports((prev) => ({ ...prev, [card.id]: { ...prev[card.id], version: null } }));
      setSummaryRev((n) => n + 1);
      if (card.integration === "hearbridge") void hb.refresh();
    } catch (e) {
      pushNotification(
        "error",
        withConsolePrefix(host, tr("hub_update_failed", { name: card.name }, `Couldn't update ${card.name}`)),
        { body: String(e) },
      );
    } finally {
      setBusy(null);
    }
  };

  const loading = !procsLoaded || !markersLoaded;

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm text-[var(--color-muted)]">
          {loading
            ? tr("hub_checking", undefined, "Checking what is on this PS5…")
            : tr("hub_count", { n: cards.length }, `${cards.length} on this PS5`)}
        </span>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<RefreshCw size={14} />}
          onClick={() => {
            void refreshProcs();
            void refreshMarkers();
          }}
        >
          {tr("refresh", undefined, "Refresh")}
        </Button>
      </div>

      {error && (
        <ErrorCard
          title={tr("hub_error", undefined, "Couldn't read what is running on the PS5")}
          detail={error}
          onDismiss={() => setError(null)}
        />
      )}

      {loading && cards.length === 0 ? (
        <SkeletonRows rows={3} />
      ) : cards.length === 0 ? (
        <EmptyState
          icon={Boxes}
          title={tr("hub_empty_title", undefined, "Nothing found on this PS5")}
          message={
            native
              ? tr(
                  "hub_empty_body",
                  undefined,
                  "No payload from the catalogue is running or installed on this console. Catalog has them, and Send loads any payload file.",
                )
              : tr("hub_empty_body_web", undefined, "No known payload is running or installed on this console.")
          }
          action={
            native ? (
              <div className="flex flex-wrap justify-center gap-2">
                <Button size="sm" leftIcon={<Boxes size={14} />} onClick={() => navigate("/payloads?tab=catalog")}>
                  {tr("payloads_tab_catalog", undefined, "Catalog")}
                </Button>
                <Button size="sm" leftIcon={<Rocket size={14} />} onClick={() => navigate("/payloads?tab=send")}>
                  {tr("payloads_tab_send", undefined, "Send payload")}
                </Button>
              </div>
            ) : undefined
          }
        />
      ) : (
        <ul className="flex flex-col gap-2.5">
          {cards.map((c) => {
            const rep = reports[c.id];
            const tag = latest[c.id];
            const newer =
              native && rep?.version && tag && compareVersions(rep.version, tag) === -1 ? tag : null;
            return (
              <HubCardView
                key={c.id}
                card={c}
                host={host}
                version={rep?.version ?? null}
                webPort={rep?.webPort ?? c.webPort}
                newer={newer}
                catalogued={catalog !== null}
                busy={busy === c.id}
                expanded={openId === c.id}
                onToggle={() => setOpen(openId === c.id ? null : c.id)}
                onStop={() => void stop(c)}
                onUpdate={(t) => void update(c, t)}
                canStop={c.integration === "hearbridge" && hbPresent ? true : undefined}
                summary={
                  c.integration === "hearbridge"
                    ? () => <HbSummary view={hb.view} />
                    : c.integration === "hush"
                      ? () => (
                          <HushSummary
                            view={hush.view}
                            running={c.state === "running"}
                            busy={hushActions.busy}
                            onResume={() => void hushActions.run(true, hush.view?.configText ?? null)}
                          />
                        )
                      : c.integration
                      ? (fallback) => (
                          <Summary
                            key={summaryRev}
                            id={c.integration!}
                            host={host}
                            onReport={report}
                            fallback={fallback}
                          />
                        )
                      : null
                }
                panel={
                  c.integration === "hearbridge" ? (
                    <HearBridgePanel host={host} view={hb.view} apply={hb.apply} refresh={hb.refresh} />
                  ) : c.integration === "hush" ? (
                    <HushPanel host={host} view={hush.view} procs={procs} refresh={hush.refresh} />
                  ) : c.integration ? (
                    <IntegrationPanel id={c.integration} host={host} />
                  ) : null
                }
              />
            );
          })}
        </ul>
      )}
      {dialog}
    </div>
  );
}

/** The one-line live summary an integration shows on its card. */
function Summary({
  id,
  host,
  onReport,
  fallback,
}: {
  id: IntegrationId;
  host: string;
  onReport: (id: string, r: IntegrationReport) => void;
  fallback: ReactNode;
}) {
  if (id === "payload-manager") {
    return <PmSummary host={host} onReport={(r) => onReport(id, r)} fallback={fallback} />;
  }
  if (id === "shadowmountplus") return <SmpSummary host={host} fallback={fallback} />;
  return <>{fallback}</>;
}

/** The integration's own panel, as its tab showed it before. */
function IntegrationPanel({ id, host }: { id: IntegrationId; host: string }) {
  if (id === "payload-manager") return <PayloadManagerPanel />;
  if (id === "shadowmountplus") return <SmpPanel mgmtAddr={host.trim() ? mgmtAddr(host.trim()) : null} />;
  if (id === "nanodns") return <NanoDnsScreen />;
  return null;
}

function HubCardView({
  card,
  host,
  version,
  webPort,
  newer,
  catalogued,
  canStop: canStopOverride,
  busy,
  expanded,
  onToggle,
  onStop,
  onUpdate,
  summary,
  panel,
}: {
  card: HubCard;
  host: string;
  version: string | null;
  webPort: number | null;
  newer: string | null;
  /** The catalogue was read, so "not in the catalogue" can be said (not in the web UI). */
  catalogued: boolean;
  /** Overrides the process-based Stop (an integration with its own clean stop). */
  canStop?: boolean;
  busy: boolean;
  expanded: boolean;
  onToggle: () => void;
  onStop: () => void;
  onUpdate: (tag: string) => void;
  summary: ((fallback: ReactNode) => ReactNode) | null;
  panel: ReactNode;
}) {
  const tr = useTr();
  const running = card.state === "running";
  const link = running && webPort ? `http://${withPort(host.trim(), webPort)}/` : null;
  const proc = card.processes[0];
  const canStop = canStopOverride ?? (running && !!proc && killPrompt(proc) !== null);
  const stateBadge =
    card.state === "running" ? (
      <Badge tone="good" dot>
        {tr("hub_state_running", undefined, "Running")}
      </Badge>
    ) : card.state === "installed" ? (
      <Badge tone="neutral">{tr("hub_state_installed", undefined, "Installed")}</Badge>
    ) : (
      <Badge tone="warn">{tr("hub_state_unknown", undefined, "Not found")}</Badge>
    );
  const detail =
    card.state === "running" && proc
      ? tr(
          "hub_running_detail",
          { name: proc.comm || proc.name, pid: proc.pid, mb: proc.memory_mib.toFixed(1) },
          `${proc.comm || proc.name} · pid ${proc.pid} · ${proc.memory_mib.toFixed(1)} MB`,
        )
      : card.state === "installed"
        ? tr("hub_installed_detail", undefined, "On the console, not running.")
        : card.state === "unknown"
          ? tr("hub_unknown_detail", undefined, "Not found on this PS5.")
          : null;

  return (
    <li
      className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)]"
      data-card={card.id}
    >
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2 px-3 py-2.5 sm:px-4">
        <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-2xl bg-[var(--color-surface-3)]">
          <Cpu size={17} className="text-[var(--color-muted)]" />
        </div>
        <div className="min-w-[min(100%,12rem)] flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <span className="truncate font-medium">{card.name}</span>
            {stateBadge}
            {version && (
              <Badge tone="neutral" variant="outline">
                {version.startsWith("v") ? version : `v${version}`}
              </Badge>
            )}
            {catalogued && card.id.startsWith("proc:") && (
              <Badge tone="neutral" variant="outline">
                {tr("hub_not_catalogued", undefined, "Not in the catalogue")}
              </Badge>
            )}
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">{summary ? summary(detail) : detail}</div>
        </div>
        <div className="ml-auto flex flex-wrap items-center gap-1.5">
          {link && (
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<ExternalLink size={14} />}
              onClick={() => void openExternalUrl(link)}
              title={link}
            >
              {tr("hub_open_page", undefined, "Open web page")}
            </Button>
          )}
          {newer && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Download size={14} />}
              disabled={busy}
              onClick={() => onUpdate(newer)}
            >
              {tr("hub_update", { version: newer }, `Update to ${newer}`)}
            </Button>
          )}
          {canStop && (
            <Button variant="ghost" size="sm" leftIcon={<Skull size={14} />} disabled={busy} onClick={onStop}>
              {tr("hub_stop", undefined, "Stop")}
            </Button>
          )}
          {panel && (
            <Button
              variant={expanded ? "primary" : "secondary"}
              size="sm"
              rightIcon={expanded ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
              aria-expanded={expanded}
              onClick={onToggle}
            >
              {tr("hub_manage", undefined, "Manage")}
            </Button>
          )}
        </div>
      </div>
      {expanded && panel && (
        <div className="border-t border-[var(--glass-edge)] px-3 py-3 sm:px-4">{panel}</div>
      )}
    </li>
  );
}
