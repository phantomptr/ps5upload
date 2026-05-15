import { useState } from "react";
import {
  FileSearch,
  ChevronDown,
  ChevronRight,
  Loader2,
  AlertTriangle,
  CheckCircle2,
  Folder,
  File as FileIcon,
  Download,
  Package,
} from "lucide-react";
import {
  ffpkgInspect,
  ffpkgExtract,
  type FfpkgInspection,
  type FfpkgExtractResult,
} from "../../api/ps5";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { formatBytes } from "../../lib/format";

/**
 * Local UFS2 inspector panel — collapsed by default, expands into
 * a metadata + tree view when the user clicks Inspect.
 *
 * Why this exists: a PS5 .ffpkg is a UFS2 filesystem image. Today
 * the only way to know what's in one is to upload all 50 GB to the
 * PS5, mount it, and browse it from the Library tab. This is a
 * 30-second local read instead.
 *
 * Backend lives in ps5upload-pkg::ufs2 and is exposed as the
 * `ffpkg_inspect` Tauri command (engine /api/ffpkg/inspect).
 *
 * Read-only — never modifies the image file.
 */
export default function FfpkgInspectorPanel({ path }: { path: string }) {
  const tr = useTr();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [meta, setMeta] = useState<FfpkgInspection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [extractBusy, setExtractBusy] = useState<string | null>(null);
  const [extractResult, setExtractResult] = useState<FfpkgExtractResult | null>(null);

  async function runInspect() {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const m = await ffpkgInspect(path);
      setMeta(m);
      setOpen(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setOpen(true);
    } finally {
      setBusy(false);
    }
  }

  async function runExtract(innerPath: string, displayName: string) {
    if (extractBusy) return;
    const dest = await openDialog({
      directory: true,
      title: tr(
        "ffpkg_extract_picker",
        { name: displayName },
        `Pick a folder to extract ${displayName} into`,
      ),
    });
    if (!dest || typeof dest !== "string") return;
    setExtractBusy(innerPath);
    setError(null);
    setExtractResult(null);
    try {
      const r = await ffpkgExtract(path, innerPath, dest);
      setExtractResult(r);
      pushNotification("success", `Extracted ${displayName}`, {
        body: `${r.file_count} file${r.file_count === 1 ? "" : "s"}, ${formatBytes(r.bytes_written)} written to ${dest}.`,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification("error", `Extract failed: ${displayName}`, { body: msg });
    } finally {
      setExtractBusy(null);
    }
  }

  return (
    <div className="mt-3 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)]">
      <div className="flex items-center gap-2 px-3 py-2">
        <FileSearch size={14} className="text-[var(--color-muted)]" />
        <div className="flex-1 text-xs font-medium">
          {tr(
            "ffpkg_inspect_title",
            undefined,
            "Local inspection (UFS2 reader)",
          )}
        </div>
        {!meta && (
          <Button
            variant="secondary"
            size="sm"
            leftIcon={
              busy ? (
                <Loader2 size={11} className="animate-spin" />
              ) : (
                <FileSearch size={11} />
              )
            }
            onClick={runInspect}
            disabled={busy}
          >
            {busy
              ? tr("ffpkg_inspect_busy", undefined, "Reading…")
              : tr("ffpkg_inspect_run", undefined, "Inspect")}
          </Button>
        )}
        {meta && (
          <button
            type="button"
            onClick={() => setOpen((v) => !v)}
            className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-2)] hover:text-[var(--color-text)]"
          >
            {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          </button>
        )}
      </div>
      {error && (
        <div className="border-t border-[var(--color-border)] px-3 py-2 text-[11px] text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mr-1 inline" />
          {error}
        </div>
      )}
      {open && meta && (
        <div className="space-y-3 border-t border-[var(--color-border)] px-3 py-3 text-[11px]">
          <MetaGrid meta={meta} />
          {/* "Extract everything" button at top — useful when the
              user just wants the whole image's contents on disk
              (e.g. to inspect with a different tool, or to repack). */}
          <div className="flex flex-wrap gap-2">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={
                extractBusy === "" ? (
                  <Loader2 size={11} className="animate-spin" />
                ) : (
                  <Package size={11} />
                )
              }
              onClick={() =>
                runExtract(
                  "",
                  tr("ffpkg_extract_all_label", undefined, "the whole image"),
                )
              }
              disabled={extractBusy !== null}
            >
              {tr("ffpkg_extract_all", undefined, "Extract everything…")}
            </Button>
            {meta.has_sce_sys && (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={
                  extractBusy === "sce_sys" ? (
                    <Loader2 size={11} className="animate-spin" />
                  ) : (
                    <Package size={11} />
                  )
                }
                onClick={() => runExtract("sce_sys", "sce_sys/")}
                disabled={extractBusy !== null}
              >
                {tr("ffpkg_extract_sce_sys", undefined, "Extract sce_sys/…")}
              </Button>
            )}
          </div>
          <RootEntries
            meta={meta}
            extractBusy={extractBusy}
            onExtract={runExtract}
          />
          {extractResult && (
            <div className="rounded-md border border-[var(--color-good)] bg-[var(--color-surface-2)] p-2">
              <div className="flex items-center gap-1 font-medium text-[var(--color-good)]">
                <CheckCircle2 size={11} />
                {tr(
                  "ffpkg_extract_done",
                  {
                    files: extractResult.file_count,
                    bytes: formatBytes(extractResult.bytes_written),
                  },
                  `Wrote ${extractResult.file_count} file${extractResult.file_count === 1 ? "" : "s"} (${formatBytes(extractResult.bytes_written)})`,
                )}
              </div>
              {extractResult.sample_paths.length > 0 && (
                <details className="mt-1 text-[10px] text-[var(--color-muted)]">
                  <summary className="cursor-pointer">
                    {tr("ffpkg_extract_show_paths", undefined, "show paths")}
                  </summary>
                  <ul className="mt-1 max-h-32 overflow-auto pl-4">
                    {extractResult.sample_paths.map((p) => (
                      <li key={p} className="break-all font-mono">{p}</li>
                    ))}
                  </ul>
                </details>
              )}
            </div>
          )}
          {meta.warnings.length > 0 && (
            <div className="rounded-md border border-[var(--color-warn)] bg-[var(--color-surface-2)] p-2">
              <div className="mb-1 font-medium text-[var(--color-warn)]">
                {tr("ffpkg_inspect_warnings", undefined, "Warnings")}
              </div>
              <ul className="list-disc pl-4 text-[var(--color-muted)]">
                {meta.warnings.map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function MetaGrid({ meta }: { meta: FfpkgInspection }) {
  const tr = useTr();
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 font-mono">
      {meta.title && (
        <>
          <dt className="text-[var(--color-muted)]">
            {tr("ffpkg_inspect_title_field", undefined, "Title")}
          </dt>
          <dd className="font-sans">{meta.title}</dd>
        </>
      )}
      {meta.title_id && (
        <>
          <dt className="text-[var(--color-muted)]">
            {tr("ffpkg_inspect_title_id", undefined, "Title ID")}
          </dt>
          <dd>{meta.title_id}</dd>
        </>
      )}
      {meta.category && (
        <>
          <dt className="text-[var(--color-muted)]">
            {tr("ffpkg_inspect_category", undefined, "Category")}
          </dt>
          <dd>{meta.category}</dd>
        </>
      )}
      <dt className="text-[var(--color-muted)]">
        {tr("ffpkg_inspect_image_size", undefined, "Image size")}
      </dt>
      <dd>{formatBytes(meta.image_bytes)}</dd>
      <dt className="text-[var(--color-muted)]">
        {tr("ffpkg_inspect_block_size", undefined, "Block / fragment")}
      </dt>
      <dd>
        {meta.block_size} / {meta.fragment_size}
      </dd>
      <dt className="text-[var(--color-muted)]">
        {tr("ffpkg_inspect_sce_sys", undefined, "sce_sys/")}
      </dt>
      <dd>
        {meta.has_sce_sys ? (
          <span className="inline-flex items-center gap-1 text-[var(--color-good)]">
            <CheckCircle2 size={11} /> {tr("ffpkg_sce_sys_present", "present")}
          </span>
        ) : (
          <span className="text-[var(--color-warn)]">
            {tr("ffpkg_sce_sys_missing", "missing")}
          </span>
        )}
      </dd>
    </dl>
  );
}

function RootEntries({
  meta,
  extractBusy,
  onExtract,
}: {
  meta: FfpkgInspection;
  extractBusy: string | null;
  onExtract: (innerPath: string, displayName: string) => void;
}) {
  const tr = useTr();
  if (meta.root_entries.length === 0) {
    return (
      <div className="text-[var(--color-muted)]">
        {tr("ffpkg_inspect_empty", undefined, "Image is empty.")}
      </div>
    );
  }
  return (
    <div>
      <div className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {tr(
          "ffpkg_inspect_root",
          { count: meta.root_entries.length },
          `Root contents (${meta.root_entries.length})`,
        )}
      </div>
      <ul className="grid grid-cols-1 gap-1 sm:grid-cols-2">
        {meta.root_entries.map((e) => (
          <li
            key={e.name}
            className="flex items-center gap-2 rounded bg-[var(--color-surface-2)] px-2 py-1"
          >
            {e.kind === "dir" ? (
              <Folder size={11} className="shrink-0 text-[var(--color-accent)]" />
            ) : (
              <FileIcon size={11} className="shrink-0 text-[var(--color-muted)]" />
            )}
            <span className="min-w-0 flex-1 truncate font-mono">{e.name}</span>
            {e.kind === "file" && (
              <span className="text-[10px] text-[var(--color-muted)]">
                {formatBytes(e.size)}
              </span>
            )}
            {(e.kind === "file" || e.kind === "dir") && (
              <button
                type="button"
                onClick={() => onExtract(e.name, e.name)}
                disabled={extractBusy !== null}
                className="rounded p-0.5 text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:opacity-50"
                title={tr(
                  "ffpkg_extract_one",
                  { name: e.name },
                  `Extract ${e.name} to a local folder`,
                )}
              >
                {extractBusy === e.name ? (
                  <Loader2 size={11} className="animate-spin" />
                ) : (
                  <Download size={11} />
                )}
              </button>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

// formatBytes moved to lib/format.ts. Prior local copy mislabelled
// 1024-step values as KB/MB/GB; identical bytes now render the same
// across the FFPKG inspector, Library, and Upload screens.
