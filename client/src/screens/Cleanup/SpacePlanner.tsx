import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { HardDriveDownload, ListChecks, Square, Wand2 } from "lucide-react";

import type { CleanupItem } from "../../api/cleanup";
import {
  appsInstalled,
  fetchVolumes,
  pkgInstalledInventory,
  scanLibrary,
  volumeLikelyFitsBytes,
  type Volume,
} from "../../api/ps5";
import { Badge, Button, Callout, Input, Select, Spinner } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { mgmtAddr, transferAddr } from "../../lib/addr";
import { driveName } from "../../lib/cleanupText";
import { formatBytes } from "../../lib/format";
import {
  driveToken,
  gibText,
  parseSizeGib,
  planSpace,
  planWithSelection,
  resolveDrive,
  runOrder,
  sameDrive,
  type PlannedStep,
  type SpacePlan,
  type SpacePlanInput,
  type SpacePlanRequest,
  type SpaceStepKind,
} from "../../lib/spacePlan";
import { playFor, useTrackedPlay } from "../../lib/trackedPlay";
import { useCollectionStore } from "../../state/collection";
import { useConnectionStore } from "../../state/connection";
import { useLangStore, useTr } from "../../state/lang";
import { libraryForHost, useLibraryStore } from "../../state/library";
import { pkgLibraryStore } from "../../state/pkgLibrary";
import { formatLastSeen, usePlayTimeStore } from "../../state/playTime";
import { useRunningAppsStore, runningOn } from "../../state/runningApps";
import { gatherSpacePlan, planVolumes } from "../../state/spacePlanGather";
import {
  consoleSpaceRunDeps,
  runSpacePlan,
  stopSpacePlan,
  useSpaceRunStore,
  type StepProgress,
} from "../../state/spacePlanRun";
import { runWalk } from "../DiskUsage/duTree";
import { humanizeCaught } from "../../lib/humanizeError";

type Tr = ReturnType<typeof useTr>;

const driveLabel = (tr: Tr, volume: string) => driveName(tr, driveToken(volume));

/** "Free 112 GiB: move A and B to Extended 1; uninstall C (you have its copy); clear 4 GiB of
 *  leftovers". */
export function planSummary(tr: Tr, plan: SpacePlan): string {
  const steps = runOrder(plan);
  if (steps.length === 0) return tr("cleanup_plan_sum_none", undefined, "Nothing picked yet.");
  const parts: string[] = [];
  const leftovers = steps.filter((s) => s.candidate.kind === "leftover");
  if (leftovers.length > 0) {
    const size = formatBytes(leftovers.reduce((n, s) => n + s.candidate.bytes, 0));
    parts.push(tr("cleanup_plan_sum_leftovers", { size }, `clear ${size} of leftovers`));
  }
  const byDest = new Map<string, string[]>();
  for (const s of steps) {
    if (s.candidate.kind !== "move" || !s.dest) continue;
    byDest.set(s.dest, [...(byDest.get(s.dest) ?? []), s.candidate.label]);
  }
  for (const [dest, names] of byDest) {
    const drive = driveLabel(tr, dest);
    parts.push(tr("cleanup_plan_sum_move", { names: names.join(", "), drive }, `move ${names.join(", ")} to ${drive}`));
  }
  const un = steps.filter((s) => s.candidate.kind === "uninstall").map((s) => s.candidate.label);
  if (un.length > 0)
    parts.push(
      tr("cleanup_plan_sum_uninstall", { names: un.join(", ") }, `uninstall ${un.join(", ")} (your Collection has a copy)`),
    );
  const size = formatBytes(plan.freedBytes);
  return tr("cleanup_plan_sum", { size, steps: parts.join("; ") }, `Free ${size}: ${parts.join("; ")}`);
}

/** The confirm dialog's list: every step, one per line, the destructive ones said plainly. */
export function confirmLines(tr: Tr, plan: SpacePlan): string {
  const steps = runOrder(plan);
  const lines: string[] = [];
  const leftovers = steps.filter((s) => s.candidate.kind === "leftover");
  if (leftovers.length > 0) {
    const size = formatBytes(leftovers.reduce((n, s) => n + s.candidate.bytes, 0));
    lines.push(
      "• " +
        tr(
          "cleanup_plan_confirm_leftovers",
          { count: leftovers.length, size },
          `Delete ${leftovers.length} leftovers (${size})`,
        ) +
        `: ${leftovers.map((s) => s.candidate.label).join(", ")}`,
    );
  }
  for (const s of steps) {
    const c = s.candidate;
    const size = formatBytes(c.bytes);
    if (c.kind === "move" && s.dest)
      lines.push(
        "• " +
          tr(
            "cleanup_plan_confirm_move",
            { name: c.label, size, drive: driveLabel(tr, s.dest) },
            `Move ${c.label} (${size}) to ${driveLabel(tr, s.dest)}: copied first, then the original is deleted`,
          ),
      );
    if (c.kind === "uninstall")
      lines.push(
        "• " +
          tr(
            "cleanup_plan_confirm_uninstall",
            { name: c.label, size },
            `Uninstall ${c.label} (${size}): install it again from your Collection`,
          ),
      );
  }
  return lines.join("\n");
}

function lastPlayedText(tr: Tr, ms: number | null, lang: string): string {
  if (ms == null) return tr("cleanup_plan_played_unknown", undefined, "Last played: unknown");
  return tr("cleanup_plan_played", { when: formatLastSeen(ms, undefined, lang) }, `Last played ${formatLastSeen(ms, undefined, lang)}`);
}

function stateBadge(tr: Tr, p: StepProgress | undefined) {
  if (!p) return null;
  switch (p.state) {
    case "waiting":
      return <Badge size="sm">{tr("cleanup_plan_state_waiting", undefined, "Waiting")}</Badge>;
    case "running":
      return (
        <Badge size="sm" tone="accent">
          {p.bytesTotal
            ? tr(
                "cleanup_plan_state_progress",
                { done: formatBytes(p.bytesDone ?? 0), total: formatBytes(p.bytesTotal) },
                `${formatBytes(p.bytesDone ?? 0)} of ${formatBytes(p.bytesTotal)}`,
              )
            : tr("cleanup_plan_state_running", undefined, "Working…")}
        </Badge>
      );
    case "done":
      return (
        <Badge size="sm" tone="good">
          {tr("cleanup_plan_state_done", undefined, "Done")}
        </Badge>
      );
    case "stopped":
      return <Badge size="sm">{tr("cleanup_plan_state_stopped", undefined, "Not run")}</Badge>;
    case "skipped":
    case "failed":
      return (
        <Badge size="sm" tone={p.state === "failed" ? "bad" : "warn"}>
          {p.state === "failed"
            ? tr("cleanup_plan_state_failed", undefined, "Failed")
            : tr("cleanup_plan_state_skipped", undefined, "Skipped")}
        </Badge>
      );
  }
}

function detailText(tr: Tr, p: StepProgress | undefined): string | null {
  if (!p?.detail || (p.state !== "failed" && p.state !== "skipped")) return null;
  switch (p.detail) {
    case "dest_exists":
      return tr("cleanup_plan_detail_dest_exists", undefined, "Something with that name is already on the other drive.");
    case "both_copies":
      return tr("cleanup_plan_detail_both_copies", undefined, "Copied, but the original couldn't be deleted: both copies are there now.");
    case "still_present":
      return tr(
        "cleanup_plan_detail_still_present",
        undefined,
        "The PS5 accepted the uninstall but the game is still there. A large game can take a few minutes.",
      );
    default:
      return p.detail;
  }
}

const GROUP_TITLE: Record<SpaceStepKind, [string, string]> = {
  leftover: ["cleanup_plan_group_leftovers", "Leftovers"],
  move: ["cleanup_plan_group_moves", "Move to another drive"],
  uninstall: ["cleanup_plan_group_uninstall", "Uninstall (your Collection has a copy)"],
};

/** The plan: summary, totals, and every candidate with a box to tick. Pure props for tests. */
export function SpacePlanView({
  plan,
  progress,
  locked,
  lang,
  onToggle,
}: {
  plan: SpacePlan;
  progress: Record<string, StepProgress>;
  locked: boolean;
  lang: string;
  onToggle: (id: string) => void;
}) {
  const tr = useTr();
  if (plan.shortBytes === 0) {
    return (
      <Callout tone="success" title={tr("cleanup_plan_fits_title", undefined, "It already fits")}>
        {tr("cleanup_plan_fits_body", undefined, "That drive has room for it now. Nothing needs to go.")}
      </Callout>
    );
  }
  const groups = (["leftover", "move", "uninstall"] as SpaceStepKind[])
    .map((k) => ({ kind: k, steps: plan.steps.filter((s) => s.candidate.kind === k) }))
    .filter((g) => g.steps.length > 0);
  return (
    <div className="space-y-3" data-testid="space-plan-view">
      <div className="text-sm font-medium" data-testid="space-plan-summary">
        {planSummary(tr, plan)}
      </div>
      <div className="text-xs tabular-nums text-[var(--color-muted)]" data-testid="space-plan-totals">
        {tr(
          "cleanup_plan_totals",
          { freed: formatBytes(plan.freedBytes), needed: formatBytes(plan.shortBytes) },
          `Frees ${formatBytes(plan.freedBytes)} of the ${formatBytes(plan.shortBytes)} needed`,
        )}
      </div>
      {!plan.enough ? (
        <Callout tone="warn" title={tr("cleanup_plan_short_title", undefined, "Not enough yet")}>
          {groups.length === 0
            ? tr(
                "cleanup_plan_nothing",
                undefined,
                "Nothing on this drive can safely make room: no leftovers, no game that can move to another drive, and no game with a copy in your Collection.",
              )
            : tr(
                "cleanup_plan_short_body",
                { size: formatBytes(plan.missingBytes) },
                `${formatBytes(plan.missingBytes)} is still missing. Tick more below, or pick another drive.`,
              )}
        </Callout>
      ) : null}
      {groups.map((g) => (
        <section
          key={g.kind}
          className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)]"
          data-testid={`space-plan-group-${g.kind}`}
        >
          <header className="border-b border-[var(--color-border)] px-3 py-2 text-sm font-semibold">
            {tr(GROUP_TITLE[g.kind][0], undefined, GROUP_TITLE[g.kind][1])}
          </header>
          <ul className="divide-y divide-[var(--color-border)]">
            {g.steps.map((s) => (
              <StepRow
                key={s.candidate.id}
                step={s}
                progress={progress[s.candidate.id]}
                locked={locked}
                lang={lang}
                onToggle={() => onToggle(s.candidate.id)}
              />
            ))}
          </ul>
        </section>
      ))}
      <p className="text-[11px] text-[var(--color-muted)]">
        {tr(
          "cleanup_plan_played_hint",
          undefined,
          "Last played is when this app saw the game running. A game played while the app was closed can show unknown or older.",
        )}
      </p>
    </div>
  );
}

function StepRow({
  step,
  progress,
  locked,
  lang,
  onToggle,
}: {
  step: PlannedStep;
  progress: StepProgress | undefined;
  locked: boolean;
  lang: string;
  onToggle: () => void;
}) {
  const tr = useTr();
  const c = step.candidate;
  const bits: string[] = [];
  if (c.kind !== "leftover") bits.push(lastPlayedText(tr, c.lastPlayedMs, lang));
  if (c.kind === "move" && step.selected)
    bits.push(
      step.dest
        ? tr("cleanup_plan_to_drive", { drive: driveLabel(tr, step.dest) }, `To ${driveLabel(tr, step.dest)}`)
        : tr("cleanup_plan_no_room_elsewhere", undefined, "No other drive has room for it"),
    );
  const detail = detailText(tr, progress);
  return (
    <li className="flex items-center gap-3 px-3 py-2" data-testid={`space-plan-step-${c.id}`}>
      <input
        type="checkbox"
        className="checkbox h-5 w-5 shrink-0"
        checked={step.selected}
        disabled={locked}
        onChange={onToggle}
        aria-label={c.label}
      />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span className="truncate font-medium">{c.label}</span>
          {stateBadge(tr, progress)}
        </div>
        {bits.length > 0 ? <div className="text-xs text-[var(--color-muted)]">{bits.join(" · ")}</div> : null}
        {detail ? <div className="text-xs text-[var(--color-bad)]">{detail}</div> : null}
      </div>
      <div className="shrink-0 text-end text-sm tabular-nums">{formatBytes(c.bytes)}</div>
    </li>
  );
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Folder size by the Disk usage walk; null when it stopped short (a floor is no use here). */
async function measureFolder(host: string, path: string): Promise<number | null> {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), 120_000);
  try {
    const s = await runWalk(mgmtAddr(host), path, () => {}, ac.signal);
    if (!s || s.state !== "done" || s.partial) return null;
    return s.bytes;
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Make room: type a size, pick a drive, get a plan, tick what you agree with, and "Do it".
 * Opened from Clean up's "Make room", or from a refused install with the size filled in.
 */
export function SpacePlanner({
  host,
  scanId,
  items,
  request,
  onClose,
  onRan,
}: {
  host: string;
  /** The Clean up screen's finished scan: its leftovers, and the id their removal needs. */
  scanId: number | null;
  items: CleanupItem[];
  request: SpacePlanRequest | null;
  onClose: () => void;
  /** After a run: the screen scans again. */
  onRan: () => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const lang = useLangStore((s) => s.lang);
  const payloadVersion = useConnectionStore((s) => s.payloadVersion);
  const tracked = useTrackedPlay(host);
  const playByHost = usePlayTimeStore((s) => s.byHost);
  const lastSeenByHost = usePlayTimeStore((s) => s.lastSeenByHost);
  const { confirm, dialog } = useConfirm();
  const run = useSpaceRunStore();

  const [drives, setDrives] = useState<Volume[] | null>(null);
  const [size, setSize] = useState(() => gibText(request?.neededBytes ?? 0));
  const [drive, setDrive] = useState<string>("");
  const [phase, setPhase] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [input, setInput] = useState<SpacePlanInput | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [volumes, setVolumes] = useState<string[]>([]);
  const [unmeasured, setUnmeasured] = useState(0);
  const [roomAfter, setRoomAfter] = useState<number | null>(null);
  // The scan the plan's leftovers came from: their ids mean nothing to a newer scan.
  const [planScanId, setPlanScanId] = useState<number | null>(null);
  const live = useRef(0);

  useEffect(() => {
    let on = true;
    fetchVolumes(transferAddr(host))
      .then((v) => {
        if (!on) return;
        const { drives: d } = planVolumes(v);
        setDrives(d);
        const paths = d.map((x) => x.path);
        setDrive((cur) => cur || resolveDrive(request?.drive ?? "internal", paths) || paths[0] || "");
      })
      .catch((e) => on && setError(humanizeCaught(e)));
    return () => {
      on = false;
    };
  }, [host, request?.drive]);

  const plan = useMemo(() => (input ? planWithSelection(input, selected) : null), [input, selected]);
  const neededBytes = parseSizeGib(size);
  const busy = phase !== null || run.running;
  const ranHere = run.host === host && (run.running || run.finished) && run.order.length > 0;

  async function makePlan() {
    if (!neededBytes || !drive || scanId === null) return;
    const gen = ++live.current;
    setError(null);
    setInput(null);
    setRoomAfter(null);
    useSpaceRunStore.getState().reset();
    try {
      const collectionIds = new Set(Object.keys(useCollectionStore.getState().library?.games ?? {}).map((k) => k.toUpperCase()));
      const running = new Set([...runningOn(useRunningAppsStore.getState(), host)].map((t) => t.toUpperCase()));
      const slot = libraryForHost(useLibraryStore.getState(), host);
      const addr = transferAddr(host);
      const g = await gatherSpacePlan(
        {
          target: drive,
          leftovers: items,
          collectionIds,
          running,
          lastPlayed: (id) => {
            const ms = playFor(tracked, { byHost: playByHost, lastSeenByHost }, host, id ?? null).lastSeenMs;
            return ms && ms > 0 ? ms : null;
          },
        },
        {
          volumes: () => fetchVolumes(addr),
          // The Games list read this session is used as is; otherwise it is read now.
          library: async () => slot.entries ?? (await scanLibrary(addr)),
          installed: async () => (await appsInstalled(addr)).titles,
          inventory: (id) => pkgInstalledInventory(addr, id),
          folderBytes: (p) => measureFolder(host, p),
        },
        (p, name) => {
          if (gen !== live.current) return;
          setPhase(
            p === "drives"
              ? tr("cleanup_plan_phase_drives", undefined, "Reading the drives…")
              : p === "games"
                ? tr("cleanup_plan_phase_games", undefined, "Reading the games on the PS5…")
                : tr("cleanup_plan_phase_sizes", { name: name ?? "" }, `Measuring ${name ?? ""}…`),
          );
        },
      );
      if (gen !== live.current) return;
      const next: SpacePlanInput = {
        neededBytes,
        roomBytes: g.roomBytes,
        candidates: g.candidates,
        destRoom: g.destRoom,
        nowMs: Date.now(),
      };
      const proposed = planSpace(next);
      setInput(next);
      setPlanScanId(scanId);
      setVolumes(g.volumes);
      setUnmeasured(g.unmeasured);
      setSelected(new Set(proposed.steps.filter((s) => s.selected).map((s) => s.candidate.id)));
    } catch (e) {
      if (gen === live.current) setError(humanizeCaught(e));
    } finally {
      if (gen === live.current) setPhase(null);
    }
  }

  function toggle(id: string) {
    setSelected((cur) => {
      const next = new Set(cur);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function doIt() {
    if (!plan || planScanId === null || planScanId !== scanId) return;
    const ok = await confirm({
      title: tr("cleanup_plan_confirm_title", { size: formatBytes(plan.freedBytes) }, `Free ${formatBytes(plan.freedBytes)} on the PS5?`),
      message:
        confirmLines(tr, plan) +
        "\n\n" +
        tr("cleanup_plan_confirm_body", undefined, "The steps run one after another. Deleted files can't be recovered."),
      confirmLabel: tr("cleanup_plan_do_it", undefined, "Do it"),
      destructive: true,
    });
    if (!ok) return;
    await runSpacePlan(host, plan, volumes, consoleSpaceRunDeps(host, planScanId, payloadVersion));
    // What the drive has now, from the console (it can take a moment to catch up).
    try {
      const after = planVolumes(await fetchVolumes(transferAddr(host))).drives.find((v) => sameDrive(v.path, drive));
      setRoomAfter(after ? volumeLikelyFitsBytes(after) : null);
    } catch {
      setRoomAfter(null);
    }
    await sleep(0);
    onRan();
  }

  function retry() {
    const r = request?.retry;
    if (!r) return;
    if (r.kind === "pkg") {
      void pkgLibraryStore(host).getState().install(r.path, host);
      navigate("/send");
    } else navigate(r.to);
  }

  const chosen = plan ? runOrder(plan).length : 0;
  const stale = plan !== null && !run.running && planScanId !== scanId;
  const failures = Object.values(run.steps).filter((p) => p.state === "failed" || p.state === "skipped").length;

  return (
    <section
      className="space-y-3 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-2)] p-3"
      data-testid="space-planner"
    >
      <header className="flex flex-wrap items-center gap-2">
        <Wand2 size={16} aria-hidden />
        <div className="min-w-0 flex-1">
          <div className="text-sm font-semibold">{tr("cleanup_plan_title", undefined, "Make room for a game")}</div>
          <div className="text-xs text-[var(--color-muted)]">
            {tr(
              "cleanup_plan_hint",
              undefined,
              "Say how much room you need and where. The plan clears leftovers first, then moves games to another PS5 drive, then uninstalls only games your Collection has a copy of.",
            )}
          </div>
        </div>
        <Button variant="ghost" size="sm" onClick={onClose} disabled={run.running}>
          {tr("close", undefined, "Close")}
        </Button>
      </header>

      <div className="flex flex-wrap items-end gap-2">
        <div className="w-32">
          <Input
            label={tr("cleanup_plan_size", undefined, "Size (GiB)")}
            inputMode="decimal"
            value={size}
            onChange={(e) => setSize(e.target.value)}
            disabled={busy}
            data-testid="space-plan-size"
          />
        </div>
        <Select
          block={false}
          label={tr("cleanup_plan_drive", undefined, "Drive")}
          value={drive}
          disabled={busy || !drives}
          onChange={(e) => setDrive(e.target.value)}
          options={(drives ?? []).map((v) => ({
            value: v.path,
            label: `${driveLabel(tr, v.path)} · ${formatBytes(volumeLikelyFitsBytes(v))} ${tr("fs_free", undefined, "free")}`,
          }))}
        />
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<ListChecks size={14} />}
          onClick={() => void makePlan()}
          disabled={busy || !neededBytes || !drive || scanId === null}
          data-testid="space-plan-make"
        >
          {tr("cleanup_plan_make", undefined, "Plan")}
        </Button>
      </div>
      {scanId === null ? (
        <p className="text-xs text-[var(--color-muted)]">
          {tr("cleanup_plan_wait_scan", undefined, "Waiting for the clean-up scan to finish…")}
        </p>
      ) : null}

      {phase ? (
        <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]" data-testid="space-plan-phase">
          <Spinner size={12} />
          <span className="truncate">{phase}</span>
        </div>
      ) : null}
      {error ? (
        <Callout tone="error" title={tr("cleanup_plan_failed", undefined, "Couldn't make a plan")}>
          {error}
        </Callout>
      ) : null}

      {plan ? (
        <>
          <SpacePlanView
            plan={plan}
            progress={ranHere ? run.steps : {}}
            locked={busy || ranHere}
            lang={lang}
            onToggle={toggle}
          />
          {unmeasured > 0 ? (
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "cleanup_plan_unmeasured",
                { count: unmeasured },
                `${unmeasured} game folders couldn't be measured and are left out.`,
              )}
            </p>
          ) : null}
          {stale && !ranHere ? (
            <p className="text-xs text-[var(--color-warn)]">
              {tr("cleanup_plan_stale", undefined, "The clean-up list was read again. Make the plan again before running it.")}
            </p>
          ) : null}
          {plan.shortBytes > 0 && !ranHere ? (
            <div className="flex flex-wrap items-center justify-end gap-2">
              <Button
                variant="danger"
                size="sm"
                leftIcon={<Wand2 size={14} />}
                disabled={busy || chosen === 0 || stale}
                onClick={() => void doIt()}
                data-testid="space-plan-do-it"
              >
                {tr("cleanup_plan_do_it", undefined, "Do it")}
              </Button>
            </div>
          ) : null}
        </>
      ) : null}

      {run.running && run.host === host ? (
        <div className="flex flex-wrap items-center gap-2 text-xs" data-testid="space-plan-running">
          <Spinner size={12} />
          <span>{tr("cleanup_plan_running", undefined, "Making room… each step runs in turn.")}</span>
          <Button
            variant="ghost"
            size="sm"
            leftIcon={<Square size={10} />}
            disabled={run.stopRequested}
            onClick={() => stopSpacePlan(consoleSpaceRunDeps(host, planScanId ?? 0, payloadVersion), plan)}
          >
            {tr("cleanup_plan_stop", undefined, "Stop after this step")}
          </Button>
        </div>
      ) : null}

      {ranHere && !run.running && plan ? (
        <div className="space-y-2" data-testid="space-plan-result">
          <Callout
            tone={failures > 0 ? "warn" : "success"}
            title={
              failures > 0
                ? tr("cleanup_plan_result_some", { count: failures }, `Done, but ${failures} steps didn't work`)
                : tr("cleanup_plan_result_ok", undefined, "Done")
            }
          >
            {roomAfter !== null
              ? tr(
                  "cleanup_plan_result_room",
                  { size: formatBytes(roomAfter) },
                  `That drive now has room for about ${formatBytes(roomAfter)}. It can take a minute to show.`,
                )
              : tr("cleanup_plan_result_room_unknown", undefined, "The drive's free space couldn't be read again yet.")}
          </Callout>
          {request?.retry ? (
            <Button variant="primary" size="sm" leftIcon={<HardDriveDownload size={14} />} onClick={retry}>
              {request.retry.kind === "pkg"
                ? tr("cleanup_plan_retry_install", { name: request.retry.label }, `Install ${request.retry.label} now`)
                : tr("cleanup_plan_retry_back", { name: request.retry.label }, `Back to ${request.retry.label}`)}
            </Button>
          ) : null}
        </div>
      ) : null}
      {dialog}
    </section>
  );
}
