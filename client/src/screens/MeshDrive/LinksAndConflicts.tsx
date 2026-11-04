// Linked consoles and conflicts (8.1.0).
//
// A console is the master of its circle: the devices that sync a drive through it. Linking a
// drive with another console makes that console a master too, with its own home folder and
// circle; an engine in the drive keeps the two homes the same, and devices sync with their own
// console (falling back to a linked one while theirs is away). A console holds a drive at most
// once, so links form a tree and a change never travels in a circle.
//
// Conflicts: a file changed in two places is kept twice, the later copy renamed
// "(conflict from <device> <time>)". The conflicts dialog lists them beside their originals and
// settles each on the console, which every device and linked console then follows.

import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Copy, FileCheck2, Gamepad2, GitMerge, Link2, Loader2, Unlink } from "lucide-react";

import { Button, Input, Modal, SegmentedControl } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import {
  consolePathOf,
  driveApi,
  fetchConflicts,
  refusalCode,
  type Drive,
  type DriveConflict,
  type DriveLink,
  type HubChoice,
  type ResolveAction,
} from "../../api/meshDrive";
import { hostOf } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { pickPath } from "../../lib/pickPath";
import { useTr, type Translator } from "../../state/lang";
import { profileNameForAddr, useRosterStore } from "../../state/roster";
import { agoText } from "../Devices";
import { driveDetailText } from "./driveDetail";
import { PathPicker, useFolderCheck } from "./DriveDialogs";
import { FolderCheckNote } from "./FolderCheckNote";
import { formatDate } from "../../lib/formatDate";
import { humanizeCaught } from "../../lib/humanizeError";

/** A console's name as the user knows it: the link's own, the console list's, or its address. */
export function linkName(l: Pick<DriveLink, "console" | "console_name">, profiles: { host: string; name: string }[]) {
  return l.console_name || profileNameForAddr(l.console, profiles as never) || l.console;
}

function linkStateText(tr: Translator, l: DriveLink): { tone: string; text: string } {
  switch (l.state) {
    case "idle":
      return { tone: "text-[var(--color-good)]", text: tr("drive_badge_synced", undefined, "In sync") };
    case "error":
      return { tone: "text-[var(--color-bad)]", text: tr("drive_badge_error", undefined, "Problem") };
    case "offline":
      return { tone: "text-[var(--color-warn)]", text: tr("drive_link_offline", undefined, "Console away") };
    case "confirm":
      return { tone: "text-[var(--color-warn)]", text: tr("drive_badge_confirm", undefined, "Needs you") };
    default:
      return {
        tone: "text-[var(--color-muted)]",
        text: tr("drive_link_waiting", undefined, "Waiting for a device that reaches both consoles"),
      };
  }
}

/** The drive's other masters, each with its state and an Unlink. */
export function LinkedConsoles({
  d,
  host,
  now,
  canManage,
  run,
}: {
  d: Drive;
  host: string;
  now: number;
  canManage: boolean;
  run: (f: () => Promise<unknown>) => Promise<void>;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const profiles = useRosterStore((s) => s.profiles);
  const links = d.links ?? [];
  if (!links.length) return null;
  const unlink = async (l: DriveLink) => {
    const name = linkName(l, profiles);
    const ok = await confirm({
      title: tr("drive_unlink_title", { name }, `Unlink ${name}?`),
      message: tr(
        "drive_unlink_body",
        { name },
        `${name} keeps its copy as a drive of its own, for the devices that sync through it. The two stop being kept the same. No files are deleted.`,
      ),
      confirmLabel: tr("drive_unlink", undefined, "Unlink"),
      destructive: true,
    });
    if (ok) await run(() => driveApi.unlink(host, d.id, l.console));
  };
  return (
    <div className="mt-3 border-t border-[var(--color-border)] pt-2">
      {dialog}
      <p className="mb-1 flex items-center gap-1.5 text-xs font-medium text-[var(--color-muted)]">
        <Link2 size={12} aria-hidden />
        {tr("drive_linked_consoles", { count: links.length }, `Linked consoles (${links.length})`)}
      </p>
      <ul className="divide-y divide-[var(--color-border)]">
        {links.map((l) => {
          const st = linkStateText(tr, l);
          return (
            <li key={l.console} className="flex min-w-0 items-start gap-3 py-2">
              <Gamepad2 size={14} aria-hidden className="mt-0.5 shrink-0 text-[var(--color-muted)]" />
              <div className="min-w-0 flex-1">
                <p className="flex flex-wrap items-center gap-x-2 text-sm">
                  <span className="truncate font-medium">{linkName(l, profiles)}</span>
                  <span className={`text-xs ${st.tone}`}>{st.text}</span>
                </p>
                <p className="truncate text-xs text-[var(--color-muted)]" title={l.hub}>
                  {[l.console, l.hub].join(" · ")}
                </p>
                <p className="text-xs text-[var(--color-muted)]">
                  {[
                    tr("drive_link_circle", { count: l.circle }, `Devices there: ${l.circle}`),
                    l.last_sync
                      ? tr("drive_member_synced", { ago: agoText(tr, now - l.last_sync) }, `synced ${agoText(tr, now - l.last_sync)}`)
                      : tr("drive_member_never", undefined, "not synced yet"),
                    l.by_name ? tr("drive_link_by", { name: l.by_name }, `by ${l.by_name}`) : null,
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </p>
                {l.error && (
                  <p className="break-words text-xs text-[var(--color-bad)]">
                    {driveDetailText(tr, l.error_code, l.error_args, l.error)}
                  </p>
                )}
                {l.state === "confirm" && canManage && (
                  <Button
                    className="mt-1"
                    size="sm"
                    variant="primary"
                    onClick={() => void run(() => driveApi.allowLinkDeletes(host, d.id, l.console))}
                  >
                    {tr("drive_link_allow_deletes", undefined, "Let the deletes through")}
                  </Button>
                )}
              </div>
              {canManage && (
                <Button
                  size="sm"
                  variant="ghost"
                  leftIcon={<Unlink size={13} />}
                  onClick={() => void unlink(l)}
                  title={tr("drive_unlink", undefined, "Unlink")}
                  className="max-sm:min-w-9! max-sm:px-0 max-sm:[&>span]:sr-only"
                >
                  {tr("drive_unlink", undefined, "Unlink")}
                </Button>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** Links a drive with another console from the console list. */
export function LinkDialog({
  host,
  drive,
  onClose,
  onDone,
}: {
  host: string;
  drive: Drive;
  onClose: () => void;
  onDone: () => void;
}) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const here = hostOf(host);
  const linked = new Set((drive.links ?? []).map((l) => hostOf(l.console)));
  const choices = profiles.filter((p) => hostOf(p.host) && hostOf(p.host) !== here && !linked.has(hostOf(p.host)));
  const [other, setOther] = useState(choices[0]?.host ?? "");
  const [mode, setMode] = useState<"new" | "existing">("new");
  const [parent, setParent] = useState("/data");
  const [folderName, setFolderName] = useState(drive.name);
  const [existing, setExisting] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const hub: HubChoice | undefined =
    mode === "new" ? (parent ? { path: parent, name: folderName.trim() } : undefined) : existing ? { path: existing } : undefined;
  const checks = useFolderCheck(host, other && hub ? { hub, drive: drive.id, console: other } : null);
  const ready = !!other && !!hub && !!checks.hub?.ok && !checks.busy && !saving;
  const otherName = profiles.find((p) => p.host === other)?.name ?? other;
  const hereName = profiles.find((p) => hostOf(p.host) === here)?.name ?? here;

  const browse = async (set: (p: string) => void) => {
    const p = await pickPath({ mode: "folder", source: { console: other } });
    if (p) set(consolePathOf(p));
  };

  const save = async () => {
    if (!hub) return;
    setSaving(true);
    setError(null);
    try {
      await driveApi.link(host, drive.id, other, hub, otherName, hereName);
      onDone();
    } catch (e) {
      setError(
        refusalCode(e) === "already_linked"
          ? tr("drive_link_already", undefined, "That console already holds this drive.")
          : e instanceof Error
            ? e.message
            : String(e),
      );
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open
      onClose={onClose}
      size="lg"
      title={tr("drive_link_title", { name: drive.name }, `Link ${drive.name} with another console`)}
      titleIcon={<Link2 size={18} />}
      footer={
        <div className="flex justify-end gap-2 px-5 py-3">
          <Button variant="ghost" onClick={onClose}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button variant="primary" disabled={!ready} loading={saving} onClick={() => void save()}>
            {tr("drive_link_go", undefined, "Link")}
          </Button>
        </div>
      }
    >
      <div className="space-y-4 px-5 py-4">
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "drive_link_body",
            undefined,
            "The other console gets its own copy and becomes the home of the drive for the devices that sync through it. A computer or Docker in the drive keeps the two consoles the same, directly between them. Devices fall back to a linked console while their own is away.",
          )}
        </p>
        {choices.length === 0 ? (
          <p className="text-sm text-[var(--color-warn)]">
            {tr(
              "drive_link_none",
              undefined,
              "Add the other console in Connection first: the drive can only link to a console in your list.",
            )}
          </p>
        ) : (
          <>
            <section className="space-y-1">
              <p className="text-sm font-medium">{tr("drive_link_which", undefined, "1. Which console")}</p>
              <div className="flex flex-wrap gap-1.5">
                {choices.map((p) => (
                  <button
                    key={p.id}
                    type="button"
                    aria-pressed={other === p.host}
                    onClick={() => setOther(p.host)}
                    className="chip gap-1.5 px-3 py-1.5 text-sm"
                  >
                    <Gamepad2 size={13} aria-hidden />
                    {p.name}
                    <span className="font-mono text-xs opacity-60">{hostOf(p.host)}</span>
                  </button>
                ))}
              </div>
            </section>
            <section className="space-y-2">
              <p className="text-sm font-medium">{tr("drive_link_home", undefined, "2. Its home on that console")}</p>
              <SegmentedControl
                value={mode}
                onChange={(v) => setMode(v as "new" | "existing")}
                ariaLabel={tr("drive_link_home", undefined, "2. Its home on that console")}
                segments={[
                  { value: "new", label: tr("drive_home_new", undefined, "Make a new folder") },
                  { value: "existing", label: tr("drive_home_existing", undefined, "Use a folder that is there") },
                ]}
              />
              {mode === "new" ? (
                <div className="space-y-2">
                  <PathPicker
                    label={tr("drive_home_in", undefined, "Make it in")}
                    value={parent}
                    placeholder={tr("drive_home_in_placeholder", undefined, "Choose where on the console")}
                    onPick={() => void browse(setParent)}
                    icon={<Gamepad2 size={14} />}
                  />
                  <Input
                    label={tr("drive_folder_name", undefined, "Folder name")}
                    value={folderName}
                    onChange={(e) => setFolderName(e.target.value)}
                    maxLength={200}
                  />
                </div>
              ) : (
                <PathPicker
                  label={tr("drive_home_folder", undefined, "Folder on the console")}
                  value={existing}
                  placeholder={tr("drive_home_folder_placeholder", undefined, "Choose a folder on the console")}
                  onPick={() => void browse(setExisting)}
                  icon={<Gamepad2 size={14} />}
                />
              )}
              <FolderCheckNote check={hub ? checks.hub : null} busy={!!hub && checks.busy} where="console" />
            </section>
          </>
        )}
        {(error || checks.error) && <p className="text-sm text-[var(--color-bad)]">{error ?? checks.error}</p>}
      </div>
    </Modal>
  );
}

const when = (secs: number | null) => (secs ? formatDate(secs * 1000) : "—");

/** The drive's conflict copies beside their originals, each settled with one choice. */
export function ConflictsDialog({
  host,
  drive,
  onClose,
  onChanged,
}: {
  host: string;
  drive: Drive;
  onClose: () => void;
  onChanged: () => void;
}) {
  const tr = useTr();
  const [list, setList] = useState<DriveConflict[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A failed read of the list shows until a read succeeds; a failed choice until the next one.
  const [loadError, setLoadError] = useState<string | null>(null);
  const [naming, setNaming] = useState<{ copy: string; name: string } | null>(null);

  const load = useCallback(async () => {
    try {
      setList(await fetchConflicts(host, drive.id));
      setLoadError(null);
    } catch (e) {
      setLoadError(humanizeCaught(e));
    }
  }, [host, drive.id]);
  useEffect(() => {
    void load();
  }, [load]);

  const settle = async (c: DriveConflict, action: ResolveAction, name?: string) => {
    setBusy(c.copy);
    setError(null);
    try {
      await driveApi.resolve(host, drive.id, c.copy, action, name);
      setNaming(null);
      onChanged();
    } catch (e) {
      const code = refusalCode(e);
      setError(
        code === "busy"
          ? tr("drive_conflict_busy", undefined, "The drive is syncing right now. Try again in a moment.")
          : code === "gone"
            ? tr("drive_conflict_gone", undefined, "Another device settled that one already.")
            : code === "bad_name"
              ? tr("drive_check_bad_name", undefined, 'That name cannot be a folder name (no / \\ : * ? " < > |).')
              : e instanceof Error
                ? e.message
                : String(e),
      );
    } finally {
      setBusy(null);
      await load();
    }
  };

  return (
    <Modal
      open
      onClose={onClose}
      size="lg"
      title={tr("drive_conflicts_title", { name: drive.name }, `Conflicts in ${drive.name}`)}
      titleIcon={<GitMerge size={18} />}
      footer={
        <div className="flex justify-end px-5 py-3">
          <Button variant="ghost" onClick={onClose}>
            {tr("close", undefined, "Close")}
          </Button>
        </div>
      }
    >
      <div className="space-y-3 px-5 py-4">
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "drive_conflicts_body",
            undefined,
            "These files changed in two places before syncing. Both versions were kept. Choose which stays: every device follows. What goes is moved to the drive's trash for 30 days.",
          )}
        </p>
        {list === null && !error && !loadError && (
          <p className="flex items-center gap-2 text-sm text-[var(--color-muted)]">
            <Loader2 size={14} className="animate-spin" aria-hidden />
            {tr("drive_checking", undefined, "Checking…")}
          </p>
        )}
        {list?.length === 0 && (
          <p className="flex items-center gap-2 text-sm text-[var(--color-good)]">
            <FileCheck2 size={14} aria-hidden />
            {tr("drive_conflicts_none", undefined, "No conflicts. Every file has one version.")}
          </p>
        )}
        <ul className="space-y-2">
          {(list ?? []).map((c) => (
            <li key={c.copy} className="rounded-[var(--radius-input,0.5rem)] border border-[var(--color-border)] p-3 text-sm">
              <p className="break-all font-medium">{c.original}</p>
              <div className="mt-2 grid gap-2 sm:grid-cols-2">
                <div className="rounded-md bg-[var(--color-surface-raised)] p-2 text-xs">
                  <p className="font-medium">{tr("drive_conflict_original", undefined, "The original")}</p>
                  {c.original_size === null ? (
                    <p className="text-[var(--color-warn)]">{tr("drive_conflict_original_gone", undefined, "Deleted since")}</p>
                  ) : (
                    <p className="text-[var(--color-muted)]">
                      {formatBytes(c.original_size)} · {when(c.original_mtime)}
                    </p>
                  )}
                </div>
                <div className="rounded-md bg-[var(--color-surface-raised)] p-2 text-xs">
                  <p className="font-medium">
                    {tr("drive_conflict_copy", { device: c.device }, `The copy from ${c.device}`)}
                  </p>
                  <p className="text-[var(--color-muted)]">
                    {formatBytes(c.copy_size)} · {when(c.copy_mtime)}
                  </p>
                </div>
              </div>
              {naming?.copy === c.copy ? (
                <div className="mt-2 flex flex-wrap items-end gap-2">
                  <div className="min-w-0 flex-1">
                    <Input
                      label={tr("drive_conflict_new_name", undefined, "Name for the copy")}
                      value={naming.name}
                      onChange={(e) => setNaming({ copy: c.copy, name: e.target.value })}
                      maxLength={200}
                    />
                  </div>
                  <Button size="sm" variant="primary" loading={busy === c.copy} onClick={() => void settle(c, "keep_both", naming.name)}>
                    {tr("drive_conflict_keep_both", undefined, "Keep both")}
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => setNaming(null)}>
                    {tr("cancel", undefined, "Cancel")}
                  </Button>
                </div>
              ) : (
                <div className="mt-2 flex flex-wrap gap-1.5">
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={busy !== null}
                    loading={busy === c.copy}
                    onClick={() => void settle(c, "keep_original")}
                  >
                    {tr("drive_conflict_keep_original", undefined, "Keep the original")}
                  </Button>
                  <Button size="sm" variant="secondary" disabled={busy !== null} onClick={() => void settle(c, "keep_copy")}>
                    {tr("drive_conflict_keep_copy", undefined, "Keep the copy")}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    leftIcon={<Copy size={13} />}
                    disabled={busy !== null}
                    onClick={() => {
                      const file = c.original.split("/").pop() ?? c.original;
                      const dot = file.lastIndexOf(".");
                      const name = dot > 0 ? `${file.slice(0, dot)} (from ${c.device})${file.slice(dot)}` : `${file} (from ${c.device})`;
                      setNaming({ copy: c.copy, name });
                    }}
                  >
                    {tr("drive_conflict_keep_both", undefined, "Keep both")}
                  </Button>
                </div>
              )}
            </li>
          ))}
        </ul>
        {(error ?? loadError) && (
          <p className="flex items-start gap-1.5 text-sm text-[var(--color-bad)]">
            <AlertTriangle size={14} className="mt-0.5 shrink-0" aria-hidden />
            {error ?? loadError}
          </p>
        )}
      </div>
    </Modal>
  );
}
