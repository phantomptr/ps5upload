// Card ③ Build & install: the start buttons, then the stage list while a run goes, then the
// result with the actions that fit it. Every action targets the package the pipeline names.

import { useEffect, useState } from "react";

import { CheckCircle2, Circle, Loader2, XCircle } from "lucide-react";

import { Button, Card, ProgressBar } from "../../components";
import type { Pipeline, PipelineStage } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";
import type { Task } from "../../state/tasks";
import { overallProgress, stageRows, type StageRow } from "./stages";

export function prettyBytes(n: number): string {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(2)} GiB`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(1)} MiB`;
  if (n >= 1 << 10) return `${(n / (1 << 10)).toFixed(0)} KiB`;
  return `${n} B`;
}

export function prettyDuration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ${s % 60} s`;
  return `${Math.floor(m / 60)} h ${m % 60} min`;
}

/** Bytes per second over `ms`, or 0 when too little time has passed to say. */
export function rateOf(bytes: number, ms: number): number {
  return ms >= 1000 ? (bytes * 1000) / ms : 0;
}

/** The delete button's label: the first press arms it, the second confirms. */
export function deleteLabel(armed: boolean): string {
  return armed ? "Confirm delete" : "Delete package";
}

const LABEL: Record<PipelineStage, [string, string]> = {
  copy: ["fpkg.stage.copy", "Copy from server"],
  check: ["fpkg.stage.check", "Check source"],
  plan: ["fpkg.stage.plan", "Plan package"],
  compress: ["fpkg.stage.compress", "Compress"],
  write: ["fpkg.stage.write", "Write package"],
  verify: ["fpkg.stage.verify", "Verify"],
  send: ["fpkg.stage.send", "Send to PS5"],
  install: ["fpkg.stage.install", "Install on PS5"],
};

export interface RunCardProps {
  pipeline: Pipeline;
  /** The stream-install task, while one runs. */
  installTask: Task | null;
  host: string;
  /** A console is connected and ready for an install. */
  canInstall: boolean;
  /** A checked source is ready to convert (false while none is chosen). */
  canConvert?: boolean;
  /** The source is an .exfat / .ffpkg image (it can also become a .ffpfsc). */
  isImage: boolean;
  deleteArmed: boolean;
  /** The game's title, for the result line. */
  title?: string | null;
  /** The game's size, for the package's share of it. */
  sourceBytes?: number;
  onConvert: () => void;
  onConvertInstall: () => void;
  onCompress: () => void;
  onCancel: () => void;
  onRetryInstall: () => void;
  onLaunch: () => void;
  onShowFolder: () => void;
  onInstallAgain: () => void;
  onDelete: () => void;
  onAnother: () => void;
}

function RowIcon({ state }: { state: StageRow["state"] }) {
  switch (state) {
    case "done":
      return <CheckCircle2 size={16} className="text-[var(--color-good)]" aria-hidden />;
    case "active":
      return <Loader2 size={16} className="animate-spin text-[var(--color-accent)]" aria-hidden />;
    case "failed":
      return <XCircle size={16} className="text-[var(--color-bad)]" aria-hidden />;
    default:
      return <Circle size={16} className="text-[var(--color-muted)]" aria-hidden />;
  }
}

/** The current time, ticking once a second while `live` (speeds and times left move). */
function useNow(live: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [live]);
  return now;
}

export function RunCard(props: RunCardProps) {
  const tr = useTr();
  const { pipeline: p, installTask } = props;
  const now = useNow(p.phase === "running");
  const rows = stageRows(p, installTask);
  const label = (s: PipelineStage) => tr(LABEL[s][0], undefined, LABEL[s][1]);

  /** " · 96 MiB/s · 21 min left" for the active row: the install task's figures while sending,
   *  else measured from the stage's own bytes since it began. */
  const speedAndEta = (r: StageRow): string => {
    const remaining = (r.total ?? 0) - (r.done ?? 0);
    let rate = 0;
    let eta = 0;
    if (r.stage === "send") {
      rate = installTask?.rate?.bytesPerSec ?? 0;
      eta = (installTask?.eta ?? 0) * 1000;
    } else if (p.phase === "running") {
      rate = rateOf(r.done ?? 0, now - p.stageStartedMs);
      eta = rate > 0 ? (remaining / rate) * 1000 : 0;
    }
    return (
      (rate > 0 ? ` · ${prettyBytes(rate)}/s` : "") +
      (eta > 0 ? ` · ${tr("fpkg.timeLeft", { time: prettyDuration(eta) }, "{time} left")}` : "")
    );
  };

  const title = (
    <div className="text-sm font-medium">
      {tr("fpkg.card.run", undefined, "③ Build & install")}
    </div>
  );

  if (p.phase === "idle") {
    return (
      <Card>
        <div className="flex flex-col gap-2">
          {title}
          <div className="flex flex-wrap gap-2">
            <Button
              variant="primary"
              onClick={props.onConvertInstall}
              disabled={!props.canInstall || props.canConvert === false}
            >
              {tr("fpkg.convertInstall", undefined, "Convert & install")}
            </Button>
            <Button onClick={props.onConvert} disabled={props.canConvert === false}>
              {tr("fpkg.convertOnly", undefined, "Convert only")}
            </Button>
            {props.isImage && (
              <Button variant="ghost" onClick={props.onCompress}>
                {tr("fpkg.compress", undefined, "Compress to .ffpfsc")}
              </Button>
            )}
          </div>
          {!props.canInstall && (
            <div className="text-xs text-[var(--color-muted)]">
              {tr("fpkg.needConsole", undefined, "Connect to a PS5 to install.")}
            </div>
          )}
        </div>
      </Card>
    );
  }

  const stageList = (
    <ol className="flex flex-col gap-1.5 text-sm">
      {rows.map((r) => (
        <li key={r.stage} className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <RowIcon state={r.state} />
            <span className={r.state === "active" ? "font-medium" : ""}>{label(r.stage)}</span>
            {r.ms !== undefined && r.state !== "active" && (
              <span className="text-xs text-[var(--color-muted)]">{prettyDuration(r.ms)}</span>
            )}
            {r.state === "active" && r.total ? (
              <span className="text-xs text-[var(--color-muted)]">
                {prettyBytes(r.done ?? 0)} / {prettyBytes(r.total)}
                {speedAndEta(r)}
              </span>
            ) : null}
          </div>
          {r.state === "active" && (
            <div className="pl-6">
              <ProgressBar
                value={r.total ? (r.done ?? 0) / r.total : null}
                label={label(r.stage)}
              />
            </div>
          )}
        </li>
      ))}
    </ol>
  );

  if (p.phase === "running") {
    const building = !["send", "install"].includes(p.stage);
    return (
      <Card>
        <div className="flex flex-col gap-3">
          {title}
          {stageList}
          <ProgressBar
            value={overallProgress(rows)}
            label={tr("fpkg.overall", undefined, "Overall")}
          />
          <div className="flex items-center justify-between text-xs text-[var(--color-muted)]">
            <span>
              {Math.round(overallProgress(rows) * 100)}%
              {(() => {
                const f = overallProgress(rows);
                const elapsed = now - p.startedMs;
                return f > 0.02 && elapsed > 5000
                  ? ` · ${tr("fpkg.timeLeft", { time: prettyDuration((elapsed / f) * (1 - f)) }, "{time} left")}`
                  : "";
              })()}
            </span>
            {building && p.jobId && (
              <Button variant="danger" onClick={props.onCancel}>
                {tr("fpkg.cancel", undefined, "Cancel")}
              </Button>
            )}
          </div>
        </div>
      </Card>
    );
  }

  if (p.phase === "failed") {
    return (
      <Card>
        <div className="flex flex-col gap-3">
          {title}
          {stageList}
          <div className="text-sm text-[var(--color-bad)]">
            {label(p.stage)}: {p.message}
          </div>
          {p.packagePath && (
            <div className="text-sm text-[var(--color-muted)]">
              {tr("fpkg.kept", { path: p.packagePath }, "The package was built and kept: {path}")}
            </div>
          )}
          <div className="flex flex-wrap gap-2">
            {p.packagePath && (
              <Button variant="primary" onClick={props.onRetryInstall} disabled={!props.canInstall}>
                {tr("fpkg.retryInstall", undefined, "Retry install")}
              </Button>
            )}
            <Button onClick={props.onAnother}>
              {tr("fpkg.another", undefined, "＋ Convert another game")}
            </Button>
          </div>
        </div>
      </Card>
    );
  }

  const installed = p.mode === "convert-install" || p.mode === "install";
  return (
    <Card>
      <div className="flex flex-col gap-3">
        {title}
        {stageList}
        <div className="text-sm font-medium text-[var(--color-good)]">
          {installed
            ? props.title
              ? tr("fpkg.installedTitle", { title: props.title }, "Installed on PS5 — {title}")
              : tr("fpkg.installedOn", { host: p.host ?? "" }, "Installed on the PS5 ({host})")
            : p.mode === "ffpfsc"
              ? tr("fpkg.compressed", undefined, "Compressed image written and verified")
              : tr("fpkg.done", undefined, "Package written")}
        </div>
        <div className="text-sm text-[var(--color-muted)]">
          {p.packageBytes > 0 && <span>{prettyBytes(p.packageBytes)}</span>}
          {p.packageBytes > 0 && props.sourceBytes ? (
            <span>
              {" "}
              {tr(
                "fpkg.ratio",
                {
                  pct: Math.round((p.packageBytes / props.sourceBytes) * 100),
                  size: prettyBytes(props.sourceBytes),
                },
                "({pct}% of {size})",
              )}
            </span>
          ) : null}
          {p.convertMs > 0 && (
            <span>
              {" · "}
              {tr("fpkg.convertedIn", { time: prettyDuration(p.convertMs) }, "converted in {time}")}
            </span>
          )}
          {p.installMs > 0 && (
            <span>
              {" · "}
              {tr("fpkg.installedIn", { time: prettyDuration(p.installMs) }, "installed in {time}")}
            </span>
          )}
        </div>
        <div className="break-all text-sm text-[var(--color-text)]">
          {p.deleted ? tr("fpkg.deleted", undefined, "Package deleted") : p.packagePath}
        </div>
        <div className="flex flex-wrap gap-2">
          {installed && !p.deleted && p.host && (
            <Button variant="primary" onClick={props.onLaunch}>
              {tr("fpkg.launch", undefined, "Launch on PS5")}
            </Button>
          )}
          {!p.deleted && (
            <Button onClick={props.onShowFolder}>{tr("fpkg.showFolder", undefined, "Show in folder")}</Button>
          )}
          {!p.deleted && p.mode !== "ffpfsc" && (
            <Button onClick={props.onInstallAgain} disabled={!props.canInstall}>
              {tr("fpkg.installAgain", undefined, "Install again")}
            </Button>
          )}
          {!p.deleted && p.mode !== "ffpfsc" && (
            <Button variant="danger" onClick={props.onDelete}>
              {props.deleteArmed
                ? tr("fpkg.confirmDelete", undefined, deleteLabel(true))
                : tr("fpkg.deletePackage", undefined, deleteLabel(false))}
            </Button>
          )}
          <Button onClick={props.onAnother}>
            {tr("fpkg.another", undefined, "＋ Convert another game")}
          </Button>
        </div>
      </div>
    </Card>
  );
}
