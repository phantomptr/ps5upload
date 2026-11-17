import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useNavigate, useSearchParams } from "react-router";
import {
  Boxes,
  ChevronDown,
  ChevronUp,
  Cpu,
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
  payloadsRelease,
  processKill,
  processList,
  smpStatus,
  type PayloadInfo,
  type ProcessInfo,
} from "../../../api/ps5";
import { mgmtAddr, withPort } from "../../../lib/addr";
import { openExternalUrl } from "../../../lib/openExternalUrl";
import { useMounterFor, useMounterStore } from "../../../lib/mounter";
import { KrakenUpdateNote } from "../../InstallPackage/KrakenUpdateNote";
import { compareVersions } from "../../../lib/semver";
import { usePoll, useTransferBusy } from "../../../lib/usePoll";
import { useDocumentVisible } from "../../../lib/visibility";
import { useConnectionStore } from "../../../state/connection";
import { useTr } from "../../../state/lang";
import { log } from "../../../state/logs";
import { killPrompt } from "../../Processes/killPrompt";
import NanoDnsScreen from "../../NanoDns";
import SmpPanel from "./SmpPanel";
import PayloadManagerPanel, { type PmRunning } from "../PayloadManagerPanel";
import { PmSummary, SmpSummary } from "./summaries";
import { pmOverview, type PmOverview } from "../../../api/payloadManager";
import { storedHelper } from "../../../lib/helperAtBoot";
import { ourElfldrPid } from "../../../lib/elfldrGuard";
import { RUNNING_LINK } from "../payloadTabs";
import { groupCards, matchSavedToCards, originOf, type Origin } from "./pmMerge";
import { usePmView, usePmViewStore } from "./pmViewStore";
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
import { humanizeCaught } from "../../../lib/humanizeError";

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
 * its web page, a link to the newer release when the catalogue has one (the app doesn't
 * download payloads), Stop, and (for
 * ShadowMount+, nanoDNS, Payload Manager and Hush) its own panel under Manage.
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
  // The newest release of each card: its tag and its page.
  const [latest, setLatest] = useState<Record<string, { tag: string; url: string }>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();
  // Keys the integrations' summaries, so a remount reads their version again.
  const [summaryRev] = useState(0);
  // The catalogue has been read (or failed): markers are checked once, against the full list.
  const [catalogDone, setCatalogDone] = useState(false);

  useEffect(() => {
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
  }, []);

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

  // Payload Manager's saved list, read with each refresh while its card is on screen (open or
  // not), so a running payload it keeps shows on its row instead of as a second card. Read
  // apart from the process list: a slow answer from :8084 doesn't hold that up.
  const pmWanted = useRef(false);
  const pmInFlight = useRef(false);
  const refreshPm = useCallback(async () => {
    if (!online || pmInFlight.current) return;
    pmInFlight.current = true;
    try {
      usePmViewStore.getState().put(addr, await pmOverview(addr));
    } catch {
      // Unread: nothing is merged until it reads again.
      usePmViewStore.getState().put(addr, null);
    } finally {
      pmInFlight.current = false;
    }
  }, [addr, online]);

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
      if (pmWanted.current) void refreshPm();
      // The mounter snapshot, once per refresh: whether Kraken Mount answers (it may run as
      // payload.elf) and ShadowMount+'s mounted images, for the cards' lines.
      try {
        const m = await smpStatus(addr);
        if (gen === genRef.current) useMounterStore.getState().put(addr, m);
      } catch {
        /* the cards go by the process list alone */
      }
    } catch (e) {
      if (gen !== genRef.current) return;
      setError(humanizeCaught(e));
    } finally {
      inFlight.current = false;
      if (gen === genRef.current) setProcsLoaded(true);
    }
  }, [addr, online, refreshPm]);
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

  // Hush keeps its pid in /data/hush/pid.lock (elfldr lists it as payload.elf): its files are
  // read while its card is on screen.
  const hushWanted =
    openId === "hush" ||
    detectHubCards({ entries, processes: procs, markers }).some((c) => c.integration === "hush");
  const hush = useHush(host, online && hushWanted);
  const hushPid = hush.view?.lockPid ?? null;
  const hushActions = useHushActions(host, hush.refresh);
  // Kraken Mount's state comes from the mounter snapshot; Payload Manager's from its own API
  // answering (its summary reports what it read).
  const mounter = useMounterFor(addr);
  const krakenUp = !!(mounter?.kraken?.api || mounter?.kraken?.running);
  const krakenInstalled = !!mounter?.kraken?.installed;
  const pmView = usePmView(addr);
  const pmAnswered = reports["payload-manager"] !== undefined || !!pmView?.present;
  const cards = useMemo(() => {
    const answering = new Set<string>();
    if (krakenUp) answering.add("kraken-mount");
    if (pmAnswered) answering.add("payload-manager");
    let seen = markers;
    const kraken = entries.find((e) => e.id === "kraken-mount")?.marker;
    if (krakenInstalled && kraken && !markers.has(kraken)) seen = new Set([...markers, kraken]);
    return detectHubCards({
      entries,
      processes: procs,
      markers: seen,
      answering,
      pids: hushPid ? new Map([["hush", [hushPid]]]) : undefined,
      forceShow: openId,
    });
  }, [entries, procs, markers, openId, hushPid, krakenUp, krakenInstalled, pmAnswered]);

  const pmCardShown = cards.some((c) => c.id === "payload-manager");
  useEffect(() => {
    pmWanted.current = pmCardShown;
    if (pmCardShown) void refreshPm();
  }, [pmCardShown, refreshPm]);

  // The saved list against the running cards. Only with a list read in full and the process
  // list in: otherwise every card shows as it always has.
  const pmList = pmView?.present && !pmView.payloadsError && procsLoaded ? pmView.payloads : null;
  const matched = useMemo(
    () => (pmList ? matchSavedToCards(pmList, cards, entries) : null),
    [pmList, cards, entries],
  );
  const groups = useMemo(() => (matched ? groupCards(cards, matched) : null), [cards, matched]);
  const matchedIds = useMemo(
    () => new Set([...(matched?.values() ?? [])].map((c) => c.id)),
    [matched],
  );
  const loaderPid = ourElfldrPid(addr);
  const originText = (c: HubCard): string | null => {
    const o: Origin | null = originOf(c, matchedIds.has(c.id), loaderPid);
    switch (o) {
      case "pm":
        return tr("pm_origin_list", undefined, "From your Payload Manager list");
      case "ours":
        return tr("hub_origin_ours", undefined, "PS5Upload's own helper");
      case "loader-ours":
        return tr(
          "hub_origin_loader_ours",
          undefined,
          "Payload loader. PS5Upload swapped in its patched build so a dropped send can't jam it.",
        );
      case "loader":
        return tr("hub_origin_loader", undefined, "Payload loader, started by your jailbreak or Payload Manager.");
      case "console":
        return tr(
          "hub_origin_console",
          undefined,
          "Started outside Payload Manager: by your jailbreak, an autoloader, or a file you sent.",
        );
      default:
        return null;
    }
  };

  const report = useCallback((id: string, r: IntegrationReport) => {
    setReports((prev) => {
      const cur = prev[id];
      if (cur && cur.version === r.version && cur.webPort === r.webPort) return prev;
      return { ...prev, [id]: r };
    });
  }, []);

  // The newest stable release of each card whose running version is known, so the card
  // can link to a newer one. Each id is asked once: what it fills is not what it waits on.
  const asked = useRef(new Set<string>());
  const versionKey = cards
    .map((c) => `${c.id}=${reports[c.id]?.version ?? ""}`)
    .join("|");
  useEffect(() => {
    for (const c of cards) {
      const v = reports[c.id]?.version;
      if (!v || c.entryIds.length !== 1 || asked.current.has(c.id)) continue;
      asked.current.add(c.id);
      payloadsRelease(c.id)
        .then((r) => {
          if (r.tag) setLatest((prev) => ({ ...prev, [c.id]: { tag: r.tag, url: r.html_url } }));
        })
        .catch(() => asked.current.delete(c.id));
    }
    // versionKey stands for cards + reports here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [versionKey]);

  // A link that opened a card (an old tab's URL, a notification) scrolls to it once.
  // Any id of a card standing for several entries (kstuff's builds) finds that card.
  const scrolledTo = useRef<string | null>(null);
  // A card shown on Payload Manager's saved row instead opens Payload Manager's card.
  const linked = openId
    ? (cards.find((c) => c.id === openId) ?? cards.find((c) => c.entryIds.includes(openId)))?.id ?? null
    : null;
  const openCard = linked && groups?.hidden.has(linked) && pmCardShown ? "payload-manager" : linked;
  useEffect(() => {
    if (!openId || !openCard || scrolledTo.current === openId) return;
    scrolledTo.current = openId;
    document
      .querySelector(`[data-card="${CSS.escape(openCard)}"]`)
      ?.scrollIntoView({ block: "start", behavior: "smooth" });
  }, [openId, openCard]);

  const setOpen = (id: string | null) => {
    const next = new URLSearchParams(params);
    if (id) next.set("open", id);
    else next.delete("open");
    setParams(next, { replace: true });
  };

  const stop = async (card: HubCard) => {
    const p = card.processes[0];
    if (!p) return;
    const kind = killPrompt(p);
    if (!kind) return;
    const ok = await confirm(
      card.byPid
        ? {
            title: tr(
              "hub_kill_pid_title",
              { name: card.name, pid: p.pid },
              `Stop ${card.name} (pid ${p.pid})?`,
            ),
            message: tr(
              "hub_kill_pid_body",
              { name: card.name, pid: p.pid },
              `This kills only process ${p.pid}. ${card.name} is the name the loader gives any payload that doesn't name itself, so other processes with that name keep running.`,
            ),
            confirmLabel: tr("processes_kill", undefined, "Kill"),
            destructive: true,
          }
        : {
            title: tr("processes_kill_payload_title", { name: card.name }, `Stop payload "${card.name}"?`),
            message: tr(
              "processes_kill_payload_body",
              { name: card.name },
              `This kills the payload "${card.name}" (kstuff, etaHEN and the like). Whatever it provides stops until you send it again, and games that need it may stop starting.`,
            ),
            confirmLabel: tr("processes_kill", undefined, "Kill"),
            destructive: true,
          },
    );
    if (!ok) return;
    setBusy(card.id);
    try {
      for (const proc of card.byPid ? [p] : card.processes) {
        const ack = await processKill(addr, proc.pid);
        if (!ack.ok) throw new Error(ack.err || `pid ${proc.pid}`);
      }
      log.info("payloads", `stopped ${card.name}`);
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setBusy(null);
      await refreshProcs();
    }
  };

  // What each saved row that is running shows: its pid, Stop (the card's flow), and the card's
  // lines (kstuff's builds, Kraken Mount's update note).
  const pmRunning = ((): Record<string, PmRunning> | null => {
    if (!matched || !pmList) return null;
    const out: Record<string, PmRunning> = {};
    for (const [path, c] of matched) {
      const p = c.processes[0];
      if (!p) continue;
      const port = reports[c.id]?.webPort ?? c.webPort;
      out[path] = {
        pid: p.pid,
        memoryMib: p.memory_mib,
        ours: false,
        canStop: killPrompt(p) !== null,
        busy: busy === c.id,
        onStop: () => void stop(c),
        variants: c.variants,
        webUrl: port ? `http://${withPort(host.trim(), port)}/` : null,
        note: c.id === "kraken-mount" ? <KrakenUpdateNote kraken={mounter?.kraken} /> : undefined,
      };
    }
    // The helper's own copy in the list: it is the helper this app is talking to.
    const helper = storedHelper(pmList);
    const self = procs.find((p) => p.is_self);
    if (helper && self && !out[helper.path]) {
      out[helper.path] = {
        pid: self.pid,
        memoryMib: self.memory_mib,
        ours: true,
        canStop: false,
        busy: false,
        onStop: () => undefined,
        variants: [],
        webUrl: null,
      };
    }
    return out;
  })();
  const pmRunningCount = pmRunning ? Object.keys(pmRunning).length : null;

  const renderCard = (c: HubCard) => {
    const rep = reports[c.id];
    const rel = latest[c.id];
    const newer =
      rep?.version && rel && compareVersions(rep.version, rel.tag) === -1 ? rel : null;
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
        expanded={openCard === c.id}
        onToggle={() => setOpen(openCard === c.id ? null : c.id)}
        onStop={() => void stop(c)}
        origin={originText(c)}
        summary={
          c.integration === "hush"
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
                    running={c.state === "running"}
                    pm={{ view: pmView, running: pmRunningCount }}
                    onReport={report}
                    fallback={fallback}
                  />
                )
              : c.id === "kraken-mount"
                ? (fallback) => <KrakenSummary host={host} fallback={fallback} />
                : null
        }
        panel={
          c.integration === "hush" ? (
            <HushPanel host={host} view={hush.view} procs={procs} refresh={hush.refresh} />
          ) : c.integration ? (
            <IntegrationPanel id={c.integration} host={host} pmRunning={pmRunning ?? undefined} />
          ) : null
        }
      />
    );
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
          message={tr(
            "hub_empty_body",
            undefined,
            "No payload from the catalogue is running or installed on this console. Catalog has them, and Send loads any payload file.",
          )}
          action={
            <div className="flex flex-wrap justify-center gap-2">
              <Button size="sm" leftIcon={<Boxes size={14} />} onClick={() => navigate("/payloads?tab=catalog")}>
                {tr("payloads_tab_catalog", undefined, "Catalog")}
              </Button>
              <Button size="sm" leftIcon={<Rocket size={14} />} onClick={() => navigate("/payloads?tab=send")}>
                {tr("payloads_tab_send", undefined, "Send payload")}
              </Button>
            </div>
          }
        />
      ) : groups ? (
        <>
          {groups.top.length > 0 && <ul className="flex flex-col gap-2.5">{groups.top.map(renderCard)}</ul>}
          {groups.also.length > 0 && (
            <section className="flex flex-col gap-2" data-testid="hub-also-running">
              <div>
                <h3 className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
                  {tr("hub_also_running", undefined, "Also running")}
                </h3>
                <p className="mt-0.5 text-xs text-[var(--color-muted)]">
                  {tr(
                    "hub_also_running_body",
                    undefined,
                    "Not in your Payload Manager list. Payloads above run from your own list.",
                  )}{" "}
                  <button
                    type="button"
                    className="underline underline-offset-2 hover:text-[var(--color-text)]"
                    onClick={() => navigate(RUNNING_LINK)}
                  >
                    {tr("hub_also_running_link", undefined, "Every process is on the Running tab.")}
                  </button>
                </p>
              </div>
              <ul className="flex flex-col gap-2.5">{groups.also.map(renderCard)}</ul>
            </section>
          )}
        </>
      ) : (
        <ul className="flex flex-col gap-2.5">{cards.map(renderCard)}</ul>
      )}
      {dialog}
    </div>
  );
}

/** Kraken Mount's card line, with the update note when it is older than it should be. */
function KrakenSummary({ host, fallback }: { host: string; fallback: ReactNode }) {
  const mounter = useMounterFor(host);
  return (
    <>
      {fallback}
      <KrakenUpdateNote kraken={mounter?.kraken} />
    </>
  );
}

/** The one-line live summary an integration shows on its card. */
function Summary({
  id,
  host,
  running,
  pm,
  onReport,
  fallback,
}: {
  id: IntegrationId;
  host: string;
  running: boolean;
  /** Payload Manager's overview and how many of its saved payloads run (null: not matched). */
  pm: { view: PmOverview | null | undefined; running: number | null };
  onReport: (id: string, r: IntegrationReport) => void;
  fallback: ReactNode;
}) {
  if (id === "payload-manager") {
    return (
      <PmSummary view={pm.view} running={pm.running} onReport={(r) => onReport(id, r)} fallback={fallback} />
    );
  }
  if (id === "shadowmountplus") return <SmpSummary host={host} running={running} fallback={fallback} />;
  return <>{fallback}</>;
}

/** The integration's own panel, as its tab showed it before. */
function IntegrationPanel({
  id,
  host,
  pmRunning,
}: {
  id: IntegrationId;
  host: string;
  pmRunning?: Record<string, PmRunning>;
}) {
  if (id === "payload-manager") return <PayloadManagerPanel running={pmRunning} />;
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
  origin,
  summary,
  panel,
}: {
  card: HubCard;
  host: string;
  version: string | null;
  webPort: number | null;
  /** A newer release than the one running: its tag and page. */
  newer: { tag: string; url: string } | null;
  /** The catalogue was read, so "not in the catalogue" can be said (not in the web UI). */
  catalogued: boolean;
  /** Overrides the process-based Stop (an integration with its own clean stop). */
  canStop?: boolean;
  busy: boolean;
  expanded: boolean;
  onToggle: () => void;
  onStop: () => void;
  /** Who started it, for a running card: one line, so nobody reads it as PS5Upload's doing. */
  origin: string | null;
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
          {origin && (
            <div className="mt-0.5 text-xs text-[var(--color-muted)]" data-testid="hub-origin">
              {origin}
            </div>
          )}
          {card.variants.length > 1 && (
            <div className="mt-0.5 text-xs text-[var(--color-muted)]">
              {tr(
                "hub_variants",
                { names: card.variants.join(", ") },
                `One of: ${card.variants.join(", ")} — the console can't tell which.`,
              )}
            </div>
          )}
        </div>
        <div className="ms-auto flex flex-wrap items-center gap-1.5">
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
          {newer && newer.url && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<ExternalLink size={14} />}
              onClick={() => void openExternalUrl(newer.url)}
              title={tr(
                "hub_newer_page_tooltip",
                undefined,
                "ps5upload doesn't download payloads. Get the newer file from its project page, then send it from Send payload.",
              )}
            >
              {tr("pu_out_open_page", { version: newer.tag }, `${newer.tag} is out — open project page`)}
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
