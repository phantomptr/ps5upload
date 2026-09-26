// The rows Convert's progress card shows for a pipeline, and one overall fraction for them.
// Pure: the card renders whatever this returns.

import type { Pipeline, PipelineMode, PipelineStage } from "../../state/fpkgConversion";
import type { Task } from "../../state/tasks";

export type RowState = "pending" | "active" | "done" | "failed";

export interface StageRow {
  stage: PipelineStage;
  state: RowState;
  /** How long a finished stage took. */
  ms?: number;
  /** The active stage's own progress, when it reports bytes. */
  done?: number;
  total?: number;
}

const BUILD: readonly PipelineStage[] = ["check", "plan", "compress", "write", "verify"];
const INSTALL: readonly PipelineStage[] = ["send", "install"];

/** Share of the whole run each stage stands for (compressing dominates a build). */
const WEIGHT: Record<PipelineStage, number> = {
  check: 2,
  plan: 2,
  compress: 55,
  write: 20,
  verify: 6,
  send: 10,
  install: 5,
};

function stagesFor(mode: PipelineMode): readonly PipelineStage[] {
  switch (mode) {
    case "convert-install":
      return [...BUILD, ...INSTALL];
    case "install":
      return INSTALL;
    default:
      return BUILD;
  }
}

/** Sending while the console is still pulling the package; installing once it has it all. */
function installStage(task: Task | null): PipelineStage {
  const p = task?.progress;
  return p && p.total > 0 && p.current >= p.total ? "install" : "send";
}

export function stageRows(p: Pipeline, installTask: Task | null): StageRow[] {
  if (p.phase === "idle") return [];
  const stages = stagesFor(p.mode);
  const withMs = (stage: PipelineStage, state: RowState): StageRow =>
    p.stageMs[stage] !== undefined ? { stage, state, ms: p.stageMs[stage] } : { stage, state };
  if (p.phase === "done") return stages.map((s) => withMs(s, "done"));
  if (p.phase === "failed") {
    const at = stages.indexOf(p.stage);
    return stages.map((s, i) => withMs(s, i < at ? "done" : i === at ? "failed" : "pending"));
  }
  const current = INSTALL.includes(p.stage) ? installStage(installTask) : p.stage;
  const at = stages.indexOf(current);
  return stages.map((s, i) => {
    if (i < at) return withMs(s, "done");
    if (i > at) return { stage: s, state: "pending" };
    if (s === "send" && installTask?.progress) {
      return { stage: s, state: "active", done: installTask.progress.current, total: installTask.progress.total };
    }
    if (BUILD.includes(s) && p.stageTotal > 0) {
      return { stage: s, state: "active", done: p.stageDone, total: p.stageTotal };
    }
    return { stage: s, state: "active" };
  });
}

/** 0..1 across the rows, each stage weighted by its share of the work. */
export function overallProgress(rows: StageRow[]): number {
  const total = rows.reduce((sum, r) => sum + WEIGHT[r.stage], 0);
  if (total === 0) return 0;
  const done = rows.reduce((sum, r) => {
    if (r.state === "done") return sum + WEIGHT[r.stage];
    if (r.state === "active" && r.total) return sum + WEIGHT[r.stage] * Math.min(1, (r.done ?? 0) / r.total);
    return sum;
  }, 0);
  return done / total;
}
