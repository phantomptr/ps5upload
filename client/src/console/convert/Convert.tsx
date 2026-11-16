import { useCallback, useEffect, useState } from "react";
import { FolderOpen, Repeat, Square } from "lucide-react";
import { formatBytes } from "../../lib/format";
import { call } from "../api";
import type { Volume } from "../home/Home";
import { joinPath, volumeOf } from "../files/move";
import { freeName, statOrNull } from "../files/paste";
import { PathPicker } from "../files/PathPicker";
import type { SectionContext } from "../sections";
import { t } from "../strings";
import {
  converting,
  DEFAULT_PKG_LEVEL,
  DEFAULT_ZSTD_LEVEL,
  dismissConvert,
  FFPFSC_LEVELS,
  formatLabel,
  INNER_FORMATS,
  fitFfpfscName,
  inspect,
  isArchiveFormat,
  isConvertSource,
  lastPart,
  OUT_FORMATS,
  PKG_LEVELS,
  outputName,
  parentOf,
  stageText,
  startBody,
  startConvert,
  stopConvert,
  useConvertRun,
  ZSTD_LEVELS,
  type ConvertRun,
  type FfpfscLevel,
  type InnerFormat,
  type Inspection,
  type OutFormat,
  type PkgLevel,
  type Verify,
  type ZstdLevel,
} from "./model";

/** What the source's check shows: reading, its answer, or why it could not be read. */
export type InspectState = { phase: "reading" } | { phase: "done"; info: Inspection } | { phase: "failed"; error: string | null };

export interface ConvertViewProps {
  source: string | null;
  inspected: InspectState | null;
  format: OutFormat;
  /** The output's full path. */
  dest: string | null;
  /** Free space where it is written (null: unknown). */
  free: number | null;
  /** Something is at `dest` already (null while checking). */
  exists: boolean | null;
  naming: boolean;
  zstd: boolean;
  level: ZstdLevel;
  /** With a .ffpfsc: the image inside it and how hard it is compressed. */
  inner: InnerFormat;
  ffpfscLevel: FfpfscLevel;
  /** With a .pkg: its Kraken level. */
  pkgLevel: PkgLevel;
  verify: Verify;
  run: ConvertRun | null;
  onPickSource: () => void;
  onPickDest: () => void;
  onFormat: (f: OutFormat) => void;
  onKeepBoth: () => void;
  onZstd: () => void;
  onLevel: (l: ZstdLevel) => void;
  onInner: (f: InnerFormat) => void;
  onFfpfscLevel: (l: FfpfscLevel) => void;
  onPkgLevel: (l: PkgLevel) => void;
  onVerify: (v: Verify) => void;
  onStart: () => void;
  onStop: () => void;
}

const LEVEL_KEYS: Record<ZstdLevel, string> = {
  1: "cw_convert_level_fast",
  5: "cw_convert_level_default",
  19: "cw_convert_level_small",
};

const PKG_LEVEL_KEYS: Record<PkgLevel, string> = {
  stored: "cw_convert_pkg_level_stored",
  fast: "cw_convert_pkg_level_fast",
  balanced: "cw_convert_pkg_level_balanced",
};

const FFPFSC_LEVEL_KEYS: Record<FfpfscLevel, string> = {
  fast: "cw_convert_ffpfsc_level_fast",
  balanced: "cw_convert_ffpfsc_level_balanced",
  smallest: "cw_convert_ffpfsc_level_smallest",
};

/** "Writing · 45% · 1.2 GB / 2.6 GB" while a conversion runs. */
export function progressLine(run: ConvertRun): string {
  if (run.stopping) return t("cw_files_batch_cancelling");
  const line = stageText(run.message) ?? t("cw_convert_running", { name: lastPart(run.source) });
  if (run.bytesTotal <= 0) return line;
  const done = Math.min(run.bytesDone, run.bytesTotal);
  const pct = Math.min(100, Math.floor((done / run.bytesTotal) * 100));
  return `${line} · ${t("cw_home_percent", { n: pct })} · ${formatBytes(done)} / ${formatBytes(run.bytesTotal)}`;
}

/** Convert without state: everything comes in through props (tests render it directly). */
export function ConvertView(p: ConvertViewProps) {
  const running = converting(p.run);
  const info = p.inspected?.phase === "done" ? p.inspected.info : null;
  const canStart = !running && info != null && p.dest != null && p.exists === false && !p.naming;
  // zstd needs the runtime on the console (only the desktop app puts it there).
  const zstdOn = p.zstd && info?.zstdRuntime != null;
  const choice = (on: boolean, label: string, action: string, onClick: () => void) => (
    <button
      key={action}
      type="button"
      className="cw-focusable cw-button"
      aria-pressed={on}
      data-action={action}
      disabled={running}
      onClick={onClick}
    >
      <span>{label}</span>
    </button>
  );
  const run = p.run;
  return (
    <div className="cw-convert-section">
      <h1 className="cw-h1">{t("cw_convert_title")}</h1>
      <p className="cw-dim">{t("cw_convert_intro")}</p>

      {run ? (
        running ? (
          <div className="cw-msg" role="status" data-convert-progress="">
            <span>{progressLine(run)}</span>
            {run.bytesTotal > 0 ? (
              <div className="cw-bar">
                <div className="cw-bar-fill" style={{ width: `${Math.min(100, (run.bytesDone / run.bytesTotal) * 100)}%` }} />
              </div>
            ) : null}
            <span className="cw-dim cw-mono">{run.dest}</span>
            {!run.stopping ? (
              <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="convert-stop" onClick={p.onStop}>
                <Square aria-hidden />
                <span>{t("cw_files_batch_stop")}</span>
              </button>
            ) : null}
          </div>
        ) : run.end ? (
          <div
            className={`cw-msg ${run.end.tone === "error" ? "is-error" : "is-info"}`}
            role={run.end.tone === "error" ? "alert" : "status"}
            data-convert-end={run.end.tone}
          >
            {run.end.text}
          </div>
        ) : null
      ) : null}

      <dl className="cw-info">
        <div>
          <dt className="cw-dim">{t("cw_convert_source")}</dt>
          <dd className="cw-mono" data-convert-source="">
            {p.source ?? "—"}
          </dd>
        </div>
      </dl>
      <div className="cw-toolbar">
        <button
          type="button"
          className="cw-focusable cw-button"
          data-action="convert-pick-source"
          data-nav-default=""
          disabled={running}
          onClick={p.onPickSource}
        >
          <FolderOpen aria-hidden />
          <span>{t(p.source ? "cw_files_extract_change" : "cw_convert_pick_source")}</span>
        </button>
      </div>

      {p.inspected?.phase === "reading" ? <p className="cw-dim">{t("cw_convert_reading")}</p> : null}
      {p.inspected?.phase === "failed" ? (
        <p className="cw-field-error" role="alert" data-convert-inspect-error="">
          {t("cw_convert_inspect_failed", { reason: p.inspected.error ?? t("cw_files_err_read") })}
        </p>
      ) : null}

      {info ? (
        <>
          <div className="cw-card" data-convert-info="">
            <h2 className="cw-h2">{info.title ?? lastPart(info.path)}</h2>
            <dl className="cw-info">
              {info.titleId ? (
                <div>
                  <dt className="cw-dim">{t("cw_convert_info_title_id")}</dt>
                  <dd className="cw-mono">{info.titleId}</dd>
                </div>
              ) : null}
              {info.contentVersion ? (
                <div>
                  <dt className="cw-dim">{t("cw_convert_info_version")}</dt>
                  <dd>{info.contentVersion}</dd>
                </div>
              ) : null}
              <div>
                <dt className="cw-dim">{t("cw_convert_info_format")}</dt>
                <dd>{formatLabel(info.format)}</dd>
              </div>
              <div>
                <dt className="cw-dim">{t("cw_convert_info_size")}</dt>
                <dd>{t("cw_convert_info_size_files", { size: formatBytes(info.size), n: info.files })}</dd>
              </div>
              <div>
                <dt className="cw-dim">{t("cw_convert_info_ampr")}</dt>
                <dd data-convert-ampr={info.packed ?? ""}>
                  {info.packed ? t("cw_convert_ampr_yes", { kind: info.packed }) : t("cw_convert_ampr_no")}
                </dd>
              </div>
            </dl>
            {!info.hasEboot ? <p className="cw-dim">{t("cw_convert_no_eboot")}</p> : null}
            {isArchiveFormat(info.format) ? (
              <p className="cw-dim" data-convert-archive="">
                {t("cw_convert_archive_note")}
              </p>
            ) : null}
          </div>

          <h2 className="cw-h2">{t("cw_convert_output")}</h2>
          <div className="cw-toolbar" role="group" aria-label={t("cw_convert_output")}>
            {OUT_FORMATS.map((f) => choice(p.format === f, formatLabel(f), `convert-format-${f}`, () => p.onFormat(f)))}
          </div>
          {p.format === "ffpfsc" ? (
            <div data-convert-ffpfsc="">
              <div className="cw-toolbar" role="group" aria-label={t("cw_convert_ffpfsc_inner")}>
                {INNER_FORMATS.map((f) => choice(p.inner === f, formatLabel(f), `convert-inner-${f}`, () => p.onInner(f)))}
              </div>
              <div className="cw-toolbar" role="group" aria-label={t("cw_convert_ffpfsc_level")}>
                {FFPFSC_LEVELS.map((l) =>
                  choice(p.ffpfscLevel === l, t(FFPFSC_LEVEL_KEYS[l]), `convert-ffpfsc-level-${l}`, () => p.onFfpfscLevel(l)),
                )}
              </div>
              <p className="cw-dim">{t("cw_convert_ffpfsc_note")}</p>
            </div>
          ) : null}
          {p.format === "pkg" ? (
            <div data-convert-pkg="">
              <div className="cw-toolbar" role="group" aria-label={t("cw_convert_pkg_level")}>
                {PKG_LEVELS.map((l) => choice(p.pkgLevel === l, t(PKG_LEVEL_KEYS[l]), `convert-pkg-level-${l}`, () => p.onPkgLevel(l)))}
              </div>
              <p className="cw-dim">{t("cw_convert_pkg_note")}</p>
            </div>
          ) : null}
          <dl className="cw-info">
            <div>
              <dt className="cw-dim">{t("cw_convert_dest")}</dt>
              <dd className="cw-mono" data-convert-dest="">
                {p.dest ?? "—"}
              </dd>
            </div>
            <div>
              <dt className="cw-dim">{t("cw_files_unpack_free")}</dt>
              <dd>{p.free != null ? formatBytes(p.free) : "—"}</dd>
            </div>
          </dl>
          <div className="cw-toolbar">
            <button type="button" className="cw-focusable cw-button" data-action="convert-pick-dest" disabled={running} onClick={p.onPickDest}>
              <FolderOpen aria-hidden />
              <span>{t("cw_convert_dest_change")}</span>
            </button>
            {p.exists ? (
              <button
                type="button"
                className="cw-focusable cw-button"
                data-action="convert-keep-both"
                aria-busy={p.naming || undefined}
                disabled={running}
                onClick={p.onKeepBoth}
              >
                <span>{t("cw_files_conflict_keep")}</span>
              </button>
            ) : null}
          </div>
          {p.exists ? (
            <p className="cw-field-error" role="alert" data-convert-blocked="exists">
              {t("cw_convert_exists", { name: p.dest ? lastPart(p.dest) : "" })}
            </p>
          ) : null}
          {p.free != null && info.dataBytes > p.free ? <p className="cw-dim">{t("cw_convert_space_note")}</p> : null}

          {p.format !== "pkg" ? (
            <>
              <button
                type="button"
                role="switch"
                aria-checked={zstdOn}
                className="cw-focusable cw-button"
                data-action="convert-zstd"
                disabled={running || !info.zstdRuntime}
                onClick={p.onZstd}
              >
                <span>{t("cw_convert_zstd")}</span>
                <span className={`cw-tag${zstdOn ? " is-on" : ""}`}>{t(zstdOn ? "cw_settings_on" : "cw_settings_off")}</span>
              </button>
              <p className="cw-dim">{t("cw_convert_zstd_note")}</p>
              {!info.zstdRuntime ? (
                <p className="cw-dim" data-convert-zstd-runtime="missing">
                  {t("cw_convert_zstd_no_runtime")}
                </p>
              ) : null}
              {zstdOn ? (
                <div className="cw-toolbar" role="group" aria-label={t("cw_convert_zstd_level")}>
                  {ZSTD_LEVELS.map((l) => choice(p.level === l, t(LEVEL_KEYS[l]), `convert-level-${l}`, () => p.onLevel(l)))}
                </div>
              ) : null}
            </>
          ) : null}

          <div className="cw-toolbar" role="group" aria-label={t("cw_convert_verify")}>
            {choice(p.verify === "fast", t("cw_convert_verify_fast"), "convert-verify-fast", () => p.onVerify("fast"))}
            {choice(p.verify === "full", t("cw_convert_verify_full"), "convert-verify-full", () => p.onVerify("full"))}
          </div>

          <div className="cw-dialog-buttons">
            <button type="button" className="cw-focusable cw-button is-primary" data-action="convert-go" disabled={!canStart} onClick={p.onStart}>
              <Repeat aria-hidden />
              <span>{t("cw_convert_start")}</span>
            </button>
          </div>
        </>
      ) : null}
    </div>
  );
}

/** Whether something is at `path` (unreadable: no, the helper refuses a name that is taken). */
async function isTaken(path: string): Promise<boolean> {
  return (await statOrNull(path)) != null;
}

/**
 * The Convert section: pick a game folder or image on the console's drives, read what it is,
 * choose the output, and convert it on the console. Opened at a source with params.path.
 */
export function Convert({ ctx }: { ctx: SectionContext }) {
  const run = useConvertRun();
  const [source, setSource] = useState<string | null>(ctx.params?.path ?? null);
  const [inspected, setInspected] = useState<InspectState | null>(null);
  const [format, setFormat] = useState<OutFormat>("exfat");
  // Where the output goes: beside the source until Change folder… picks another.
  const [parent, setParent] = useState<string | null>(null);
  const [name, setName] = useState<string | null>(null);
  const [exists, setExists] = useState<boolean | null>(null);
  const [naming, setNaming] = useState(false);
  const [zstd, setZstd] = useState(false);
  const [level, setLevel] = useState<ZstdLevel>(DEFAULT_ZSTD_LEVEL);
  const [inner, setInner] = useState<InnerFormat>("exfat");
  const [ffpfscLevel, setFfpfscLevel] = useState<FfpfscLevel>("balanced");
  const [pkgLevel, setPkgLevel] = useState<PkgLevel>(DEFAULT_PKG_LEVEL);
  const [verify, setVerify] = useState<Verify>("fast");
  const [picking, setPicking] = useState<"source" | "dest" | null>(null);
  const [volumes, setVolumes] = useState<Volume[]>([]);

  useEffect(() => {
    let live = true;
    call<{ volumes?: unknown }>("/api/volumes").then(
      (r) => {
        if (live) setVolumes(Array.isArray(r?.volumes) ? (r.volumes as Volume[]) : []);
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [picking]);

  // A new source is read (the helper runs one converter at a time: not while a conversion runs).
  const busy = converting(run);
  useEffect(() => {
    if (!source || busy) return;
    let live = true;
    setInspected({ phase: "reading" });
    void inspect(source).then((r) => {
      if (!live) return;
      setInspected(r.ok ? { phase: "done", info: r.info } : { phase: "failed", error: r.error });
    });
    return () => {
      live = false;
    };
  }, [source, busy]);

  const outParent = parent ?? (source ? parentOf(source) : null);
  const outName = name ?? (source ? outputName(source, format) : null);
  const dest = outParent && outName ? joinPath(outParent, outName) : null;
  useEffect(() => {
    if (!dest) return;
    let live = true;
    setExists(null);
    isTaken(dest).then(
      (yes) => {
        if (live) setExists(yes);
      },
      () => {
        if (live) setExists(false);
      },
    );
    return () => {
      live = false;
    };
  }, [dest, run?.end]);

  const pickSource = useCallback((p: string) => {
    setPicking(null);
    setSource(p);
    setParent(null);
    setName(null);
    dismissConvert();
  }, []);
  const pickDest = useCallback((d: string) => {
    setPicking(null);
    setParent(d);
    setName(null);
  }, []);
  const keepBoth = () => {
    if (!outParent || !outName) return;
    setNaming(true);
    // A .ffpfsc keeps to the length ShadowMountPlus mounts with its " (2)" too.
    freeName(outParent, outName, format === "folder", isTaken, format === "ffpfsc" ? fitFfpfscName : undefined).then(
      (n) => {
        setNaming(false);
        setName(n);
      },
      () => setNaming(false),
    );
  };
  // zstd only when the console has the runtime (the switch is off and disabled without it).
  const zstdOk = inspected?.phase === "done" && inspected.info.zstdRuntime != null;
  const start = () => {
    if (!source || !dest) return;
    void startConvert(startBody({ source, dest, format, zstd: zstd && zstdOk, level, verify, inner, ffpfscLevel, pkgLevel }));
  };

  const vol = outParent ? volumeOf(outParent, volumes) : null;
  return (
    <>
      <ConvertView
        source={source}
        inspected={inspected}
        format={format}
        dest={dest}
        free={vol ? vol.free_bytes : null}
        exists={exists}
        naming={naming}
        zstd={zstd}
        level={level}
        inner={inner}
        ffpfscLevel={ffpfscLevel}
        pkgLevel={pkgLevel}
        verify={verify}
        run={run}
        onPickSource={() => setPicking("source")}
        onPickDest={() => setPicking("dest")}
        onFormat={(f) => {
          setFormat(f);
          setName(null);
        }}
        onKeepBoth={keepBoth}
        onZstd={() => setZstd((z) => !z)}
        onLevel={setLevel}
        onInner={setInner}
        onFfpfscLevel={setFfpfscLevel}
        onPkgLevel={setPkgLevel}
        onVerify={setVerify}
        onStart={start}
        onStop={stopConvert}
      />
      {picking === "source" ? (
        <PathPicker
          title={t("cw_convert_pick_title")}
          subtitle={t("cw_convert_pick_hint_archives")}
          volumes={volumes}
          start={source ? parentOf(source) : null}
          pickFile={isConvertSource}
          onPick={pickSource}
          onCancel={() => setPicking(null)}
        />
      ) : null}
      {picking === "dest" ? (
        <PathPicker
          title={t("cw_convert_dest_pick")}
          subtitle={outName ?? ""}
          volumes={volumes}
          start={outParent}
          onPick={pickDest}
          onCancel={() => setPicking(null)}
        />
      ) : null}
    </>
  );
}
