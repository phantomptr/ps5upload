import { useEffect, useMemo, useState } from "react";
import { File, FileArchive, Folder, Package } from "lucide-react";

import { archiveEntries, type ArchiveEntries, type ArchiveRow, type PackageBrief } from "../api/archiveEntries";
import { choiceOf, initialTicks, packageRole, type ArchiveChoice } from "../lib/archiveChoice";
import { formatBytes } from "../lib/format";
import { humanizeCaught } from "../lib/humanizeError";
import { rarPasswordProblem } from "../lib/rarPassword";
import { useTr } from "../state/lang";
import { Button } from "./Button";
import { Checkbox } from "./Checkbox";
import { Modal } from "./Modal";
import { Spinner } from "./Spinner";

/** One line about a package: its game, what it is and its version, when its header was read. */
export function PackageLine({ info }: { info: Partial<PackageBrief> | null | undefined }) {
  const tr = useTr();
  if (!info) return null;
  const role = packageRole(info.category);
  const parts = [
    info.title?.trim() || null,
    info.title_id || null,
    role === "base"
      ? tr("archive_choose_role_base", undefined, "Game")
      : role === "update"
        ? tr("archive_choose_role_update", undefined, "Update")
        : role === "dlc"
          ? tr("archive_choose_role_dlc", undefined, "DLC")
          : null,
    info.app_ver ? tr("archive_choose_version", { v: info.app_ver }, "v{v}") : null,
  ].filter((p): p is string => !!p);
  if (parts.length === 0) return null;
  return <span className="block truncate text-[11px] text-[var(--color-muted)]">{parts.join(" · ")}</span>;
}

/** Tick all / none, and what the ticks come to. */
export function ChoiceSummary({
  choice,
  total,
  onAll,
  onNone,
  disabled = false,
}: {
  choice: ArchiveChoice;
  total: number;
  onAll: () => void;
  onNone: () => void;
  disabled?: boolean;
}) {
  const tr = useTr();
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs" data-testid="archive-choose-summary">
      <span className="text-[var(--color-muted)]">
        {tr(
          "archive_choose_count",
          { n: choice.count, total, size: formatBytes(choice.bytes) },
          "{n} of {total} chosen, {size}",
        )}
      </span>
      <span className="flex-1" />
      <Button size="sm" variant="ghost" onClick={onAll} disabled={disabled} data-testid="archive-choose-all">
        {tr("archive_choose_all", undefined, "Select all")}
      </Button>
      <Button size="sm" variant="ghost" onClick={onNone} disabled={disabled} data-testid="archive-choose-none">
        {tr("archive_choose_none", undefined, "Select none")}
      </Button>
    </div>
  );
}

/** The archive's rows, each with its tick. */
export function ArchiveRowList({
  rows,
  ticked,
  onToggle,
  disabled = false,
}: {
  rows: readonly ArchiveRow[];
  ticked: ReadonlySet<string>;
  onToggle: (path: string, on: boolean) => void;
  disabled?: boolean;
}) {
  const tr = useTr();
  return (
    <ul
      className="max-h-[50vh] overflow-auto rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] text-xs"
      data-testid="archive-choose-rows"
    >
      {rows.map((r) => {
        const Icon = r.kind === "package" ? Package : r.kind === "folder" ? Folder : File;
        return (
          <li key={r.path} className="border-b border-[var(--glass-edge)] px-2.5 py-1 last:border-b-0">
            <Checkbox
              checked={ticked.has(r.path)}
              onChange={(on) => onToggle(r.path, on)}
              disabled={disabled}
              label={
                <span className="flex min-w-0 items-center gap-2">
                  <Icon size={14} className="shrink-0 text-[var(--color-muted)]" aria-hidden />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-mono" title={r.path}>
                      {r.path}
                    </span>
                    {r.kind === "package" && <PackageLine info={r.package} />}
                    {r.kind === "folder" && (
                      <span className="block text-[11px] text-[var(--color-muted)]">
                        {tr("archive_choose_folder_files", { n: r.files }, "Folder, {n} file(s)")}
                      </span>
                    )}
                  </span>
                  <span className="shrink-0 tabular-nums text-[var(--color-muted)]">{formatBytes(r.size)}</span>
                </span>
              }
            />
          </li>
        );
      })}
    </ul>
  );
}

/**
 * "Choose files" in an archive (#969): its packages, folders and loose files, all ticked to start
 * with (or as `chosen` left them). Confirm hands back the choice; every row ticked is the whole
 * archive (`paths` null), exactly as without a choice.
 */
export function ArchiveChooser({
  open,
  archivePath,
  password = null,
  chosen = null,
  onCancel,
  onConfirm,
  load = archiveEntries,
}: {
  open: boolean;
  archivePath: string;
  /** A RAR's password, when it has one. */
  password?: string | null;
  /** The earlier choice (paths inside the archive), or null for everything. */
  chosen?: readonly string[] | null;
  onCancel: () => void;
  onConfirm: (choice: ArchiveChoice) => void;
  /** Tests pass their own listing. */
  load?: (path: string, password?: string | null) => Promise<ArchiveEntries>;
}) {
  const tr = useTr();
  const [listing, setListing] = useState<ArchiveEntries | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [ticked, setTicked] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (!open) return;
    let live = true;
    setListing(null);
    setError(null);
    load(archivePath, password).then(
      (l) => {
        if (!live) return;
        setListing(l);
        setTicked(initialTicks(l.items, chosen));
      },
      (e: unknown) => {
        if (!live) return;
        const msg = e instanceof Error ? e.message : String(e);
        setError(
          rarPasswordProblem(null, msg)
            ? tr(
                "archive_choose_password",
                undefined,
                "This archive needs its password before its files can be listed. Enter it first, then choose again.",
              )
            : tr("archive_choose_failed", { error: humanizeCaught(e) }, "Couldn't read what is in the archive: {error}"),
        );
      },
    );
    return () => {
      live = false;
    };
    // `chosen` is read once per opening; `load` is the default reader or a test's.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, archivePath, password]);

  const rows = useMemo(() => listing?.items ?? [], [listing]);
  const choice = choiceOf(rows, ticked, listing?.complete ?? true);
  const name = archivePath.replace(/\\/g, "/").split("/").pop() ?? archivePath;
  const toggle = (path: string, on: boolean) =>
    setTicked((cur) => {
      const next = new Set(cur);
      if (on) next.add(path);
      else next.delete(path);
      return next;
    });

  return (
    <Modal
      open={open}
      onClose={onCancel}
      size="lg"
      variant="sheet"
      titleIcon={<FileArchive size={18} />}
      title={tr("archive_choose_title", { name }, "Choose files in {name}")}
      footer={
        <>
          <Button variant="ghost" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            disabled={!listing || choice.count === 0}
            onClick={() => onConfirm(choice)}
            data-testid="archive-choose-ok"
          >
            {tr("archive_choose_ok", undefined, "Use these")}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-2 text-sm">
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "archive_choose_body",
            undefined,
            "Only the ticked items are unpacked and sent; the rest of the archive is skipped. A folder goes with everything in it.",
          )}
        </p>
        {!listing && !error && (
          <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            <Spinner size={14} tone="accent" />
            {tr("rarpkg.listing", undefined, "Reading the archive…")}
          </div>
        )}
        {error && (
          <p role="alert" className="text-xs text-[var(--color-bad)]" data-testid="archive-choose-error">
            {error}
          </p>
        )}
        {listing && !listing.complete && (
          <p className="text-xs text-[var(--color-warn)]">
            {tr(
              "archive_choose_cut",
              undefined,
              "This archive has too many items to list them all, so it is sent whole.",
            )}
          </p>
        )}
        {listing && listing.complete && rows.length > 0 && (
          <>
            <ChoiceSummary
              choice={choice}
              total={rows.length}
              onAll={() => setTicked(new Set(rows.map((r) => r.path)))}
              onNone={() => setTicked(new Set())}
            />
            <ArchiveRowList rows={rows} ticked={ticked} onToggle={toggle} />
          </>
        )}
        {listing && rows.length === 0 && (
          <p className="text-xs text-[var(--color-muted)]">
            {tr("archive_choose_empty", undefined, "This archive has no files in it.")}
          </p>
        )}
      </div>
    </Modal>
  );
}
