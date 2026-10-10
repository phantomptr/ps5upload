// Card ③ Build & install: the start buttons, then the stage list while a run goes, then the
// result with the actions that fit it. Every action targets the package the pipeline names.

import { useEffect, useState } from "react";

import { CheckCircle2, Circle, Loader2, XCircle } from "lucide-react";

import { Button, Card, Checkbox, ProgressBar } from "../../components";
import { makesImage } from "../../state/fpkgConversion";
import type { InstallMethod, Pipeline, PipelineStage } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";
import type { Task } from "../../state/tasks";
import type { AmprPacksReadiness, ImageFormat } from "../../api/fpkg";
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
  /** The source is a game folder on this computer: write it as an image (`compress`: then
   *  compress it into a .ffpfsc). Absent when it cannot be made into one from here. */
  onMakeImage?: (compress: boolean, format: ImageFormat) => void;
  /** The image picked (format, compressed): also what the queue makes when it makes images. */
  imageFormat?: ImageFormat;
  imageCompress?: boolean;
  onImageChoice?: (format: ImageFormat, compress: boolean) => void;
  /** For a libSceAmpr game: the AMPR LZ4 asset-pack option of an .exfat image. */
  lz4?: Lz4Choice;
  onCancel: () => void;
  /** Install the kept package: streamed from this computer, or uploaded to the PS5 first. */
  onInstall: (method: InstallMethod) => void;
  onLaunch: () => void;
  onShowFolder: () => void;
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
              {props.replaces
                ? tr("fpkg.convertReplace", undefined, "Convert & replace")
                : tr("fpkg.convertInstall", undefined, "Convert & install")}
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
          {/* A folder can also become a game image: the kind ShadowMount+ mounts from a
              drive, instead of a package the PS5 installs. */}
          {props.onMakeImage && (
            <div className="mt-1 border-t border-[var(--color-border)] pt-2">
              <div className="mb-1 text-xs text-[var(--color-muted)]">
                {tr(
                  "fpkg.image.lead",
                  undefined,
                  "Or make a game image instead of a package: one file you copy to a PS5 drive, which ShadowMount+ mounts and shows on the home screen. Nothing is installed.",
                )}
              </div>
              <ImageChoice
                disabled={props.canConvert === false}
                format={props.imageFormat ?? "ffpkg"}
                compress={props.imageCompress ?? false}
                onChoice={(f, c) => props.onImageChoice?.(f, c)}
                onMake={(compress, format) => props.onMakeImage?.(compress, format)}
                lz4={props.lz4}
              />
            </div>
          )}
          {props.replaces && props.canInstall && (
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "fpkg.replaceHint",
                undefined,
                "The dump on the PS5 is set aside while its package installs, and put back if the install fails. Saves are kept.",
              )}
            </div>
          )}
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
    <Card>
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
        {p.swap && (
          <div className="flex flex-col gap-2 rounded-md border border-[var(--color-border)] p-3 text-sm">
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
        {!p.deleted && !makesImage(p.mode) && installChoice(!installed)}
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
          {!p.deleted && (
            <Button onClick={props.onShowFolder}>{tr("fpkg.showFolder", undefined, "Show in folder")}</Button>
          )}
          {!p.deleted && !makesImage(p.mode) && props.onViewPackage && (
            <Button onClick={props.onViewPackage}>{tr("viewer_open", undefined, "View details")}</Button>
          )}
          {!p.deleted && !makesImage(p.mode) && (
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
            "In the Upload queue: it goes to the PS5 next, into the folder ShadowMount+ watches.",
          )}
        </span>
        {onOpen && (
          <Button size="sm" onClick={onOpen}>
            {tr("fpkg.openUpload", undefined, "Open Upload")}
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

/** The AMPR LZ4 asset-pack option for a libSceAmpr game's .exfat image. */
export interface Lz4Choice {
  /** What the check found about the game's own ampr_emu. */
  readiness: AmprPacksReadiness;
  on: boolean;
  onToggle: (on: boolean) => void;
  /** A profile read from a .toml file (its name); null packs with the built-in one. */
  profileName: string | null;
  onProfile: (profile: { name: string; text: string } | null) => void;
}

/** Packing replaces .ffpfsc compression: the packs are already compressed, and the engine
 *  writes them into a plain exFAT image only. */
export function lz4Active(format: ImageFormat, lz4: Lz4Choice | undefined): boolean {
  return format === "exfat" && !!lz4?.on && !lz4.readiness.refusal;
}

function Lz4Option({ lz4 }: { lz4: Lz4Choice }) {
  const tr = useTr();
  const refusal = lz4.readiness.refusal;
  return (
    <div className="space-y-1" data-testid="lz4-option">
      <label className="flex items-start gap-2 text-xs">
        <input
          type="checkbox"
          checked={lz4.on && !refusal}
          disabled={!!refusal}
          onChange={(e) => lz4.onToggle(e.target.checked)}
          className="mt-0.5"
        />
        <span>
          <span className="text-[var(--color-text)]">
            {tr("fpkg.image.lz4", undefined, "AMPR LZ4 asset packs (smaller, for ShadowMount+)")}
          </span>{" "}
          <span className="text-[var(--color-muted)]">
            {tr(
              "fpkg.image.lz4_body",
              undefined,
              "Packs the game's data into LZ4 asset packs that the game's own ampr_emu (fakelib/libSceAmpr.sprx) reads back, so the image is smaller. Executables, system files and already-compressed media stay as they are. Plain .exfat only, without .ffpfsc compression.",
            )}
          </span>
        </span>
      </label>
      {refusal ? (
        <div className="text-xs text-[var(--color-warn)]">
          {tr("fpkg.image.lz4_unavailable", { reason: refusal }, "Not available for this game: {reason}")}
        </div>
      ) : (
        lz4.readiness.runtime_version && (
          <div className="text-xs text-[var(--color-muted)]">
            {tr(
              "fpkg.image.lz4_runtime",
              { version: lz4.readiness.runtime_version },
              "The game's ampr_emu: {version}",
            )}
          </div>
        )
      )}
      {lz4.on && !refusal && (
        <div className="flex flex-wrap items-center gap-2 text-xs">
          {lz4.profileName ? (
            <>
              <span className="text-[var(--color-muted)]">
                {tr("fpkg.image.lz4_profile_named", { name: lz4.profileName }, "Profile: {name}")}
              </span>
              <Button variant="ghost" onClick={() => lz4.onProfile(null)}>
                {tr("fpkg.image.lz4_profile_clear", undefined, "Use the built-in profile")}
              </Button>
            </>
          ) : (
            <label className="cursor-pointer text-[var(--color-accent)] underline">
              {tr("fpkg.image.lz4_profile", undefined, "Use a profile (.toml)…")}
              <input
                type="file"
                accept=".toml"
                className="hidden"
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  e.target.value = "";
                  if (file) void file.text().then((text) => lz4.onProfile({ name: file.name, text }));
                }}
              />
            </label>
          )}
        </div>
      )}
    </div>
  );
}

/** Which image to make: the filesystem (UFS2 .ffpkg, what ShadowMount+ recommends, or exFAT),
 *  and whether to compress it into a .ffpfsc. */
function ImageChoice({
  disabled,
  format,
  compress,
  onChoice,
  onMake,
  lz4,
}: {
  disabled: boolean;
  format: ImageFormat;
  compress: boolean;
  onChoice: (format: ImageFormat, compress: boolean) => void;
  onMake: (compress: boolean, format: ImageFormat) => void;
  lz4?: Lz4Choice;
}) {
  const tr = useTr();
  const setFormat = (f: ImageFormat) => onChoice(f, compress);
  const setCompress = (c: boolean) => onChoice(format, c);
  const packing = lz4Active(format, lz4);
  const options: { id: ImageFormat; label: string; body: string }[] = [
    {
      id: "ffpkg",
      label: tr("fpkg.image.ffpkg", undefined, ".ffpkg (UFS2), recommended"),
      body: tr(
        "fpkg.image.ffpkg_body",
        undefined,
        "What ShadowMount+ recommends for most games.",
      ),
    },
    {
      id: "exfat",
      label: tr("fpkg.image.exfat_choice", undefined, ".exfat"),
      body: tr(
        "fpkg.image.exfat_body",
        undefined,
        "For games that only work like content on an external drive.",
      ),
    },
    {
      id: "ffpfs",
      label: tr("fpkg.image.ffpfs", undefined, ".ffpfs (PFS), experimental"),
      body: tr(
        "fpkg.image.ffpfs_body",
        undefined,
        "Experimental in ShadowMount+ 1.7. File names must be plain ASCII; the image's own name is kept to 63 characters.",
      ),
    },
  ];
  return (
    <div className="mt-1 space-y-2" data-testid="image-choice">
      <div className="grid gap-2 sm:grid-cols-3">
        {options.map((o) => (
          <button
            key={o.id}
            type="button"
            aria-pressed={format === o.id}
            onClick={() => setFormat(o.id)}
            className={`rounded-lg border px-3 py-2 text-left text-xs ${
              format === o.id
                ? "border-[var(--color-accent)] bg-[var(--color-surface-3)]"
                : "border-[var(--color-border)] hover:bg-[var(--color-surface-3)]"
            }`}
          >
            <div className="font-semibold text-[var(--color-text)]">{o.label}</div>
            <div className="mt-0.5 text-[var(--color-muted)]">{o.body}</div>
          </button>
        ))}
      </div>
      <label className="flex items-start gap-2 text-xs">
        <input
          type="checkbox"
          checked={compress && !packing}
          disabled={packing}
          onChange={(e) => setCompress(e.target.checked)}
          className="mt-0.5"
        />
        <span>
          <span className="text-[var(--color-text)]">
            {tr("fpkg.image.compress", undefined, "Compress it into a .ffpfsc")}
          </span>{" "}
          <span className="text-[var(--color-muted)]">
            {tr(
              "fpkg.image.compress_body_v2",
              undefined,
              "Usually 40–60% smaller, slower to make, and always mounted read-only. It is compressed as it is written, so only the compressed file is ever on disk.",
            )}
          </span>
        </span>
      </label>
      {lz4 && format === "exfat" && <Lz4Option lz4={lz4} />}
      <Button onClick={() => onMake(compress && !packing, format)} disabled={disabled}>
        {compress && !packing
          ? tr("fpkg.image.make_compressed", undefined, "Make compressed image")
          : tr("fpkg.image.make", undefined, "Make game image")}
      </Button>
    </div>
  );
}
