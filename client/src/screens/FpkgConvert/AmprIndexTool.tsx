// Workshop's "Rebuild AMPR index" (#621): a libSceAmpr game's ampr_emu.index regenerated from
// its files without converting it. ampr_emu trusts the index's sizes, so a stale one (a folder
// trimmed, patched or dumped again) breaks the game; a package or image Workshop builds always
// carries a fresh one, this is for a game played as it is.
//
// - A folder here or on the PS5: the index is rewritten in place, the game's own kept once as
//   ampr_emu.index.bak.
// - An uncompressed image here: an edited copy with the new index (the image is never changed).
// - A .ffpfsc is compressed and read-only: converting it again makes a fresh index.

import { useEffect, useRef, useState } from "react";
import { ListRestart } from "lucide-react";

import {
  fpkg,
  type AmprIndexOutcome,
  type FpkgInspection,
} from "../../api/fpkg";
import { jobCancel, jobStatus, type JobSnapshot } from "../../api/ps5";
import { Button } from "../../components";
import { isRemotePath } from "../../lib/remotePath";
import { isArchiveSource } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";

/** What the action does for a source, or why it is not offered. */
export type AmprIndexPlan =
  /** Not shown: no game checked, or a package or archive (a build makes a fresh index). */
  | "hidden"
  /** The game does not read through libSceAmpr: shown off, with why. */
  | "not_ampr"
  | "folder"
  | "console_folder"
  /** An uncompressed image on this computer: an edited copy. */
  | "image"
  /** An image on the PS5: it has to be here for an edited copy. */
  | "console_image"
  /** A .ffpfsc: converted again, never edited. */
  | "compressed";

const IMAGE = /\.(exfat|ffpkg|ufs2|ffpfs)$/i;

/** What "Rebuild AMPR index" does for `source`, as checked under ① (`inspection`): offered
 *  for a game whose eboot.bin imports libSceAmpr, the test a build uses (the check's
 *  `ampr_packs` says `no_ampr` otherwise). */
export function amprIndexPlan(
  source: string,
  inspection: FpkgInspection | null,
): AmprIndexPlan {
  const s = source.trim();
  if (!s || !inspection) return "hidden";
  if (isArchiveSource(s) || /\.pkg$/i.test(s) || isRemotePath(s))
    return "hidden";
  if (!inspection.ampr_packs || inspection.ampr_packs.kind === "no_ampr")
    return "not_ampr";
  const onConsole = s.startsWith("ps5://");
  if (/\.ffpfsc$/i.test(s)) return "compressed";
  if (IMAGE.test(s)) return onConsole ? "console_image" : "image";
  return onConsole ? "console_folder" : "folder";
}

export type AmprIndexRun =
  | { phase: "idle" }
  | { phase: "running"; jobId: string | null; stage?: string; pct?: number }
  | { phase: "done"; outcome?: AmprIndexOutcome; dest: string }
  | { phase: "failed"; message: string };

const POLL_MS = 500;

type Tr = ReturnType<typeof useTr>;

const stageText = (tr: Tr, id?: string) =>
  id === "list"
    ? tr("ampr_index_stage_list", undefined, "Listing the game's files…")
    : id === "check"
      ? tr("ampr_index_stage_check", undefined, "Making the index…")
      : id === "write"
        ? tr("ampr_index_stage_write", undefined, "Writing it…")
        : tr("ampr_index_running", undefined, "Rebuilding the AMPR index…");

/** The line under the button: why it is off, what it is doing, or what it did. */
export function amprIndexStatus(
  plan: AmprIndexPlan,
  run: AmprIndexRun,
  explain: boolean,
  tr: Tr,
): string | null {
  if (plan === "not_ampr")
    return tr(
      "ampr_index_not_ampr",
      undefined,
      "Only for a game whose eboot.bin reads its files through libSceAmpr. This one does not.",
    );
  if (explain && plan === "compressed")
    return tr(
      "ampr_index_compressed",
      undefined,
      "This image is compressed and read-only: rebuild the index by converting it again (Workshop › Convert).",
    );
  if (explain && plan === "console_image")
    return tr(
      "ampr_index_console_image",
      undefined,
      "This image is on the PS5. Copy it to this computer first: the new index goes into an edited copy.",
    );
  if (run.phase === "running")
    return `${stageText(tr, run.stage)}${run.pct !== undefined ? ` ${run.pct}%` : ""}`;
  if (run.phase === "failed") return run.message;
  if (run.phase === "done") {
    if (plan === "image")
      return tr(
        "ampr_index_image_done",
        { path: run.dest },
        "Written to {path}, with the new index. The image you opened is unchanged.",
      );
    const o = run.outcome;
    const files = o?.files ?? 0;
    const head = !o?.had_index
      ? tr(
          "ampr_index_done_new",
          { count: files },
          "Rebuilt: {count} files. The game had no index before.",
        )
      : o.differed
        ? tr(
            "ampr_index_done_differed",
            { count: files },
            "Rebuilt: {count} files. The old index was different.",
          )
        : tr(
            "ampr_index_done_same",
            { count: files },
            "Rebuilt: {count} files. The old index was already right.",
          );
    return o?.backup
      ? `${head} ${tr("ampr_index_backup", undefined, "The old one is kept as ampr_emu.index.bak.")}`
      : head;
  }
  return null;
}

export function AmprIndexTool({
  source,
  inspection,
  locked,
  onConvertAgain,
  onRebuilt,
}: {
  source: string;
  inspection: FpkgInspection | null;
  /** A build is running: one job at a time on this screen. */
  locked: boolean;
  /** Make a fresh .ffpfsc of this game (Workshop › Convert). */
  onConvertAgain: () => void;
  /** A folder's index was rewritten: check the game again. */
  onRebuilt: () => void;
}) {
  const tr = useTr();
  const plan = amprIndexPlan(source, inspection);
  const [run, setRun] = useState<AmprIndexRun>({ phase: "idle" });
  const [explain, setExplain] = useState(false);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  // Another game: the last result was about the previous one.
  useEffect(() => {
    setRun({ phase: "idle" });
    setExplain(false);
  }, [source]);

  if (plan === "hidden") return null;

  const follow = async (jobId: string, isImage: boolean) => {
    setRun({ phase: "running", jobId });
    for (;;) {
      await new Promise((r) => setTimeout(r, POLL_MS));
      if (!live.current) return;
      let snap: JobSnapshot;
      try {
        snap = await jobStatus(jobId);
      } catch (e) {
        setRun({
          phase: "failed",
          message: e instanceof Error ? e.message : String(e),
        });
        return;
      }
      if (!live.current) return;
      if (snap.status === "running") {
        const st = snap.stage;
        setRun({
          phase: "running",
          jobId,
          stage: isImage ? undefined : st?.id,
          pct:
            st && st.total > 0
              ? Math.min(100, Math.round((st.done / st.total) * 100))
              : undefined,
        });
        continue;
      }
      if (snap.status === "done") {
        const outcome = isImage
          ? undefined
          : (snap.commit_ack as unknown as AmprIndexOutcome | undefined);
        setRun({ phase: "done", outcome, dest: snap.dest ?? "" });
        if (!isImage) onRebuilt();
        return;
      }
      setRun({
        phase: "failed",
        message:
          snap.error ??
          tr("ampr_index_failed", undefined, "The index was not rebuilt."),
      });
      return;
    }
  };

  const start = async () => {
    setExplain(false);
    if (
      plan === "compressed" ||
      plan === "console_image" ||
      plan === "not_ampr"
    ) {
      setExplain(true);
      return;
    }
    setRun({ phase: "running", jobId: null });
    try {
      const isImage = plan === "image";
      const { job_id } = isImage
        ? await fpkg.rebuildImageAmprIndex(source.trim())
        : await fpkg.rebuildAmprIndex(source.trim());
      await follow(job_id, isImage);
    } catch (e) {
      setRun({
        phase: "failed",
        message: e instanceof Error ? e.message : String(e),
      });
    }
  };

  const cancel = () => {
    if (run.phase === "running" && run.jobId)
      void jobCancel(run.jobId).catch(() => {});
  };

  const label = tr("ampr_index_rebuild", undefined, "Rebuild AMPR index");
  const running = run.phase === "running";
  const result = amprIndexStatus(plan, run, explain, tr);

  return (
    <>
      <Button
        size="sm"
        variant="secondary"
        leftIcon={<ListRestart size={14} />}
        data-testid="ampr-index-rebuild"
        disabled={plan === "not_ampr" || locked || running}
        title={tr(
          "ampr_index_hint",
          undefined,
          "Make ampr_emu.index again from the game's files, for a game played as it is. A package or image Workshop builds already carries a fresh one.",
        )}
        onClick={() => void start()}
      >
        {label}
      </Button>
      {result && (
        <div
          className={`flex w-full flex-wrap items-center gap-2 text-xs ${
            run.phase === "failed"
              ? "text-[var(--color-bad)]"
              : "text-[var(--color-muted)]"
          }`}
          data-testid="ampr-index-status"
          role="status"
        >
          <span>{result}</span>
          {running && run.jobId && (
            <Button size="sm" variant="ghost" onClick={cancel}>
              {tr("cancel", undefined, "Cancel")}
            </Button>
          )}
          {explain && plan === "compressed" && (
            <Button
              size="sm"
              variant="secondary"
              disabled={locked}
              onClick={onConvertAgain}
            >
              {tr("ampr_index_convert_again", undefined, "Convert again")}
            </Button>
          )}
        </div>
      )}
    </>
  );
}
