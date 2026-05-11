import { useCallback, useEffect, useRef, useState } from "react";
import {
  CheckCircle2,
  XCircle,
  ChevronDown,
  ChevronRight,
  RefreshCw,
  Loader2,
  AlertTriangle,
  Activity,
  HardDrive,
} from "lucide-react";
import { smpStatus, type SmpStatus } from "../../api/ps5";
import { Button } from "../../components";
import { useTr } from "../../state/lang";

/**
 * Read-only ShadowMount+ status panel. Renders nothing until the
 * first probe completes; renders a compact "not installed" chip when
 * SMP is absent; expands into a detailed view when present.
 *
 * Why mount this in Library: the artefacts SMP manages (mounted game
 * images under /mnt/shadowmnt) are exactly what a user opens this
 * tab to find. Surfacing SMP's mount state here means the user
 * doesn't need to context-switch into FTP to know "did SMP pick up
 * my new image?"
 *
 * Why read-only: we never write SMP's config or state. Restart-style
 * actions go through the same Send-payload flow the user could
 * trigger themselves on the Payloads tab.
 */
export default function SmpPanel({ mgmtAddr }: { mgmtAddr: string | null }) {
  const tr = useTr();
  const [status, setStatus] = useState<SmpStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);

  // Tracks whether we've already auto-expanded on first install
  // detection. Using a ref (vs. reading `status === null` inside the
  // callback) means `refresh` doesn't have to depend on `status`,
  // which previously caused the auto-expand to capture a stale
  // `status` from useCallback's first creation and silently fail on
  // every subsequent manual Refresh click.
  const didAutoExpandRef = useRef(false);

  const refresh = useCallback(async () => {
    if (!mgmtAddr) return;
    setLoading(true);
    setError(null);
    try {
      const s = await smpStatus(mgmtAddr);
      setStatus(s);
      // Auto-expand on FIRST detection of "installed" so the user
      // sees the data without an extra click. Once we've fired this
      // once, keep the user's manual choice on subsequent refreshes.
      if (s.installed && !didAutoExpandRef.current) {
        didAutoExpandRef.current = true;
        setExpanded(true);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [mgmtAddr]);

  // Reset the auto-expand latch when switching to a different PS5 so
  // the new console gets its own first-install auto-expand.
  useEffect(() => {
    didAutoExpandRef.current = false;
    refresh();
  }, [mgmtAddr, refresh]);

  // Don't render anything until first probe lands. Avoids a "loading"
  // flash on every Library tab visit.
  if (!mgmtAddr || (status === null && !error)) return null;

  // Compact "not installed" — single chip the user can click to
  // expand if they want details (e.g. seeing the error). Doesn't
  // take meaningful screen space when SMP isn't set up.
  if (status && !status.installed && !status.running && !expanded) {
    return null;
  }

  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)]">
      {/* Header row: a clickable toggle (left, fills available space)
       * sits next to a sibling Refresh button. Earlier this was a single
       * <button> wrapping the Refresh <Button>, which produced a React
       * hydration warning ("button cannot appear as a descendant of
       * button") and also broke keyboard semantics — Tab landed on the
       * outer button, Enter both toggled expand AND triggered refresh
       * via stopPropagation handling. Splitting them keeps a11y clean. */}
      <div className="flex w-full items-center gap-3 px-4 py-2.5">
        <button
          type="button"
          className="flex min-w-0 flex-1 items-center gap-3 text-left"
          onClick={() => setExpanded((v) => !v)}
          aria-expanded={expanded}
        >
          {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          <HardDrive size={14} />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-semibold">
              {tr("smp_panel_title", undefined, "ShadowMount+")}
            </div>
            <div className="flex items-center gap-3 text-[11px] text-[var(--color-muted)]">
              <SmpBadge ok={status?.installed ?? false}>
                {status?.installed
                  ? tr("smp_installed", undefined, "installed")
                  : tr("smp_not_installed", undefined, "not installed")}
              </SmpBadge>
              <SmpBadge ok={status?.running ?? false}>
                {status?.running
                  ? tr("smp_running", undefined, "running")
                  : tr("smp_not_running", undefined, "not running")}
              </SmpBadge>
              {status && status.mounted_images.length > 0 && (
                <span>
                  {tr(
                    "smp_mounted_count",
                    { n: status.mounted_images.length },
                    `${status.mounted_images.length} mounted`,
                  )}
                </span>
              )}
            </div>
          </div>
        </button>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={
            loading ? (
              <Loader2 size={11} className="animate-spin" />
            ) : (
              <RefreshCw size={11} />
            )
          }
          onClick={() => {
            void refresh();
          }}
          disabled={loading}
        >
          {tr("smp_refresh", undefined, "Refresh")}
        </Button>
      </div>

      {expanded && (
        <div className="space-y-3 border-t border-[var(--color-border)] p-4 text-xs">
          {error && (
            <div className="flex items-start gap-2 rounded-md border border-[var(--color-bad)] p-2 text-[var(--color-bad)]">
              <AlertTriangle size={11} className="mt-0.5 shrink-0" />
              {error}
            </div>
          )}

          {status?.mounted_images.length ? (
            <SmpSection title={tr("smp_section_mounts", undefined, "Mounted images")}>
              <ul className="space-y-1">
                {status.mounted_images.map((m) => (
                  <li
                    key={m.mount_point}
                    className="flex items-center justify-between gap-3 rounded-md bg-[var(--color-surface)] px-2 py-1"
                  >
                    <span className="font-medium">
                      {m.derived_name || m.mount_point}
                    </span>
                    <code className="text-[10px] text-[var(--color-muted)]">
                      {m.mount_point}
                    </code>
                  </li>
                ))}
              </ul>
            </SmpSection>
          ) : status?.installed ? (
            <SmpSection title={tr("smp_section_mounts", undefined, "Mounted images")}>
              <p className="text-[var(--color-muted)]">
                {tr(
                  "smp_no_mounts",
                  undefined,
                  "No images currently mounted.",
                )}
              </p>
            </SmpSection>
          ) : null}

          {status?.config_ini && (
            <SmpFileBlock
              title="config.ini"
              path="/data/shadowmount/config.ini"
              body={status.config_ini}
            />
          )}
          {status?.autotune_ini && (
            <SmpAutotuneTable
              body={status.autotune_ini}
              mgmtAddr={mgmtAddr}
              onSaved={refresh}
            />
          )}
          {status?.debug_log_tail && (
            <SmpFileBlock
              title="debug.log (tail)"
              path="/data/shadowmount/debug.log"
              body={status.debug_log_tail}
            />
          )}

          {status && status.errors.length > 0 && (
            <SmpSection title={tr("smp_section_errors", undefined, "Probe errors")}>
              <ul className="space-y-1 text-[var(--color-warn)]">
                {status.errors.map((e) => (
                  <li key={e}>{e}</li>
                ))}
              </ul>
            </SmpSection>
          )}
        </div>
      )}
    </section>
  );
}

function SmpBadge({
  ok,
  children,
}: {
  ok: boolean;
  children: React.ReactNode;
}) {
  return (
    <span
      className={`inline-flex items-center gap-1 ${
        ok ? "text-[var(--color-good)]" : "text-[var(--color-muted)]"
      }`}
    >
      {ok ? <CheckCircle2 size={11} /> : <XCircle size={11} />}
      {children}
    </span>
  );
}

function SmpSection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="mb-1 flex items-center gap-1 text-[10px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        <Activity size={10} />
        {title}
      </div>
      {children}
    </div>
  );
}

function SmpFileBlock({
  title,
  path,
  body,
}: {
  title: string;
  path: string;
  body: string;
}) {
  return (
    <details className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)]">
      <summary className="cursor-pointer px-2 py-1.5 text-[11px]">
        <span className="font-medium">{title}</span>{" "}
        <code className="ml-2 text-[10px] text-[var(--color-muted)]">{path}</code>
      </summary>
      <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words border-t border-[var(--color-border)] p-2 font-mono text-[10px] text-[var(--color-text)]">
        {body}
      </pre>
    </details>
  );
}

interface AutotuneRow {
  /** "kstuff_delay" / "image_sector" / etc — keeps the renderer
   *  generic if SMP adds new autotune knobs in future versions. */
  key: string;
  /** TITLE_ID for kstuff_delay rules, image filename for image_*. */
  target: string;
  value: string;
}

/**
 * Parse SMP's autotune.ini into a structured table.
 *
 * Format (per shadowMountPlus's sm_config_mount.c):
 *   `<key>=<target>:<value>` — e.g. `kstuff_delay=PPSA12345:35`
 *   `<key>=<value>`         — e.g. `kstuff_pause_delay_image_seconds=25`
 *
 * Lines without `=` are comments / blank — skipped silently. The
 * structured view replaces the raw INI dump in the SMP panel; the
 * raw view is still accessible via "show raw" toggle.
 */
function parseAutotune(body: string): AutotuneRow[] {
  const rows: AutotuneRow[] = [];
  for (const raw of body.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#") || line.startsWith(";")) continue;
    const eq = line.indexOf("=");
    if (eq < 0) continue;
    const key = line.slice(0, eq).trim();
    const rest = line.slice(eq + 1).trim();
    const colon = rest.indexOf(":");
    if (colon >= 0) {
      rows.push({
        key,
        target: rest.slice(0, colon).trim(),
        value: rest.slice(colon + 1).trim(),
      });
    } else {
      rows.push({ key, target: "", value: rest });
    }
  }
  return rows;
}

function SmpAutotuneTable({
  body,
  mgmtAddr,
  onSaved,
}: {
  body: string;
  mgmtAddr: string | null;
  onSaved: () => void;
}) {
  const tr = useTr();
  const [showRaw, setShowRaw] = useState(false);
  const [edits, setEdits] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const rows = parseAutotune(body);
  const kstuffRows = rows.filter((r) => r.key === "kstuff_delay");
  const imageRows = rows.filter((r) => r.key.startsWith("image_"));
  const otherRows = rows.filter(
    (r) => r.key !== "kstuff_delay" && !r.key.startsWith("image_"),
  );

  // Build edited rows for save: take all original rows, override
  // with any edits keyed by `<key>:<target>`. Also lets the user
  // delete a row by setting its value to empty string.
  function buildEditedIni(): string {
    const lines: string[] = [];
    // Preserve top-of-file comments from the original.
    for (const raw of body.split(/\r?\n/)) {
      const t = raw.trim();
      if (t.startsWith("#") || t.startsWith(";") || !t) {
        lines.push(raw);
        continue;
      }
      // For data rows, look up the edit. The key for kstuff_delay is
      // `kstuff_delay:<title_id>`; for global keys it's just `<key>`.
      const eq = t.indexOf("=");
      if (eq < 0) {
        lines.push(raw);
        continue;
      }
      const key = t.slice(0, eq).trim();
      const rest = t.slice(eq + 1).trim();
      const colon = rest.indexOf(":");
      const target = colon >= 0 ? rest.slice(0, colon).trim() : "";
      const editKey = target ? `${key}:${target}` : key;
      if (editKey in edits) {
        const newVal = edits[editKey];
        if (!newVal) continue; // empty = delete row
        if (target) {
          lines.push(`${key}=${target}:${newVal}`);
        } else {
          lines.push(`${key}=${newVal}`);
        }
      } else {
        lines.push(raw);
      }
    }
    return lines.join("\n") + "\n";
  }

  async function save() {
    if (!mgmtAddr || Object.keys(edits).length === 0) return;
    setSaving(true);
    setSaveError(null);
    try {
      const { fsWriteText } = await import("../../api/ps5");
      const r = await fsWriteText(
        mgmtAddr,
        "/data/shadowmount/autotune.ini",
        buildEditedIni(),
      );
      if (!r.ok) {
        setSaveError(r.err ?? "write failed");
      } else {
        setEdits({});
        onSaved();
      }
    } catch (e) {
      setSaveError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }
  return (
    <details className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)]" open>
      <summary className="cursor-pointer px-2 py-1.5 text-[11px]">
        <span className="font-medium">autotune.ini</span>{" "}
        <code className="ml-2 text-[10px] text-[var(--color-muted)]">
          /data/shadowmount/autotune.ini
        </code>{" "}
        <span className="ml-2 text-[10px] text-[var(--color-muted)]">
          {tr(
            "smp_autotune_count",
            { n: rows.length },
            `${rows.length} rule${rows.length === 1 ? "" : "s"}`,
          )}
        </span>
      </summary>
      <div className="space-y-2 border-t border-[var(--color-border)] p-2 text-[11px]">
        {kstuffRows.length > 0 && (
          <AutotuneSubsection
            title={tr(
              "smp_autotune_kstuff",
              undefined,
              "Per-game kstuff delays (seconds)",
            )}
            rows={kstuffRows}
            targetLabel={tr("smp_autotune_title_id", undefined, "Title ID")}
            valueLabel={tr("smp_autotune_seconds", undefined, "Delay (s)")}
            editable
            edits={edits}
            onEdit={(editKey, value) =>
              setEdits((prev) => ({ ...prev, [editKey]: value }))
            }
          />
        )}
        {imageRows.length > 0 && (
          <AutotuneSubsection
            title={tr(
              "smp_autotune_images",
              undefined,
              "Per-image overrides",
            )}
            rows={imageRows}
            targetLabel={tr("smp_autotune_image", undefined, "Image")}
            valueLabel={tr("smp_autotune_value", undefined, "Override")}
          />
        )}
        {otherRows.length > 0 && (
          <AutotuneSubsection
            title={tr("smp_autotune_global", undefined, "Global tuning")}
            rows={otherRows}
            targetLabel={tr("smp_autotune_key", undefined, "Key")}
            valueLabel={tr("smp_autotune_value", undefined, "Value")}
          />
        )}
        <div className="flex items-center gap-2 pt-1">
          <button
            type="button"
            onClick={() => setShowRaw((v) => !v)}
            className="text-[10px] text-[var(--color-muted)] underline-offset-2 hover:underline"
          >
            {showRaw
              ? tr("smp_autotune_hide_raw", undefined, "hide raw")
              : tr("smp_autotune_show_raw", undefined, "show raw INI")}
          </button>
          {Object.keys(edits).length > 0 && mgmtAddr && (
            <>
              <button
                type="button"
                onClick={save}
                disabled={saving}
                className="rounded-md bg-[var(--color-accent)] px-2 py-0.5 text-[10px] text-[var(--color-accent-contrast)] disabled:opacity-50"
              >
                {saving
                  ? tr("smp_autotune_saving", undefined, "Saving…")
                  : tr(
                      "smp_autotune_save",
                      { count: Object.keys(edits).length },
                      `Save ${Object.keys(edits).length} edit${Object.keys(edits).length === 1 ? "" : "s"}`,
                    )}
              </button>
              <button
                type="button"
                onClick={() => setEdits({})}
                className="text-[10px] text-[var(--color-muted)] hover:underline"
              >
                {tr("smp_autotune_revert", undefined, "Discard")}
              </button>
            </>
          )}
          {saveError && (
            <span className="text-[10px] text-[var(--color-bad)]">
              {saveError}
            </span>
          )}
        </div>
        {showRaw && (
          <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words border-t border-[var(--color-border)] p-2 font-mono text-[10px] text-[var(--color-text)]">
            {body}
          </pre>
        )}
      </div>
    </details>
  );
}

function AutotuneSubsection({
  title,
  rows,
  targetLabel,
  valueLabel,
  editable = false,
  edits = {},
  onEdit,
}: {
  title: string;
  rows: AutotuneRow[];
  targetLabel: string;
  valueLabel: string;
  editable?: boolean;
  edits?: Record<string, string>;
  onEdit?: (editKey: string, value: string) => void;
}) {
  return (
    <div>
      <div className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {title}
      </div>
      <table className="w-full text-[10px]">
        <thead>
          <tr className="text-left text-[var(--color-muted)]">
            <th className="px-1 py-0.5">{targetLabel}</th>
            <th className="px-1 py-0.5">{valueLabel}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => {
            const editKey = r.target ? `${r.key}:${r.target}` : r.key;
            const editedValue = editKey in edits ? edits[editKey] : r.value;
            return (
              <tr
                key={`${r.key}-${r.target}-${i}`}
                className="border-t border-[var(--color-border)]"
              >
                <td className="px-1 py-0.5 font-mono">
                  {r.target || r.key}
                </td>
                <td className="px-1 py-0.5 font-mono">
                  {editable && onEdit ? (
                    <input
                      type="text"
                      value={editedValue}
                      onChange={(e) => onEdit(editKey, e.target.value)}
                      className={`w-20 rounded border bg-[var(--color-surface)] px-1 ${
                        editKey in edits
                          ? "border-[var(--color-accent)]"
                          : "border-[var(--color-border)]"
                      }`}
                      placeholder="(blank=delete)"
                    />
                  ) : (
                    r.value
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
