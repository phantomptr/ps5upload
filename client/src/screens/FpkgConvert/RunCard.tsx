// Card ③ Build & install: the one set of choices (what to make, what happens after) that both
// Convert and the queue use, with the start buttons; then the stage list while a run goes, then
// the result with the actions that fit it. Every action targets the package the pipeline names.

import { useEffect, useState } from "react";

import { CheckCircle2, Circle, ListPlus, Loader2, XCircle } from "lucide-react";

import { Button, Card, Checkbox, ProgressBar, Toggle } from "../../components";
import type { ConvertThen } from "../../state/convertQueue";
import type { MakeKind } from "../../state/convertPrefs";
import { makesFolder, makesImage } from "../../state/fpkgConversion";
import type { InstallMethod, Pipeline, PipelineStage } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";
import { formatBytes } from "../../lib/format";
import type { Task } from "../../state/tasks";
import type { ImageFormat } from "../../api/fpkg";
import { overallProgress, stageRows, type StageRow } from "./stages";
import { lz4Active, type Lz4Choice } from "./ImageOptions";

export { lz4Active, type Lz4Choice } from "./ImageOptions";

/** The app's one size formatter (IEC), kept under this name for the Convert cards. */
export const prettyBytes = formatBytes;

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
  extract: ["fpkg.stage.extract", "Unpack archive"],
  check: ["fpkg.stage.check", "Check source"],
  pack: ["fpkg.stage.pack", "Pack LZ4 assets"],
  plan: ["fpkg.stage.plan", "Plan package"],
  compress: ["fpkg.stage.compress", "Compress"],
  write: ["fpkg.stage.write", "Write package"],
  verify: ["fpkg.stage.verify", "Verify"],
  park: ["fpkg.stage.park", "Set the dump aside"],
  send: ["fpkg.stage.send", "Send to PS5"],
  install: ["fpkg.stage.install", "Install on PS5"],
};

/** An LZ4 image build (its pack stage ran) that came out no smaller than the source: the
 *  game's data was already compressed. Said, rather than leaving "101% of" unexplained. */
export function lz4SavedNothing(
  p: { mode: string; packageBytes: number; stageMs: Partial<Record<string, number>> },
  sourceBytes: number | undefined,
): boolean {
  return (
    p.mode === "image" &&
    p.stageMs.pack !== undefined &&
    !!sourceBytes &&
    p.packageBytes >= sourceBytes * 0.99
  );
}

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
  /** What to make and what happens after: the one choice Convert and the queue both use. */
  choice: OutputChoiceProps;
  /** Convert the current game as chosen. */
  onRun: () => void;
  /** The source is a game folder on this computer, so it can become an image here. */
  canImage?: boolean;
  /** The source is a game image or a package, so it can become a plain folder. */
  canFolder?: boolean;
  /** The source is a folder already: there is nothing to unpack. */
  isFolderSource?: boolean;
  /** One line saying what the run makes, of which game, where (from ① to ③). */
  summary?: string;
  /** Add the current game to the queue, with the same choice. */
  onQueue?: () => void;
  canQueue?: boolean;
  onCompress: () => void;
  onCancel: () => void;
  /** Install the kept package: streamed from this computer, or uploaded to the PS5 first. */
  onInstall: (method: InstallMethod) => void;
  onLaunch: () => void;
  /** Absent when no file manager can open the output (browser build, or an engine on
   *  another machine). */
  onShowFolder?: () => void;
  onDelete: () => void;
  onAnother: () => void;
  /** The source is a dump on the console: installing swaps it out (Convert & replace). */
  replaces?: boolean;
  /** Open the built package in the viewer. */
  onViewPackage?: () => void;
  /** After a swap: delete the set-aside dump, or keep it. */
  onFinishReplace?: (choice: "delete" | "keep") => void;
  /** A finished image: put it in the Upload queue (`deleteAfter`: remove it here once sent). */
  onUploadImage?: (deleteAfter: boolean) => void;
  /** A finished image already in the Upload queue: open Upload to see it. */
  onOpenUpload?: () => void;
  /** The console a Convert & install run installs on, by name (recorded when it started). */
  targetLabel?: string | null;
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

type Tr = ReturnType<typeof useTr>;

/** The idle card's main button: what it says and whether it can be pressed. Shared with the
 *  Workshop's bottom bar, which offers the same button while ④ is scrolled out of view. */
export function idleRun(
  props: Pick<RunCardProps, "choice" | "canConvert" | "canFolder" | "canImage" | "replaces">,
  tr: Tr,
): { label: string; canRun: boolean } {
  const c = props.choice;
  const image = c.make === "image";
  const folder = c.make === "folder";
  const packing = image && lz4Active(c.imageFormat, c.lz4);
  const sends = c.then !== "keep";
  const canRun = props.canConvert !== false && (folder ? !!props.canFolder : !image || !!props.canImage);
  const label = folder
    ? tr("fpkg.folder.make", undefined, "Make game folder")
    : image
      ? c.imageCompress && !packing
        ? tr("fpkg.image.make_compressed", undefined, "Make compressed image")
        : tr("fpkg.image.make", undefined, "Make game image")
      : !sends
        ? tr("fpkg.convertOnly", undefined, "Convert only")
        : props.replaces
          ? tr("fpkg.convertReplace", undefined, "Convert & replace")
          : tr("fpkg.convertInstall", undefined, "Convert & install");
  return { label, canRun };
}

export function RunCard(props: RunCardProps) {
  const tr = useTr();
  const { pipeline: p, installTask } = props;
  const now = useNow(p.phase === "running");
  const rows = stageRows(p, installTask);
  // An image build plans, writes and verifies an image, not a package.
  const image = p.phase !== "idle" && makesImage(p.mode);
  const folder = p.phase !== "idle" && makesFolder(p.mode);
  const label = (s: PipelineStage) =>
    folder && s === "write"
      ? tr("fpkg.stage.write_folder", undefined, "Write folder")
      : folder && s === "verify"
        ? tr("fpkg.stage.verify_folder", undefined, "Read the folder back")
        : image && s === "plan"
      ? tr("fpkg.stage.plan_image", undefined, "Plan image")
      : image && s === "write"
        ? tr("fpkg.stage.write_image", undefined, "Write image")
        : image && s === "verify"
          ? tr("fpkg.stage.verify_image", undefined, "Read the image back")
          : tr(LABEL[s][0], undefined, LABEL[s][1]);

  /** " · 96 MiB/s · 21 min left" for the active row: the install task's figures while sending,
   *  else measured from the stage's own bytes since it began. */
  const speedAndEta = (r: StageRow): string => {
    const remaining = (r.total ?? 0) - (r.done ?? 0);
    let rate = 0;
    let eta = 0;
    if (r.stage === "send" && installTask) {
      rate = installTask.rate?.bytesPerSec ?? 0;
      eta = (installTask.eta ?? 0) * 1000;
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
      {tr("fpkg.card.run_v2", undefined, "④ Build & install")}
    </div>
  );

  if (p.phase === "idle") {
    const c = props.choice;
    const image = c.make === "image";
    const folder = c.make === "folder";
    const sends = c.then !== "keep";
    const { label: runLabel, canRun } = idleRun(props, tr);
    return (
      <Card padded={false} className="p-3.5 sm:p-4">
        <div className="flex flex-col gap-2.5">
          {title}
          {props.summary && (
            <div className="text-xs text-[var(--color-muted)]" data-testid="run-summary">
              {props.summary}
            </div>
          )}
          <OutputChoice {...c} />
          <div className="flex flex-wrap gap-2 border-t border-[var(--color-border)] pt-3">
            <Button variant="primary" onClick={props.onRun} disabled={!canRun}>
              {runLabel}
            </Button>
            {props.onQueue && (
              <Button leftIcon={<ListPlus size={14} />} onClick={props.onQueue} disabled={!props.canQueue}>
                {tr("cq_add_current", undefined, "Add this game to the queue")}
              </Button>
            )}
            {props.isImage && (
              <Button variant="ghost" onClick={props.onCompress}>
                {tr("fpkg.compress", undefined, "Compress to .ffpfsc")}
              </Button>
            )}
          </div>
          {image && !props.canImage && props.canConvert !== false && (
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "fpkg.image.folderOnly_v4",
                undefined,
                "Only a game folder, game image or archive (.zip, .7z, .rar) with a game in it can become a game image; an archive is unpacked into the output folder first.",
              )}
            </div>
          )}
          {folder && props.isFolderSource && (
            <div className="text-xs text-[var(--color-muted)]" data-testid="folder-already">
              {tr("fpkg.folder.already", undefined, "This is already a folder")}
            </div>
          )}
          {folder && !props.isFolderSource && !props.canFolder && props.canConvert !== false && (
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "fpkg.folder.sourceOnly",
                undefined,
                "Only a game image or a package made with the debug key can become a folder; unpack an archive first.",
              )}
            </div>
          )}
          {props.replaces && props.canInstall && !image && sends && (
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "fpkg.replaceHint",
                undefined,
                "The dump on the PS5 is set aside while its package installs, and put back if the install fails. Saves are kept.",
              )}
            </div>
          )}
          {!props.canInstall && !folder && (
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

  /** The two ways to install the kept package, with what sets them apart. */
  const installChoice = (primary: boolean) => (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap gap-2">
        <Button
          variant={primary ? "primary" : undefined}
          onClick={() => props.onInstall("stream")}
          disabled={!props.canInstall}
        >
          {tr("pkglib.streamInstall", undefined, "Stream & install")}
        </Button>
        <Button onClick={() => props.onInstall("upload")} disabled={!props.canInstall}>
          {tr("fpkg.uploadInstall", undefined, "Upload & install")}
        </Button>
      </div>
      <div className="text-xs text-[var(--color-muted)]">
        {tr(
          "fpkg.installRoutes",
          undefined,
          "Stream install sends the package straight from this computer. Upload & install copies it to the PS5 first — use it when a stream install can't reach this computer.",
        )}
      </div>
    </div>
  );

  if (p.phase === "running") {
    const building = !["send", "install"].includes(p.stage);
    return (
      <Card padded={false} className="p-3.5 sm:p-4">
        <div className="flex flex-col gap-3">
          {title}
          {p.mode === "convert-install" && props.targetLabel && (
            <div className="self-start rounded-full border border-[var(--color-border-strong)] px-3 py-1 text-xs text-[var(--color-muted)]">
              {tr("fpkg.willInstallOn", { console: props.targetLabel }, "Will install on {console}")}
            </div>
          )}
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
      <Card padded={false} className="p-3.5 sm:p-4">
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
          {p.packagePath && installChoice(true)}
          <div className="flex flex-wrap gap-2">
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
    <Card padded={false} className="p-3.5 sm:p-4">
      <div className="flex flex-col gap-3">
        {title}
        {stageList}
        <div className="text-sm font-medium text-[var(--color-good)]">
          {installed
            ? props.title
              ? tr("fpkg.installedTitle", { title: props.title }, "Installed on PS5 — {title}")
              : tr("fpkg.installedOn", { host: p.host ?? "" }, "Installed on the PS5 ({host})")
            : p.mode === "ffpfsc" || (p.mode === "image" && p.packagePath.endsWith(".ffpfsc"))
              ? tr("fpkg.compressed", undefined, "Compressed image written and verified")
              : p.mode === "image"
                ? tr(
                    "fpkg.image.done",
                    undefined,
                    "Game image written and read back. Copy it to a PS5 drive (Upload) and ShadowMount+ will mount it.",
                  )
                : folder
                  ? tr("fpkg.folder.done", undefined, "Game folder written and read back.")
                  : tr("fpkg.done", undefined, "Package written")}
        </div>
        <div className="text-sm text-[var(--color-muted)]">
          {p.packageBytes > 0 && <span>{prettyBytes(p.packageBytes)}</span>}
          {p.packageBytes > 0 && props.sourceBytes && !folder ? (
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
        {lz4SavedNothing(p, props.sourceBytes) && (
          <div className="text-sm text-[var(--color-warn)]">
            {tr(
              "fpkg.lz4_saved_nothing",
              undefined,
              "LZ4 found nothing to compress: this game's files are already compressed by its developer, so the image stays the size of the game. It works as usual.",
            )}
          </div>
        )}
        <div className="break-all text-sm text-[var(--color-text)]">
          {p.deleted ? tr("fpkg.deleted", undefined, "Package deleted") : p.packagePath}
        </div>
        {p.swap && (
          <div className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] p-3 text-sm">
            <span>
              {tr(
                "fpkg.swapDone",
                { path: p.swap.parked },
                "The old dump is set aside at {path}. Launch the game to check it runs, then delete the dump or keep it.",
              )}
            </span>
            <div className="flex flex-wrap gap-2">
              <Button variant="danger" size="sm" onClick={() => props.onFinishReplace?.("delete")}>
                {tr("fpkg.deleteDump", undefined, "Delete the old dump")}
              </Button>
              <Button size="sm" onClick={() => props.onFinishReplace?.("keep")}>
                {tr("fpkg.keepDump", undefined, "Keep it parked")}
              </Button>
            </div>
          </div>
        )}
        {!p.deleted && !makesImage(p.mode) && !folder && installChoice(!installed)}
        {!p.deleted && makesImage(p.mode) && (
          <ImageUpload
            queued={!!p.uploadQueued}
            canUpload={props.canInstall && !!props.onUploadImage}
            onUpload={(deleteAfter) => props.onUploadImage?.(deleteAfter)}
            onOpen={props.onOpenUpload}
          />
        )}
        <div className="flex flex-wrap gap-2">
          {installed && !p.deleted && p.host && (
            <Button variant="primary" onClick={props.onLaunch}>
              {tr("fpkg.launch", undefined, "Launch on PS5")}
            </Button>
          )}
          {!p.deleted && props.onShowFolder && (
            <Button onClick={props.onShowFolder}>{tr("fpkg.showFolder", undefined, "Show in folder")}</Button>
          )}
          {!p.deleted && !makesImage(p.mode) && !folder && props.onViewPackage && (
            <Button onClick={props.onViewPackage}>{tr("viewer_open", undefined, "View details")}</Button>
          )}
          {!p.deleted && !makesImage(p.mode) && !folder && (
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

/** A finished image: send it to the PS5 through the Upload queue. */
function ImageUpload({
  queued,
  canUpload,
  onUpload,
  onOpen,
}: {
  queued: boolean;
  canUpload: boolean;
  onUpload: (deleteAfter: boolean) => void;
  onOpen?: () => void;
}) {
  const tr = useTr();
  const [deleteAfter, setDeleteAfter] = useState(false);
  if (queued) {
    return (
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span className="text-[var(--color-good)]">
          {tr(
            "fpkg.imageQueued",
            undefined,
            "In the queue: it goes to the PS5 next, into the folder ShadowMount+ watches.",
          )}
        </span>
        {onOpen && (
          <Button size="sm" onClick={onOpen}>
            {tr("fpkg.openUpload_v2", undefined, "Open Upload & Install")}
          </Button>
        )}
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="primary" disabled={!canUpload} onClick={() => onUpload(deleteAfter)}>
          {tr("fpkg.uploadImage", undefined, "Upload to PS5")}
        </Button>
        <Checkbox
          checked={deleteAfter}
          onChange={setDeleteAfter}
          label={tr(
            "fpkg.uploadImageDelete",
            undefined,
            "Delete it from this computer once it is on the PS5",
          )}
        />
      </div>
      {!canUpload && (
        <span className="text-xs text-[var(--color-muted)]">
          {tr("fpkg.uploadNeedsPs5", undefined, "Connect to a PS5 to upload it.")}
        </span>
      )}
    </div>
  );
}

/** What Convert makes and what happens after: one set of choices, used by Convert and by
 *  the queue alike. What to make is chosen under ② and the image's own options under ③;
 *  this card asks what happens after, and runs. */
export interface OutputChoiceProps {
  make: MakeKind;
  onMake: (make: MakeKind) => void;
  /** The image picked (format, compressed into a .ffpfsc). */
  imageFormat: ImageFormat;
  imageCompress: boolean;
  onImageChoice: (format: ImageFormat, compress: boolean) => void;
  /** After converting, as it can be done now (see `thenFor`). */
  then: ConvertThen;
  onThen: (then: ConvertThen) => void;
  deleteAfter: boolean;
  onDeleteAfter: (on: boolean) => void;
  /** A console answers: installing or uploading is possible. */
  canInstall: boolean;
  /** For a libSceAmpr game: the AMPR LZ4 asset-pack option of an .exfat image. */
  lz4?: Lz4Choice;
}

function OutputChoice(c: OutputChoiceProps) {
  const tr = useTr();
  const image = c.make === "image";
  return (
    <div className="flex flex-col gap-3" data-testid="output-choice">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm">
        <label className="flex flex-wrap items-center gap-x-3 gap-y-2">
          <span className="min-w-28 text-[var(--color-muted)]">{tr("cq_after", undefined, "After converting")}</span>
          <select
            value={c.then}
            onChange={(e) => c.onThen(e.target.value as ConvertThen)}
            className="select w-auto!"
            data-testid="convert-then"
          >
            {c.make === "folder" ? (
              <option value="keep">{tr("cq_then_keep_folder", undefined, "Keep the folder")}</option>
            ) : image ? (
              <>
                <option value="keep">{tr("cq_then_keep_image", undefined, "Keep the image")}</option>
                <option value="upload" disabled={!c.canInstall}>
                  {tr("cq_then_upload_image", undefined, "Upload it to the PS5")}
                </option>
              </>
            ) : (
              <>
                <option value="keep">{tr("cq_then_keep", undefined, "Keep the package")}</option>
                <option value="stream" disabled={!c.canInstall}>
                  {tr("cq_then_stream", undefined, "Stream & install")}
                </option>
                <option value="upload" disabled={!c.canInstall}>
                  {tr("cq_then_upload", undefined, "Upload & install")}
                </option>
              </>
            )}
          </select>
        </label>
        {c.then !== "keep" && (
          <Toggle
            checked={c.deleteAfter}
            onChange={c.onDeleteAfter}
            label={
              image
                ? tr("cq_delete_after_image", undefined, "Delete the image once it is on the PS5")
                : tr("cq_delete_after", undefined, "Delete the package once installed")
            }
          />
        )}
      </div>
    </div>
  );
}

