// Mesh Drive (8.0.0; reworked 8.0.1): folders kept the same on every device that joins them, at
// home on the console. The tab lists the drives on this console: whether this device keeps each
// one and how it is doing, and every device in it as that device last reported to the console (the
// console is the one source all of them read). Joining needs no other device: a drive is open, or
// asks for the password it was given. Making, joining and the password are guided dialogs
// (DriveDialogs.tsx) where every folder is the user's choice and is checked before anything is made.

import { useCallback, useEffect, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import {
  FolderOpen,
  FolderSync,
  HardDrive,
  GitMerge,
  KeyRound,
  Link2,
  Lock,
  MonitorSmartphone,
  Pencil,
  Pause,
  Play,
  Plus,
  RefreshCw,
  Trash2,
  UserMinus,
  X,
} from "lucide-react";

import { Badge, Button, Callout, OverflowMenu, SectionHeading, type OverflowMenuItem } from "../../components";
import { useConfirm, usePrompt } from "../../components/ConfirmDialog";
import { meshDevice, setMeshName } from "../../api/mesh";
import {
  driveApi,
  fetchDrives,
  type Drive,
  type DriveMember,
  type DrivesAnswer,
  type LocalDriveMember,
} from "../../api/meshDrive";
import { formatBytes } from "../../lib/format";
import { openLocalPath } from "../../lib/openLocalPath";
import { isMobile } from "../../lib/platform";
import { useConnectionStore } from "../../state/connection";
import { engineIsOnThisDevice } from "../../state/engine";
import { deviceName, meshFor, useMeshStore } from "../../state/mesh";
import { useTr, type Translator } from "../../state/lang";
import { useToastStore } from "../../state/toasts";
import { agoText, KIND_ICONS } from "../Devices";
import { CreateDriveDialog, JoinDriveDialog, PasswordDialog } from "./DriveDialogs";
import { ConflictsDialog, LinkDialog, LinkedConsoles, linkName } from "./LinksAndConflicts";
import { useRosterStore } from "../../state/roster";
import { AwayActions, AwayFolders, awayText, useConsoleName } from "./AwayFolders";
import { driveDetailText } from "./driveDetail";
import { humanizeCaught } from "../../lib/humanizeError";

type Tone = "good" | "warn" | "bad" | "neutral" | "accent";

const TONE_TEXT: Record<Tone, string> = {
  good: "text-[var(--color-good)]",
  warn: "text-[var(--color-warn)]",
  bad: "text-[var(--color-bad)]",
  accent: "text-[var(--color-accent-bright)]",
  neutral: "text-[var(--color-muted)]",
};

type Loaded = Extract<DrivesAnswer, { supported: true }>;

function statusOf(
  tr: Translator,
  l: LocalDriveMember,
  now: number,
  consoleName: string,
): { tone: Tone; short: string; long: string } {
  const s = l.status;
  const detail = driveDetailText(tr, s.detail_code, s.detail_args, s.detail);
  const ago = s.last_ok ? agoText(tr, now - s.last_ok) : "";
  // Its console is away: say which way, and that nothing made here is lost.
  if (s.state === "offline" && s.away) return { tone: "warn", ...awayText(tr, s, consoleName, now) };
  switch (s.state) {
    case "idle":
      // A linked console stands in while this one is away.
      if (s.away)
        return {
          tone: "good",
          short: tr("drive_badge_synced", undefined, "In sync"),
          long: tr(
            "drive_away_via",
            { name: consoleName },
            `${consoleName} is away: synced through a linked console`,
          ),
        };
      return {
        tone: "good",
        short: tr("drive_badge_synced", undefined, "In sync"),
        long: ago
          ? tr("drive_synced", { count: s.files, ago }, `In sync · ${s.files} files · ${ago}`)
          : tr("drive_member_files", { count: s.files }, `Files: ${s.files}`),
      };
    case "syncing":
      return {
        tone: "accent",
        short: tr("drive_syncing", undefined, "Syncing…"),
        long: s.todo
          ? tr(
              "drive_progress",
              { done: s.done ?? 0, todo: s.todo, file: s.current ?? "" },
              `${s.done ?? 0} of ${s.todo} · ${s.current ?? ""}`,
            )
          : "",
      };
    case "waiting":
      return {
        tone: "neutral",
        short: tr("drive_badge_waiting", undefined, "Waiting"),
        long: detail || tr("drive_waiting", undefined, "Another device is syncing it"),
      };
    case "paused":
      return { tone: "neutral", short: tr("drive_paused", undefined, "Paused"), long: "" };
    case "offline":
      return { tone: "warn", short: tr("drive_offline", undefined, "Folder offline"), long: detail };
    case "confirm":
      return {
        tone: "warn",
        short: tr("drive_badge_confirm", undefined, "Needs you"),
        long: tr(
          "drive_confirm_text",
          { count: s.held_deletes },
          `${s.held_deletes} files would be deleted everywhere. Nothing was deleted yet.`,
        ),
      };
    case "error":
      return {
        tone: "bad",
        short: tr("drive_badge_error", undefined, "Problem"),
        long: detail || tr("drive_error", undefined, "The last sync failed"),
      };
    case "removed":
      return { tone: "neutral", short: tr("drive_removed", undefined, "No longer in this drive"), long: "" };
    default:
      return { tone: "neutral", short: tr("drive_starting", undefined, "Starting…"), long: "" };
  }
}

/** What a member last told the console, in a few words (for any device, this one included). */
function reportedState(tr: Translator, m: DriveMember): { tone: Tone; text: string } {
  if (m.paused) return { tone: "neutral", text: tr("drive_paused", undefined, "Paused") };
  switch (m.state) {
    case "idle":
      return { tone: "good", text: tr("drive_badge_synced", undefined, "In sync") };
    case "error":
      return { tone: "bad", text: tr("drive_badge_error", undefined, "Problem") };
    case "offline":
      return { tone: "warn", text: tr("drive_offline", undefined, "Folder offline") };
    case "confirm":
      return { tone: "warn", text: tr("drive_badge_confirm", undefined, "Needs you") };
    case "removed":
      return { tone: "neutral", text: tr("drive_removed", undefined, "No longer in this drive") };
    default:
      return { tone: "neutral", text: tr("drive_not_reported", undefined, "Not reported yet") };
  }
}

const KIND_LABEL: Record<string, string> = { desktop: "Desktop", android: "Android", ios: "iOS", docker: "Docker" };

/** Phone, Docker or computer, as on the Devices screen. */
function KindGlyph({ kind }: { kind: string }) {
  const Icon = KIND_ICONS[kind] ?? MonitorSmartphone;
  return <Icon size={13} aria-hidden className="shrink-0 text-[var(--color-muted)]" />;
}

function MemberRow({
  m,
  me,
  online,
  now,
  onRemove,
  name,
  onRename,
}: {
  m: DriveMember;
  me: boolean;
  online: boolean | null;
  now: number;
  onRemove: (() => void) | null;
  /** The name the console knows the device by (Devices renames it for everyone). */
  name: string;
  onRename: () => void;
}) {
  const tr = useTr();
  const st = reportedState(tr, m);
  const kind = [KIND_LABEL[m.kind ?? ""] ?? m.kind, m.os && m.kind !== m.os ? m.os : "", m.app ? `v${m.app}` : ""]
    .filter(Boolean)
    .join(" · ");
  return (
    <li className="flex flex-wrap min-w-0 items-start gap-3 py-2">
      <span
        className={`mt-1.5 size-2 shrink-0 rounded-full ${online ? "bg-[var(--color-good)]" : "bg-[var(--color-border)]"}`}
        title={
          online === null
            ? undefined
            : online
              ? tr("mesh_online", undefined, "Online")
              : tr("mesh_offline", undefined, "Offline")
        }
        aria-hidden
      />
      <div className="min-w-0 flex-1 basis-[12rem]">
        <p className="flex flex-wrap items-center gap-x-2 text-sm">
          <KindGlyph kind={m.kind ?? ""} />
          <span className="truncate font-medium">{name}</span>
          {me && <span className="text-xs text-[var(--color-accent-bright)]">{tr("drive_you", undefined, "you")}</span>}
          <span className={`text-xs ${TONE_TEXT[st.tone]}`}>{st.text}</span>
        </p>
        <p className="truncate text-xs text-[var(--color-muted)]" title={m.path}>
          {[kind, m.path].filter(Boolean).join(" · ")}
        </p>
        <p className="text-xs text-[var(--color-muted)]">
          {m.reported
            ? [
                tr("drive_member_files", { count: m.files ?? 0 }, `Files: ${m.files ?? 0}`),
                formatBytes(m.bytes ?? 0),
                m.last_sync
                  ? tr("drive_member_synced", { ago: agoText(tr, now - m.last_sync) }, `synced ${agoText(tr, now - m.last_sync)}`)
                  : tr("drive_member_never", undefined, "not synced yet"),
              ].join(" · ")
            : tr("drive_member_joined", { ago: agoText(tr, now - m.joined) }, `joined ${agoText(tr, now - m.joined)}`)}
        </p>
        {m.error && (
          <p className="break-words text-xs text-[var(--color-bad)]">
            {driveDetailText(tr, m.error_code, m.error_args, m.error)}
          </p>
        )}
      </div>
      <div className="flex shrink-0 items-center justify-end gap-1">
        {/* On a phone the icons alone, so the device's name keeps the room. */}
        <Button
          size="sm"
          variant="ghost"
          leftIcon={<Pencil size={13} />}
          onClick={onRename}
          title={tr("mesh_rename", undefined, "Rename")}
          className="max-sm:min-w-9! max-sm:px-0 max-sm:[&>span]:sr-only"
        >
          {tr("mesh_rename", undefined, "Rename")}
        </Button>
        {onRemove && (
          <Button
            size="sm"
            variant="ghost"
            leftIcon={<UserMinus size={13} />}
            onClick={onRemove}
            title={tr("drive_member_remove", undefined, "Remove")}
            className="max-sm:min-w-9! max-sm:px-0 max-sm:[&>span]:sr-only"
          >
            {tr("drive_member_remove", undefined, "Remove")}
          </Button>
        )}
      </div>
    </li>
  );
}

/** The files a held mass delete would take: the first ones by path, and how many more. */
export function heldDetails(status: { held_deletes: number; held_paths?: string[] }, tr: Translator): string[] {
  const shown = status.held_paths ?? [];
  const more = status.held_deletes - shown.length;
  return more > 0 && shown.length > 0
    ? [...shown, tr("drive_confirm_more", { count: more }, "…and {count} more")]
    : shown;
}

function DriveCard({
  d,
  data,
  host,
  now,
  run,
  onJoin,
  onPassword,
  onLink,
  onConflicts,
}: {
  d: Drive;
  data: Loaded;
  host: string;
  now: number;
  run: (f: () => Promise<unknown>) => Promise<void>;
  onJoin: (d: Drive) => void;
  onPassword: (d: Drive) => void;
  onLink: (d: Drive) => void;
  onConflicts: (d: Drive) => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const { confirm, dialog } = useConfirm();
  const { prompt, dialog: promptDialog } = usePrompt();
  const meshDevices = useMeshStore((s) => meshFor(s, host).state?.devices ?? null);
  // The console's name for a device wins over the one its engine last reported: a rename in
  // Devices (or here) reaches every device through the console at once.
  const nameOf = (m: DriveMember): string => {
    const dev = meshDevices?.find((x) => x.id === m.device);
    return dev ? deviceName(dev) : m.device_name || tr("mesh_another_device", undefined, "Another device");
  };
  const renameMember = async (m: DriveMember) => {
    const name = await prompt({
      title: tr("mesh_rename_title", undefined, "Rename this device"),
      message: tr("mesh_rename_body", undefined, "Every device that uses this console sees the new name."),
      defaultValue: nameOf(m),
    });
    if (name === null || !name.trim()) return;
    await run(async () => {
      if (m.device === data.me) await setMeshName(name.trim());
      await meshDevice(host, "rename", m.device, name.trim());
    });
  };
  const mine = data.local.find((l) => l.drive === d.id);
  // Kept on this device through a linked console instead (one folder per drive per device).
  const viaOther = mine ? undefined : data.elsewhere.find((e) => e.drive === d.id);
  const profiles = useRosterStore((s) => s.profiles);
  const myMember = d.members.find((m) => m.id === mine?.member);
  const iAmIn = d.members.some((m) => m.device === data.me) || d.by === data.me;
  const consoleName = useConsoleName(host);
  const st = mine ? statusOf(tr, mine, now, consoleName) : null;
  const devices = new Set(d.members.map((m) => m.device)).size;
  const free = data.free[d.id];
  // A folder of this device opens in Finder / Explorer; a phone or a Docker engine has none to show.
  const canOpen = !!mine && engineIsOnThisDevice() && !isMobile();
  const onlineOf = (device: string): boolean | null =>
    device === data.me ? true : meshDevices ? meshDevices.some((x) => x.id === device && x.online) : null;

  const removeMember = async (m: DriveMember) => {
    const ok = await confirm({
      title: tr(
        "drive_member_remove_title",
        { device: nameOf(m) },
        `Remove ${nameOf(m)} from this drive?`,
      ),
      message: tr(
        "drive_member_remove_body",
        undefined,
        "For a device that is gone. Its files stay where they are; it stops syncing. It can join again.",
      ),
      confirmLabel: tr("drive_member_remove", undefined, "Remove"),
      destructive: true,
    });
    if (ok) await run(() => driveApi.leave(host, d.id, m.id));
  };

  const openHere = async () => {
    if (!mine) return;
    if (!(await openLocalPath(mine.path))) throw new Error(tr("drive_open_failed", { path: mine.path }, `Could not open ${mine.path}`));
  };

  const leave = async () => {
    if (!mine) return;
    const ok = await confirm({
      title: tr("drive_leave_title", undefined, "Stop keeping this drive on this device?"),
      message: tr(
        "drive_leave_body",
        { path: mine.path },
        `The files in ${mine.path} stay where they are; they just stop being kept in sync.`,
      ),
      confirmLabel: tr("drive_leave", undefined, "Leave"),
      destructive: true,
    });
    if (ok) await run(() => driveApi.leave(host, d.id, mine.member));
  };

  const remove = async () => {
    const ok = await confirm({
      title: tr("drive_delete_title", { name: d.name }, `Delete the drive ${d.name}?`),
      message: tr(
        "drive_delete_body",
        undefined,
        "Every device stops keeping it in sync. No files are deleted: each folder, and the one on the console, keeps what it has.",
      ),
      confirmLabel: tr("drive_delete", undefined, "Delete drive"),
      destructive: true,
    });
    if (ok) await run(() => driveApi.remove(host, d.id));
  };

  const reviewDeletes = async () => {
    if (!mine) return;
    const ok = await confirm({
      title: tr(
        "drive_confirm_title",
        { count: mine.status.held_deletes },
        `Delete ${mine.status.held_deletes} files everywhere?`,
      ),
      message: tr(
        "drive_confirm_body",
        undefined,
        "They were deleted in one place, so the drive would delete them on every device. Check that no folder was moved or a drive unplugged first. Deleted files stay in the drive's trash folder for 30 days.",
      ),
      details: heldDetails(mine.status, tr),
      confirmLabel: tr("drive_confirm_go", undefined, "Delete everywhere"),
      destructive: true,
    });
    if (ok) await run(() => driveApi.confirm(host, d.id, mine.member));
  };

  const menu: OverflowMenuItem[] = [
    ...(mine && myMember
      ? [
          {
            label: myMember.paused ? tr("drive_resume", undefined, "Resume") : tr("drive_pause", undefined, "Pause"),
            icon: myMember.paused ? <Play size={12} /> : <Pause size={12} />,
            onSelect: () => void run(() => driveApi.pause(host, d.id, mine.member, !myMember.paused)),
          },
          { label: tr("drive_leave", undefined, "Leave"), icon: <X size={12} />, onSelect: () => void leave() },
        ]
      : []),
    ...(iAmIn
      ? [
          {
            label: tr("drive_link_menu", undefined, "Link to another console…"),
            icon: <Link2 size={12} />,
            onSelect: () => onLink(d),
          },
        ]
      : []),
    {
      label: d.lock
        ? tr("drive_password_change", undefined, "Change or remove password…")
        : tr("drive_password_set", undefined, "Set a password…"),
      icon: <KeyRound size={12} />,
      onSelect: () => onPassword(d),
    },
    ...(iAmIn
      ? [
          {
            label: tr("drive_delete", undefined, "Delete drive"),
            icon: <Trash2 size={12} />,
            onSelect: () => void remove(),
            destructive: true,
          },
        ]
      : []),
  ];

  return (
    <article className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-4">
      {dialog}
      {promptDialog}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start">
        <div className="flex min-w-0 flex-1 items-start gap-3">
          <FolderSync size={22} className="mt-0.5 shrink-0 text-[var(--color-accent-bright)]" aria-hidden />
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <h3 className="truncate text-base font-semibold">{d.name}</h3>
              {d.lock && (
                <span title={tr("drive_locked_hint", undefined, "Joining asks for a password")}>
                  <Lock size={13} className="text-[var(--color-muted)]" aria-label={tr("drive_locked", undefined, "Password")} />
                </span>
              )}
              {st ? (
                <Badge tone={st.tone} dot>
                  {st.short}
                </Badge>
              ) : viaOther ? (
                <Badge tone="neutral">
                  {tr(
                    "drive_kept_via",
                    { name: linkName({ console: viaOther.console, console_name: "" }, profiles) },
                    `Kept here through ${linkName({ console: viaOther.console, console_name: "" }, profiles)}`,
                  )}
                </Badge>
              ) : (
                <Badge tone="neutral">{tr("drive_not_here", undefined, "Not on this device")}</Badge>
              )}
            </div>
            <p className="truncate text-xs text-[var(--color-muted)]" title={d.hub}>
              {tr("drive_home", { path: d.hub }, `Home on the console: ${d.hub}`)}
            </p>
            <p className="text-xs text-[var(--color-muted)]">
              {[
                tr("drive_device_count", { count: devices }, `Devices: ${devices}`),
                d.stats ? tr("drive_member_files", { count: d.stats.files }, `Files: ${d.stats.files}`) : null,
                d.stats ? formatBytes(d.stats.bytes) : null,
                d.stats?.conflicts
                  ? tr("drive_conflict_count", { count: d.stats.conflicts }, `Conflict copies: ${d.stats.conflicts}`)
                  : null,
                free !== undefined ? tr("drive_free", { size: formatBytes(free) }, `${formatBytes(free)} free on the console`) : null,
              ]
                .filter(Boolean)
                .join(" · ")}
            </p>
            {mine && (
              <p className="truncate text-xs text-[var(--color-muted)]" title={mine.path}>
                {tr("drive_here_at", { path: mine.path }, `Here: ${mine.path}`)}
              </p>
            )}
            {st?.long && <p className={`mt-1 text-xs ${TONE_TEXT[st.tone]}`}>{st.long}</p>}
            {mine?.status.state === "offline" && mine.status.away && (
              <div className="mt-2">
                <AwayActions console={host} why={mine.status.away} />
              </div>
            )}
            {!!mine?.status.skipped?.length && (
              <p className="mt-1 text-xs text-[var(--color-warn)]" title={mine.status.skipped.join("\n")}>
                {tr(
                  "drive_skipped",
                  { count: mine.status.skipped.length },
                  `Left out: ${mine.status.skipped.length}. They keep failing, or this device cannot hold their names. Hover to see them.`,
                )}
              </p>
            )}
          </div>
        </div>
        <div className="flex shrink-0 flex-wrap items-center gap-1.5">
          <Button
            size="sm"
            variant="ghost"
            onClick={() => navigate(`/files?path=${encodeURIComponent(d.hub)}`)}
            leftIcon={<HardDrive size={14} />}
            title={tr("drive_open_on_console", undefined, "Open on console")}
            // On a phone the icon alone, so it, Sync now and ⋯ fit one line.
            className="max-sm:min-w-10! max-sm:px-0 max-sm:[&>span]:sr-only"
          >
            {tr("drive_open_on_console", undefined, "Open on console")}
          </Button>
          {canOpen && (
            <Button size="sm" variant="ghost" onClick={() => void run(openHere)} leftIcon={<FolderOpen size={14} />}>
              {tr("drive_open_folder", undefined, "Open folder")}
            </Button>
          )}
          {mine ? (
            <Button
              size="sm"
              variant="secondary"
              onClick={() => void run(() => driveApi.sync(host, d.id))}
              leftIcon={<RefreshCw size={14} />}
            >
              {tr("drive_sync_now", undefined, "Sync now")}
            </Button>
          ) : viaOther ? null : (
            <Button size="sm" variant="primary" onClick={() => onJoin(d)} leftIcon={<Plus size={14} />}>
              {tr("drive_keep_here", undefined, "Keep on this device…")}
            </Button>
          )}
          {menu.length > 0 && <OverflowMenu items={menu} ariaLabel={tr("more_actions", undefined, "More actions")} />}
        </div>
      </div>
      {(mine?.status.state === "confirm" || !!d.stats?.conflicts) && (
        <div className="mt-3 flex flex-wrap gap-1.5">
          {mine?.status.state === "confirm" && (
            <Button size="sm" variant="primary" onClick={() => void reviewDeletes()}>
              {tr("drive_confirm_review", undefined, "Review the deletes")}
            </Button>
          )}
          {!!d.stats?.conflicts && (
            <Button size="sm" variant="secondary" leftIcon={<GitMerge size={14} />} onClick={() => onConflicts(d)}>
              {tr("drive_conflicts_open", { count: d.stats.conflicts }, `Settle conflicts (${d.stats.conflicts})`)}
            </Button>
          )}
        </div>
      )}
      <LinkedConsoles d={d} host={host} now={now} canManage={iAmIn} run={run} />
      {d.members.length > 0 && (
        <details className="mt-3 border-t border-[var(--color-border)] pt-2" open={d.members.length <= 4}>
          <summary className="cursor-pointer text-xs font-medium text-[var(--color-muted)]">
            {tr("drive_members", { count: d.members.length }, `Devices in this drive (${d.members.length})`)}
          </summary>
          <ul className="divide-y divide-[var(--color-border)]">
            {d.members.map((m) => (
              <MemberRow
                key={m.id}
                m={m}
                me={m.device === data.me}
                online={onlineOf(m.device)}
                now={now}
                onRemove={m.device === data.me ? null : () => void removeMember(m)}
                name={nameOf(m)}
                onRename={() => void renameMember(m)}
              />
            ))}
          </ul>
        </details>
      )}
    </article>
  );
}

/** The anchor old /mesh-drive links land on: /shared-folders#mesh-drive scrolls here. */
export const MESH_DRIVE_ANCHOR = "mesh-drive";

/**
 * Mesh Drives, as a section of Shared Folders: folders kept the same on every device, with
 * their home on this console. It was its own screen; servers (NAS, SMB, FTP, SFTP) sit beside
 * it now, since to a user both are folders reached over the network.
 */
export function MeshDriveSection() {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const [data, setData] = useState<DrivesAnswer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [creating, setCreating] = useState(false);
  // Dialogs hold the drive's id, and read the drive from the latest answer: a drive locked (or
  // removed) by another device after the click is shown as it is now.
  const [joiningId, setJoiningId] = useState<string | null>(null);
  const [lockingId, setLockingId] = useState<string | null>(null);
  const [linkingId, setLinkingId] = useState<string | null>(null);
  const [conflictsId, setConflictsId] = useState<string | null>(null);
  const drives = data?.supported ? data.drives : [];
  const joining = drives.find((d) => d.id === joiningId) ?? null;
  const locking = drives.find((d) => d.id === lockingId) ?? null;
  const linking = drives.find((d) => d.id === linkingId) ?? null;
  const settling = drives.find((d) => d.id === conflictsId) ?? null;
  const seq = useRef(0);
  const inFlight = useRef(false);
  // A failed poll shows until a poll succeeds; an action's failure until the next action.
  const [pollError, setPollError] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!host || !up || inFlight.current) return;
    const mine = ++seq.current;
    inFlight.current = true;
    try {
      const d = await fetchDrives(host);
      if (mine !== seq.current) return; // the console changed meanwhile
      setData(d);
      setPollError(null);
      setNow(Date.now());
    } catch (e) {
      if (mine === seq.current) setPollError(humanizeCaught(e));
    } finally {
      inFlight.current = false;
    }
  }, [host, up]);

  // Another console: nothing of the last one's stays on screen.
  useEffect(() => {
    seq.current++;
    inFlight.current = false;
    setData(null);
    setError(null);
    setPollError(null);
    setJoiningId(null);
    setLockingId(null);
    setLinkingId(null);
    setConflictsId(null);
  }, [host]);

  useEffect(() => {
    void load();
    const id = window.setInterval(() => {
      if (document.visibilityState === "visible") void load();
    }, 5_000);
    return () => window.clearInterval(id);
  }, [load]);

  const run = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(humanizeCaught(e));
    }
    inFlight.current = false;
    await load();
  };

  // An old /mesh-drive link lands on /shared-folders#mesh-drive: bring the section into view once.
  const { hash } = useLocation();
  const sectionRef = useRef<HTMLElement>(null);
  const scrolled = useRef(false);
  useEffect(() => {
    if (hash !== `#${MESH_DRIVE_ANCHOR}` || scrolled.current) return;
    scrolled.current = true;
    sectionRef.current?.scrollIntoView?.({ block: "start", behavior: "smooth" });
  }, [hash]);

  const header = (
    <SectionHeading
      id="mesh-drive-heading"
      icon={FolderSync}
      title={tr("drive_title", undefined, "Mesh Drive")}
      description={tr(
        "drive_subtitle",
        undefined,
        "A folder kept the same on your computers, phones and Docker, with its home on this console. Add a file anywhere and it reaches every device in the drive.",
      )}
      right={
        data?.supported ? (
          <Button variant="secondary" leftIcon={<Plus size={16} />} onClick={() => setCreating(true)}>
            {tr("drive_create", undefined, "New Mesh Drive")}
          </Button>
        ) : undefined
      }
    />
  );
  const sectionProps = {
    ref: sectionRef,
    id: MESH_DRIVE_ANCHOR,
    "aria-labelledby": "mesh-drive-heading",
    className: "scroll-mt-4 space-y-4",
  };

  if (!up || data?.supported === false) {
    const engineOld = data?.supported === false && data.engineOld;
    return (
      <section {...sectionProps}>
        {header}
        <Callout
          tone="info"
          title={
            engineOld
              ? tr("mesh_old_engine", undefined, "This app's engine is older than 8.0.0")
              : !up
                ? tr("drive_need_console", undefined, "Connect to a console to use Mesh Drive")
                : tr("mesh_old_helper", undefined, "Update the helper to see devices")
          }
        >
          {engineOld ? (
            <p>
              {tr(
                "mesh_old_engine_body",
                undefined,
                "The engine this app talks to has no device list yet. Restart the app; if it uses an engine elsewhere (a Docker container), update that to 8.0.0.",
              )}
            </p>
          ) : (
            <Button size="sm" variant="secondary" onClick={() => navigate("/connection")}>
              {tr("connect", undefined, "Connection")}
            </Button>
          )}
        </Callout>
        {!engineOld && <AwayFolders />}
      </section>
    );
  }

  return (
    <section {...sectionProps}>
      {header}
      {(error ?? pollError) && (
        <Callout tone="error" title={tr("mesh_failed", undefined, "That didn't work")}>
          {error ?? pollError}
        </Callout>
      )}
      {data?.supported && data.drives.length === 0 && (
        <div className="rounded-[var(--radius-card)] border border-dashed border-[var(--color-border)] p-6 text-center">
          <FolderSync size={32} className="mx-auto mb-2 text-[var(--color-accent-bright)]" aria-hidden />
          <p className="font-medium">{tr("drive_empty_title", undefined, "No Mesh Drive on this console yet")}</p>
          <p className="mx-auto mt-1 max-w-md text-sm text-[var(--color-muted)]">
            {tr(
              "drive_empty_body",
              undefined,
              "Make one: give it a name, a home folder on this console, and a folder on this device. Your other devices then join it with a folder of their own.",
            )}
          </p>
          <Button className="mt-3" variant="primary" leftIcon={<Plus size={16} />} onClick={() => setCreating(true)}>
            {tr("drive_create", undefined, "New Mesh Drive")}
          </Button>
        </div>
      )}
      {data?.supported &&
        data.drives.map((d) => (
          <DriveCard
            key={d.id}
            d={d}
            data={data}
            host={host}
            now={now}
            run={run}
            onJoin={(x) => setJoiningId(x.id)}
            onPassword={(x) => setLockingId(x.id)}
            onLink={(x) => setLinkingId(x.id)}
            onConflicts={(x) => setConflictsId(x.id)}
          />
        ))}
      {data?.supported && data.drives.length > 0 && (
        <details className="rounded-[var(--radius-card)] border border-[var(--color-border)] p-3 text-sm">
          <summary className="cursor-pointer font-medium">{tr("drive_how", undefined, "How it works")}</summary>
          <ul className="mt-2 list-disc space-y-1 ps-5 text-[var(--color-muted)]">
            <li>
              {tr(
                "drive_how_1",
                undefined,
                "Every device in the drive syncs with its home folder on the console every half minute while the console's helper runs. Docker keeps going with no app open.",
              )}
            </li>
            <li>
              {tr(
                "drive_how_5",
                undefined,
                "Any device can join from this page. To keep others out, give the drive a password; only its hash is kept on the console.",
              )}
            </li>
            <li>
              {tr(
                "drive_how_2",
                undefined,
                'Changed in two places at once: both are kept, one renamed "(conflict from …)".',
              )}
            </li>
            <li>
              {tr(
                "drive_how_3",
                undefined,
                "Deleted files go to the drive's .ps5upload-trash folder for 30 days. Deleting many files at once waits for you to confirm.",
              )}
            </li>
            <li>
              {tr(
                "drive_how_4",
                undefined,
                "A folder missing its .ps5upload-mesh file (an unplugged drive) is treated as offline, never as deleted.",
              )}
            </li>
          </ul>
        </details>
      )}
      {/* Mounted only while open: each opening starts empty. */}
      {creating && (
        <CreateDriveDialog
          host={host}
          open
          onClose={() => setCreating(false)}
          onDone={() => {
            setCreating(false);
            void load();
          }}
        />
      )}
      {joining && (
        <JoinDriveDialog
          key={joining.id}
          host={host}
          drive={joining}
          open
          onClose={() => setJoiningId(null)}
          onDone={() => {
            const name = joining.name;
            setJoiningId(null);
            useToastStore.getState().push({
              tone: "success",
              message: tr("drive_joined", { name }, `${name} is now kept on this device.`),
            });
            void load();
          }}
        />
      )}
      {linking && (
        <LinkDialog
          key={linking.id}
          host={host}
          drive={linking}
          onClose={() => setLinkingId(null)}
          onDone={() => {
            setLinkingId(null);
            void load();
          }}
        />
      )}
      {settling && (
        <ConflictsDialog
          key={settling.id}
          host={host}
          drive={settling}
          onClose={() => setConflictsId(null)}
          onChanged={() => void load()}
        />
      )}
      {locking && data?.supported && (
        <PasswordDialog
          key={locking.id}
          host={host}
          drive={locking}
          inDrive={locking.members.some((m) => m.device === data.me)}
          open
          onClose={() => setLockingId(null)}
          onDone={() => {
            setLockingId(null);
            void load();
          }}
        />
      )}
    </section>
  );
}
