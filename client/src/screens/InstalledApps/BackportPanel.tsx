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
import type { ScanTitleInput } from "../../state/fakelibCorpus";
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
  type FakelibSet,
} from "../../lib/backport";
import {
  loadBackportRecord,
  removeBackportRecord,
  saveBackportRecord,
} from "../../state/backportRecords";

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
  const [record, setRecord] = useState<BackportRecord | null>(() =>
    loadBackportRecord(host, title.titleId),
  );
  const [patchLibc, setPatchLibc] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const candidates = rankSets(sets, rejected, title.titleId);

  const load = useCallback(async () => {
    if (!open || record) return;
    const candidate = rankSets(sets, rejected, title.titleId)[0];
    if (!candidate) {
      setPlan(null);
      // An empty corpus is not a failure — it is a new user, and the panel's
      // job is to offer the two ways to fill it. Only "tried everything" is a
      // dead end worth wording as one.
      setError(sets.length === 0
        ? null
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
  }, [corpusError, host, open, corpusRoot, sets, record, rejected, title, titleSdkVersion, tr]);

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
