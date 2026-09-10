import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Layers, Play, RotateCcw } from "lucide-react";
import { Button, Callout, Modal, Spinner } from "../../components";
import {
  fsCopy,
  fsDelete,
  fsListDir,
  fsMkdir,
  processKill,
  processList,
  startTransferFile,
  smpImageRwBegin,
  smpImageRwFinish,
  waitForJob,
  sdkPatch,
  sdkRestore,
  type InstalledTitle,
  type SdkScanResponse,
} from "../../api/ps5";
import { hostOf, mgmtAddr, transferAddr } from "../../lib/addr";
import { useTr } from "../../state/lang";
import { useEditSessionStore } from "../../state/editSession";
import { LibrarySourcePicker } from "./LibrarySourcePicker";
import { appLaunch, klogChunk } from "../../api/ps5";
import { titleSdkPair, type ScanTitleInput } from "../../state/fakelibCorpus";
import {
  applyBackport,
  backportOverlayReady,
  BackportApplyError,
  existingLibraries,
  planBackport,
  rankSets,
  undoBackport,
  type BackportPlan,
  type BackportRecord,
  type BackportTransport,
  type VerifySample,
  type VerifyVerdict,
  verdictFrom,
  combineAttempts,
  FAILURE_ATTEMPTS,
  nextSetsAfter,
  type FakelibSet,
} from "../../lib/backport";
import {
  loadBackportRecord,
  removeBackportRecord,
  saveBackportRecord,
} from "../../state/backportRecords";

/** How long to watch a launched title before judging it.
 *
 *  Nine eight-second samples. A cold start from USB can take most of a minute
 *  to show threads (Nioh 3 looked dead at 18 seconds and reached 263 threads),
 *  so a short window produces false failures. */
const VERIFY_SAMPLES = 9;
const VERIFY_INTERVAL_MS = 8000;

export function BackportPanel({
  open,
  host,
  title,
  titleSdkVersion,
  sets,
  corpusRoot,
  corpusError,
  onCorpusChanged,
  scanTitles,
  scan,
  onClose,
  onLaunch,
}: {
  open: boolean;
  host: string;
  title: InstalledTitle;
  /** `sdkVersion` from the target's param.json — what it was BUILT against. */
  titleSdkVersion: string;
  /** Sets from the local fakelibs corpus (manifest.json). */
  sets: FakelibSet[];
  /** Host path holding `<titleId>/<library>` for each set. */
  corpusRoot: string;
  corpusError?: string | null;
  /** Called after libraries are imported or scanned, so the panel re-reads the
   *  corpus and can propose a set without being closed and reopened. */
  onCorpusChanged: () => void;
  /** Titles the scan route may walk. Empty when the console is unreachable. */
  scanTitles: ScanTitleInput[];
  scan: SdkScanResponse;
  onClose: () => void;
  onLaunch: (title: InstalledTitle) => void;
}) {
  const tr = useTr();
  const [plan, setPlan] = useState<BackportPlan | null>(null);
  /* Sets already tried and rejected for this title. No signal in the data
   * predicts which set a game needs, so working down the ranked list is
   * the actual workflow, not an error path. */
  const [rejected, setRejected] = useState<string[]>([]);
  const [verdict, setVerdict] = useState<VerifyVerdict | null>(null);
  /* The set that just failed and how, kept across the undo so the next
   * proposal can honour it: after a missing-library failure a SMALLER set
   * cannot supply what was missing, and offering one costs the user a whole
   * install-launch-undo cycle to learn nothing. */
  const [lastFailure, setLastFailure] = useState<{ set: FakelibSet; verdict: VerifyVerdict } | null>(null);
  const [verifying, setVerifying] = useState(false);
  const [record, setRecord] = useState<BackportRecord | null>(() =>
    loadBackportRecord(host, title.titleId),
  );
  const [patchLibc, setPatchLibc] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const candidates = rankSets(sets, rejected, title.titleId);
  /* After a failure, what is still worth trying. A missing-library failure
   * needs a BIGGER set, so smaller ones are dropped rather than offered and
   * wasted. */
  const nextCandidates =
    verdict && plan ? nextSetsAfter(verdict, plan.set, candidates) : candidates;

  const load = useCallback(async () => {
    if (!open || record) return;
    const ranked = rankSets(sets, rejected, title.titleId);
    const candidate = (lastFailure
      ? nextSetsAfter(lastFailure.verdict, lastFailure.set, ranked)
      : ranked)[0];
    if (!candidate) {
      setPlan(null);
      // An empty corpus is not a failure — it is a new user, and the panel's
      // job is to offer the two ways to fill it. Only "tried everything" is a
      // dead end worth wording as one.
      setError(sets.length === 0
        ? null
        : lastFailure?.verdict.kind === "missing-libraries"
          ? tr("backport_no_larger", undefined,
              "This game needs libraries none of your sets have. Import a more complete pack, or scan another console.")
          : tr("backport_no_more", undefined, "Every available set has been tried for this title."));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      let present: Awaited<ReturnType<typeof fsListDir>> = [];
      try {
        present = await fsListDir(transferAddr(host), `${title.source}/fakelib`);
      } catch {
        present = [];
      }
      setPlan(planBackport(title, candidate, existingLibraries(present), corpusRoot));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [corpusError, host, open, corpusRoot, sets, record, rejected, lastFailure, title, titleSdkVersion, tr]);

  useEffect(() => { void load(); }, [load]);

  const transport: BackportTransport = {
    copyConsole: (from, to) => fsCopy(transferAddr(host), from, to),
    uploadHost: async (from, to) => {
      const jobId = await startTransferFile(from, to, transferAddr(host));
      await waitForJob(jobId);
    },
    mkdirConsole: (path) => fsMkdir(transferAddr(host), path),
    patch: async (titleId, libc) => {
      const result = await sdkPatch(titleId, "0x04000031", transferAddr(host), libc);
      if (!result.ok) throw new Error(result.error || result.detail || "SDK patch failed");
    },
    restore: async (titleId) => {
      const result = await sdkRestore(titleId, transferAddr(host));
      if (!result.ok) throw new Error(result.error || "Restore failed");
    },
    remove: (path) => fsDelete(transferAddr(host), path),
  };

  const imageBacked = title.imageBacked || title.source.startsWith("/mnt/shadowmnt/");

  /** Run a mutation while an SMP image is mounted read-write. Clicking
   * Backport/Undo is the user's authorization to stop that title; leaving it
   * running while its backing image is blipped risks a busy unmount. */
  const withWritableTitle = async <T,>(
    operation: () => Promise<T>,
    onCompleted?: (result: T) => void,
  ): Promise<T> => {
    let opened = false;
    if (imageBacked) {
      const addr = mgmtAddr(host);
      const running = (await processList(addr)).processes.find(
        (p) => p.title_id === title.titleId,
      );
      if (running) {
        const stopped = await processKill(addr, running.pid);
        if (!stopped.ok) throw new Error(stopped.err || `Could not stop ${title.titleName}`);
        await new Promise((resolve) => setTimeout(resolve, 2000));
      }
      await smpImageRwBegin(transferAddr(host), title.titleId);
      opened = true;
      await useEditSessionStore.getState().refresh(host);
    }
    let result: T | undefined;
    let operationError: unknown;
    try {
      result = await operation();
      onCompleted?.(result);
    } catch (error) {
      operationError = error;
    } finally {
      if (opened) {
        try {
          await smpImageRwFinish(transferAddr(host));
          await useEditSessionStore.getState().refresh(host);
        } catch (finishError) {
          await useEditSessionStore.getState().refresh(host);
          if (!operationError) throw finishError;
        }
      }
    }
    if (operationError) throw operationError;
    return result as T;
  };

  /** Launch the title and watch what happens.
   *
   *  Runs automatically after an install because the alternative is asking the
   *  user to go and find out, and because a failed backport should cost one
   *  click to move past rather than a manual launch, a manual close and a
   *  guess about what went wrong. */
  /** One launch, watched to the end of the window. */
  const attempt = useCallback(
    async (): Promise<VerifyVerdict> => {
      // Drain whatever is already buffered so the diagnosis reads only lines
      // from THIS launch — a previous attempt's unpatched-function line would
      // otherwise be read as this one's.
      await klogChunk(mgmtAddr(host)).catch(() => "");
      await appLaunch(transferAddr(host), title.titleId);
      const samples: VerifySample[] = [];
      // Drain klog on EVERY tick and keep it all. The kernel log is a small
      // ring buffer — 136 lines on the console this was measured against — and
      // it can be flooded by unrelated spam (SceSystemTts repeats two lines
      // continuously). Reading only at the end loses the one line that matters,
      // and reads its absence as "the libraries are merely wrong".
      let klog = "";
      for (let i = 0; i < VERIFY_SAMPLES; i += 1) {
        await new Promise((r) => setTimeout(r, VERIFY_INTERVAL_MS));
        const list = await processList(mgmtAddr(host)).catch(() => ({ processes: [] }));
        const proc = list.processes.find((pr) => pr.title_id === title.titleId);
        samples.push({ threads: proc ? proc.threads : null });
        klog += await klogChunk(mgmtAddr(host)).catch(() => "");
      }
      klog += await klogChunk(mgmtAddr(host)).catch(() => "");
      const verdict = verdictFrom(samples, klog, title.titleId);
      // A launch that produced nothing has two very different causes that look
      // identical from here. Before blaming the library set, check the eboot
      // actually carries the backport SDK pair — an un-backported title cannot
      // run whatever libraries are installed, and diagnosing that as a library
      // problem costs the user cycle after pointless cycle. (Measured: a title
      // whose SDK had been restored produced six launches returning ok with no
      // process, in both arms of a library experiment.)
      if (verdict.kind === "wrong-libraries" || verdict.kind === "missing-libraries") {
        const pair = await titleSdkPair(mgmtAddr(host), title.source);
        if (pair?.backported === false) return { kind: "not-backported" };
      }
      return verdict;
    },
    [host, title.titleId],
  );

  /** Verify, retrying a failure before believing it.
   *
   *  Launching is unreliable in the failing direction: an unchanged Red Dead
   *  ran, ran, then produced no process at all. Acting on one failed launch
   *  sends the user through an install-launch-undo cycle for nothing. */
  const verify = useCallback(
    async (failedSet?: FakelibSet) => {
      setVerifying(true);
      setVerdict(null);
      setError(null);
      try {
        const attempts: VerifyVerdict[] = [];
        for (let i = 0; i < FAILURE_ATTEMPTS; i += 1) {
          attempts.push(await attempt());
          const so_far = combineAttempts(attempts);
          setVerdict(so_far);
          // Stop as soon as the answer cannot change: a run, or a
          // missing-library message, both settle it.
          if (so_far.kind === "running" || so_far.kind === "missing-libraries") break;
        }
        const result = combineAttempts(attempts);
        setVerdict(result);
        if (result.kind === "missing-libraries" || result.kind === "wrong-libraries") {
          setLastFailure(failedSet ? { set: failedSet, verdict: result } : null);
        } else {
          setLastFailure(null);
        }
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setVerifying(false);
      }
    },
    [attempt],
  );

  const apply = async () => {
    if (!plan) return;
    setBusy(true);
    setError(null);
    try {
      await withWritableTitle(
        () => applyBackport(plan, transport, patchLibc),
        (completed) => {
          // Persist before the image is flipped back to read-only. If that
          // final remount fails, recovery still knows exactly what to undo.
          saveBackportRecord(host, completed);
          setRecord(completed);
        },
      );
      void verify(plan.set);
    } catch (e) {
      if (e instanceof BackportApplyError) {
        const partial = {
          titleId: title.titleId,
          setId: plan.set.id,
          targetSource: title.source,
          copiedPaths: e.copiedPaths,
          replaced: e.replaced,
          stashDir: plan.stashDir,
          complete: false,
        };
        saveBackportRecord(host, partial);
        setRecord(partial);
      }
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const undo = async () => {
    if (!record) return;
    setBusy(true);
    setError(null);
    try {
      await withWritableTitle(
        () => undoBackport(record, transport),
        () => removeBackportRecord(host, title.titleId),
      );
      // Remember that this set did not work, so "try another" moves on
      // rather than proposing the same one again.
      setRejected((prev) => prev.includes(record.setId) ? prev : [...prev, record.setId]);
      setRecord(null);
      setPlan(null);
      setVerdict(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const overlay = scan.overlay;
  const overlayBlocked = overlay?.state === "blocked" || overlay?.state === "error";
  const overlayReady = backportOverlayReady(overlay);

  return (
    <Modal open={open} onClose={onClose} title={tr("backport_title", { name: title.titleName }, `Backport ${title.titleName}`)} titleIcon={<Layers size={16} />} size="lg">
      <div className="space-y-4 p-4 text-sm">
        {overlayBlocked ? (
          <Callout tone="warn" title={tr("backport_overlay_unavailable", undefined, "Library overlay is not available")}>
            {overlay?.error || tr("backport_overlay_external", undefined, "An external BackPork/unionfs overlay is already active.")}
          </Callout>
        ) : null}
        {!overlayBlocked && !overlayReady ? (
          <Callout tone="warn" title={tr("backport_overlay_unavailable", undefined, "Library overlay is not available")}>
            {tr("backport_overlay_missing", undefined, "Send the current ps5upload payload before applying a backport. Older payloads cannot mount the selected libraries when the game launches.")}
          </Callout>
        ) : null}
        {error ? <Callout tone="error" title={tr("backport_failed", undefined, "Backport could not be completed")}>{error}</Callout> : null}
        {busy && !plan && !record ? <div className="flex items-center gap-2"><Spinner size={16} />{tr("backport_inspecting", undefined, "Inspecting installed library families…")}</div> : null}
        {!record && sets.length === 0 ? (
          <>
            <p>
              {tr("backport_needs_libraries", { name: title.titleName },
                `Backporting ${title.titleName} needs replacement system libraries. They are Sony files, so ps5upload cannot include them — you supply them once, from games you own, and every later backport reuses them.`)}
            </p>
            <LibrarySourcePicker
              addr={host}
              consoleName={hostOf(host)}
              scanTitles={scanTitles}
              onChanged={onCorpusChanged}
            />
          </>
        ) : null}
        {plan && !record ? (
          <>
            <p>{tr("backport_summary", { count: plan.copies.length, set: plan.set.label }, `This will downgrade the game to the FW 4 SDK pair and install the ${plan.copies.length}-library set that ${plan.set.label} ships. A set is installed whole — mixing libraries across games is what makes a backport fail to launch.`)}</p>
            {plan.replaced.length > 0 ? (
              <Callout tone="warn" title={tr("backport_replacing", { count: plan.replaced.length }, `${plan.replaced.length} existing librar${plan.replaced.length === 1 ? "y" : "ies"} will be set aside`)}>
                {tr("backport_replacing_body", { names: plan.replaced.map((l) => l.name).join(", ") }, `${plan.replaced.map((l) => l.name).join(", ")} — copied to the console before removal, and put back by Undo.`)}
              </Callout>
            ) : null}
            <details className="text-xs">
              <summary className="cursor-pointer text-[var(--color-muted)]">
                {tr("backport_get_more", undefined, "Get more libraries…")}
              </summary>
              <div className="pt-3">
                <LibrarySourcePicker
                  addr={host}
                  consoleName={hostOf(host)}
                  scanTitles={scanTitles}
                  onChanged={onCorpusChanged}
                />
              </div>
            </details>
            <p className="text-xs text-[var(--color-muted)]">{tr("backport_candidates", { n: candidates.length }, `${candidates.length} set${candidates.length === 1 ? "" : "s"} available. If the game does not start, Undo and try the next one.`)}</p>
            <ul className="max-h-40 overflow-auto rounded bg-[var(--color-surface-3)] p-3 font-mono text-xs">
              {plan.copies.map((copy) => <li key={copy.name}>{tr("backport_library_size", { name: copy.name, size: copy.size.toLocaleString() }, `${copy.name} (${copy.size.toLocaleString()} bytes)`)}</li>)}
            </ul>
            <label className="flex items-start gap-2">
              <input type="checkbox" checked={patchLibc} onChange={(e) => setPatchLibc(e.target.checked)} />
              <span><strong>{tr("sdk_patch_libc", undefined, "Also patch libc.prx")}</strong><br /><span className="text-[var(--color-muted)]">{tr("sdk_patch_libc_hint", undefined, "Helps some titles and stops others from launching. Leave off unless needed.")}</span></span>
            </label>
            {imageBacked ? <p className="text-xs text-[var(--color-muted)]">{tr("backport_image_cycle", undefined, "This disk image will be stopped if needed, remounted read-write for the edit, then returned to read-only automatically.")}</p> : null}
            <Button variant="primary" onClick={() => void apply()} disabled={busy || !overlayReady} loading={busy}>{tr("backport_action", undefined, "Backport")}</Button>
          </>
        ) : null}
        {record && (verifying || verdict) ? (
          verifying ? (
            <div className="flex items-center gap-2">
              <Spinner size={16} />
              {tr("backport_verifying", undefined,
                "Launching and watching — a failed launch is retried, because launching is unreliable enough that one failure proves nothing.")}
            </div>
          ) : verdict?.kind === "running" ? (
            // Deliberately not "success". Thread count has been wrong three
            // times and a byte-identical control both passed and failed, so
            // the only trustworthy check is a human looking at the screen.
            <Callout tone="info" title={tr("backport_verify_running", undefined, "It is running — does it reach gameplay?")}>
              {tr("backport_verify_running_body", { threads: verdict.peakThreads },
                `The game is still up after a minute (${verdict.peakThreads} threads). That means it did not crash on load, but not that it plays — check the screen. Keep it if it works, or undo and try the next set.`)}
            </Callout>
          ) : verdict?.kind === "not-backported" ? (
            <Callout tone="warn" title={tr("backport_verify_unpatched", undefined, "This title is not backported")}>
              {tr("backport_verify_unpatched_body", undefined,
                "Its eboot still carries the original SDK version, so it cannot run on this firmware whatever libraries are installed — the library set is not the problem. Run the backport again; if it keeps happening the SDK patch is not taking.")}
            </Callout>
          ) : verdict?.kind === "missing-libraries" ? (
            <Callout tone="warn" title={tr("backport_verify_missing", undefined, "It needs more libraries")}>
              {tr("backport_verify_missing_body", { count: nextCandidates.length },
                `The game called a function none of the installed libraries provide. ${nextCandidates.length} larger set(s) left to try.`)}
            </Callout>
          ) : verdict?.kind === "wrong-libraries" ? (
            <Callout tone="warn" title={tr("backport_verify_wrong", undefined, "Wrong libraries for this game")}>
              {tr("backport_verify_wrong_body", { count: nextCandidates.length },
                `The game started and then stopped, with no missing-function error — the libraries are present but not the ones it needs. ${nextCandidates.length} other set(s) left to try.`)}
            </Callout>
          ) : (
            <Callout tone="warn" title={tr("backport_verify_unknown", undefined, "Could not tell whether this worked")}>
              {tr("backport_verify_unknown_body", undefined,
                "The launches did not agree, so this says nothing about the libraries — launching fails on its own often enough that a mixed result is meaningless. Try launching it yourself, and undo if it does not play.")}
            </Callout>
          )
        ) : null}
        {record ? (
          <>
            <Callout tone={record.complete ? "success" : "warn"} title={record.complete ? tr("backport_installed", undefined, "Backport files are installed") : tr("backport_partial", undefined, "Backport stopped partway")}>
              {record.complete
                ? tr("backport_installed_body", { set: record.setId }, `Installed the library set from ${record.setId}. The payload overlay mounts it when the game launches. If the game does not reach gameplay, Undo and try the next set.`)
                : tr("backport_partial_body", undefined, "Undo now to restore the original SDK and libraries before trying another set.")}
            </Callout>
            <div className="flex gap-2">
              {record.complete ? <Button variant="primary" leftIcon={<Play size={15} />} onClick={() => onLaunch(title)} disabled={!overlayReady}>{tr("backport_launch", undefined, "Launch")}</Button> : null}
              <Button variant="secondary" leftIcon={<RotateCcw size={15} />} onClick={() => void undo()} loading={busy}>{tr("backport_undo", undefined, "Undo")}</Button>
            </div>
          </>
        ) : null}
        {overlay?.state === "blocked" ? <div className="flex gap-2 text-xs text-[var(--color-warn)]"><AlertTriangle size={14} />{tr("backport_stop_external", undefined, "Stop the external BackPork payload, then resend ps5upload.")}</div> : null}
      </div>
    </Modal>
  );
}
