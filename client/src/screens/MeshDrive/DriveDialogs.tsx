// Making a Mesh Drive and joining one. No folder is picked for the user: the drive's home on the
// console is a new folder they name (in a place they pick) or an existing one they browse to, and
// every device picks its own folder. Each choice is checked by the engine as it is made, and the
// button stays off until both are usable.

import { useEffect, useRef, useState } from "react";
import { FolderOpen, FolderPlus, FolderSync, HardDrive, KeyRound, Monitor } from "lucide-react";

import { Button, Input, Modal, SegmentedControl } from "../../components";
import {
  checkFolders,
  consolePathOf,
  driveApi,
  refusalCode,
  type Drive,
  type FolderCheck,
  type HubChoice,
} from "../../api/meshDrive";
import { pickPath } from "../../lib/pickPath";
import { useTr } from "../../state/lang";
import { profileNameForAddr, useRosterStore } from "../../state/roster";
import { useToastStore } from "../../state/toasts";
import { FolderCheckNote } from "./FolderCheckNote";
import { humanizeCaught } from "../../lib/humanizeError";

/** Debounced engine check of whatever is chosen so far. */
export function useFolderCheck(
  host: string,
  q: { hub?: HubChoice; path?: string; drive?: string; console?: string } | null,
) {
  const [result, setResult] = useState<{ hub: FolderCheck | null; local: FolderCheck | null }>({ hub: null, local: null });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const key = JSON.stringify(q);
  const seq = useRef(0);
  useEffect(() => {
    const parsed = JSON.parse(key) as typeof q;
    // A new choice: the last one's answer no longer says anything (and one still on its way is
    // dropped by the sequence number).
    const mine = ++seq.current;
    setResult({ hub: null, local: null });
    setError(null);
    if (!parsed || (!parsed.hub && !parsed.path)) {
      setBusy(false);
      return;
    }
    setBusy(true);
    const t = window.setTimeout(() => {
      checkFolders(host, parsed)
        .then((r) => {
          if (mine === seq.current) {
            setResult(r);
            setError(null);
          }
        })
        .catch((e) => {
          if (mine === seq.current) setError(humanizeCaught(e));
        })
        .finally(() => {
          if (mine === seq.current) setBusy(false);
        });
    }, 400);
    return () => window.clearTimeout(t);
  }, [host, key]);
  return { ...result, busy, error };
}

export function PathPicker({
  label,
  value,
  placeholder,
  onPick,
  icon,
}: {
  label: string;
  value: string;
  placeholder: string;
  onPick: () => void;
  icon: React.ReactNode;
}) {
  const tr = useTr();
  return (
    <div className="space-y-1">
      <p className="text-sm font-medium">{label}</p>
      <div className="flex items-center gap-2">
        <div
          className={`min-w-0 flex-1 truncate rounded-[var(--radius-input,0.5rem)] border border-[var(--color-border)] px-3 py-2 text-sm ${value ? "" : "text-[var(--color-muted)]"}`}
          title={value || undefined}
        >
          {value || placeholder}
        </div>
        <Button variant="secondary" onClick={onPick} leftIcon={icon}>
          {value ? tr("drive_change", undefined, "Change…") : tr("browse", undefined, "Browse…")}
        </Button>
      </div>
    </div>
  );
}

/** This device's folder, picked by the user. */
function useLocalPick() {
  const tr = useTr();
  const [path, setPath] = useState("");
  const pick = async () => {
    const p = await pickPath({
      mode: "folder",
      title: tr("drive_pick_folder", undefined, "Choose a folder for this drive"),
    });
    if (p) setPath(p);
  };
  return { path, pick };
}

export function CreateDriveDialog({
  host,
  open,
  onClose,
  onDone,
}: {
  host: string;
  open: boolean;
  onClose: () => void;
  onDone: () => void;
}) {
  const tr = useTr();
  const [name, setName] = useState("");
  const [mode, setMode] = useState<"new" | "existing">("new");
  const [parent, setParent] = useState("");
  const [folderName, setFolderName] = useState("");
  const [folderNameTouched, setFolderNameTouched] = useState(false);
  const [existing, setExisting] = useState("");
  const local = useLocalPick();
  const [password, setPassword] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The new folder is named after the drive until the user names it.
  const effectiveFolder = folderNameTouched ? folderName : name.trim();
  const hub: HubChoice | undefined =
    mode === "new"
      ? parent
        ? { path: parent, name: effectiveFolder }
        : undefined
      : existing
        ? { path: existing }
        : undefined;
  const checks = useFolderCheck(host, open ? { hub, path: local.path || undefined } : null);
  const ready =
    !!name.trim() && !!hub && !!local.path && !!checks.hub?.ok && !!checks.local?.ok && !checks.busy && !saving;

  const browseConsole = async (set: (p: string) => void) => {
    const p = await pickPath({ mode: "folder", source: { console: host } });
    if (p) set(consolePathOf(p));
  };

  const useTheExisting = () => {
    if (!checks.hub) return;
    setExisting(checks.hub.path);
    setMode("existing");
  };

  const create = async () => {
    if (!hub) return;
    setSaving(true);
    setError(null);
    try {
      await driveApi.create(host, name.trim(), hub, local.path, password.trim());
      onDone();
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      size="lg"
      title={tr("drive_create", undefined, "New Mesh Drive")}
      titleIcon={<FolderSync size={18} />}
      footer={
        <div className="flex justify-end gap-2 px-5 py-3">
          <Button variant="ghost" onClick={onClose}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button variant="primary" disabled={!ready} loading={saving} onClick={() => void create()}>
            {tr("drive_create_go", undefined, "Create")}
          </Button>
        </div>
      }
    >
      <div className="space-y-5 px-5 py-4">
        <section className="space-y-1">
          <Input
            label={tr("drive_step_name", undefined, "1. Name the drive")}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={tr("drive_name_placeholder", undefined, "For example: Shared files")}
            maxLength={60}
            autoFocus
          />
        </section>

        <section className="space-y-2">
          <p className="text-sm font-medium">{tr("drive_step_home", undefined, "2. Its home on this console")}</p>
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "drive_step_home_hint",
              undefined,
              "Every device syncs with this folder. Pick a drive with room for what you will put in it.",
            )}
          </p>
          <SegmentedControl
            value={mode}
            onChange={(v) => setMode(v as "new" | "existing")}
            ariaLabel={tr("drive_step_home", undefined, "2. Its home on this console")}
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
                onPick={() => void browseConsole(setParent)}
                icon={<HardDrive size={14} />}
              />
              <Input
                label={tr("drive_folder_name", undefined, "Folder name")}
                value={effectiveFolder}
                onChange={(e) => {
                  setFolderNameTouched(true);
                  setFolderName(e.target.value);
                }}
                placeholder={tr("drive_folder_name_placeholder", undefined, "Named after the drive")}
                maxLength={200}
              />
            </div>
          ) : (
            <PathPicker
              label={tr("drive_home_folder", undefined, "Folder on the console")}
              value={existing}
              placeholder={tr("drive_home_folder_placeholder", undefined, "Choose a folder on the console")}
              onPick={() => void browseConsole(setExisting)}
              icon={<FolderOpen size={14} />}
            />
          )}
          <FolderCheckNote check={hub ? checks.hub : null} busy={!!hub && checks.busy} where="console" />
          {mode === "new" && checks.hub?.problem === "exists" && (
            <Button size="sm" variant="secondary" onClick={useTheExisting}>
              {tr("drive_use_existing", undefined, "Use that folder instead")}
            </Button>
          )}
        </section>

        <section className="space-y-2">
          <PathPicker
            label={tr("drive_step_local", undefined, "3. This device's copy")}
            value={local.path}
            placeholder={tr("drive_local_placeholder", undefined, "Choose a folder on this device")}
            onPick={() => void local.pick()}
            icon={<Monitor size={14} />}
          />
          <FolderCheckNote check={local.path ? checks.local : null} busy={!!local.path && checks.busy} where="local" />
        </section>

        <section className="space-y-1">
          <Input
            type="password"
            autoComplete="new-password"
            label={tr("drive_step_password", undefined, "4. Password (optional)")}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            placeholder={tr("drive_password_placeholder", undefined, "Leave empty: any device on your network can join")}
            maxLength={200}
          />
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "drive_password_hint",
              undefined,
              "Other devices type it once to join. Any device in the drive can change or remove it later.",
            )}
          </p>
        </section>

        {(error || checks.error) && <p className="text-sm text-[var(--color-bad)]">{error ?? checks.error}</p>}
      </div>
    </Modal>
  );
}

export function JoinDriveDialog({
  host,
  drive,
  open,
  onClose,
  onDone,
}: {
  host: string;
  drive: Drive | null;
  open: boolean;
  onClose: () => void;
  onDone: () => void;
}) {
  const tr = useTr();
  const local = useLocalPick();
  const [password, setPassword] = useState("");
  const locked = !!drive?.lock;
  const checks = useFolderCheck(host, open && drive && local.path ? { path: local.path, drive: drive.id } : null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ready =
    !!drive && !!local.path && !!checks.local?.ok && !checks.busy && !saving && (!locked || !!password.trim());

  const join = async () => {
    if (!drive) return;
    setSaving(true);
    setError(null);
    try {
      await driveApi.join(host, drive.id, local.path, locked ? password.trim() : undefined);
      onDone();
    } catch (e) {
      const code = refusalCode(e);
      setError(
        code === "wrong_password"
          ? tr("drive_wrong_password", undefined, "That is not the drive's password.")
          : code === "password_needed"
            ? tr("drive_password_needed", undefined, "This drive asks for a password now. Type it above.")
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
      open={open}
      onClose={onClose}
      size="lg"
      title={tr("drive_join_title", { name: drive?.name ?? "" }, `Keep ${drive?.name ?? ""} on this device`)}
      titleIcon={<FolderPlus size={18} />}
      footer={
        <div className="flex justify-end gap-2 px-5 py-3">
          <Button variant="ghost" onClick={onClose}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button variant="primary" disabled={!ready} loading={saving} onClick={() => void join()}>
            {tr("drive_join", undefined, "Join")}
          </Button>
        </div>
      }
    >
      <div className="space-y-3 px-5 py-4">
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "drive_join_body",
            undefined,
            "Pick the folder on this device that keeps the drive. Files already in it join the drive; the drive's files come here.",
          )}
        </p>
        <PathPicker
          label={tr("drive_step_local_join", undefined, "This device's copy")}
          value={local.path}
          placeholder={tr("drive_local_placeholder", undefined, "Choose a folder on this device")}
          onPick={() => void local.pick()}
          icon={<Monitor size={14} />}
        />
        <FolderCheckNote check={local.path ? checks.local : null} busy={!!local.path && checks.busy} where="local" />
        {locked && (
          <Input
            type="password"
            autoComplete="current-password"
            label={tr("drive_password", undefined, "The drive's password")}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            maxLength={200}
          />
        )}
        {(error || checks.error) && <p className="text-sm text-[var(--color-bad)]">{error ?? checks.error}</p>}
      </div>
    </Modal>
  );
}

/** Sets, changes or removes a drive's password. A device not in the drive types the current one. */
export function PasswordDialog({
  host,
  drive,
  inDrive,
  open,
  onClose,
  onDone,
}: {
  host: string;
  drive: Drive | null;
  inDrive: boolean;
  open: boolean;
  onClose: () => void;
  onDone: () => void;
}) {
  const tr = useTr();
  const [next, setNext] = useState("");
  const [current, setCurrent] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const locked = !!drive?.lock;
  const needCurrent = locked && !inDrive;

  const save = async (value: string) => {
    if (!drive) return;
    setSaving(true);
    setError(null);
    try {
      const { pending } = await driveApi.setPassword(host, drive.id, value, needCurrent ? current.trim() : undefined);
      // A linked console that was away still asks for the old password, until a device that
      // reaches both consoles carries the change over.
      if (pending.length > 0) {
        const profiles = useRosterStore.getState().profiles;
        useToastStore.getState().push({
          tone: "warn",
          message: tr(
            "drive_password_pending",
            { consoles: pending.map((c) => profileNameForAddr(c, profiles) || c).join(", ") },
            "{consoles} did not answer. It keeps the old password until a device reaches both consoles.",
          ),
        });
      }
      setNext("");
      setCurrent("");
      onDone();
    } catch (e) {
      setError(
        refusalCode(e) === "wrong_password"
          ? tr("drive_wrong_password", undefined, "That is not the drive's password.")
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
      open={open}
      onClose={onClose}
      title={tr("drive_password_title", { name: drive?.name ?? "" }, `Password for ${drive?.name ?? ""}`)}
      titleIcon={<KeyRound size={18} />}
      footer={
        <div className="flex flex-wrap justify-end gap-2 px-5 py-3">
          {locked && (
            <Button
              variant="ghost"
              disabled={saving || (needCurrent && !current)}
              onClick={() => void save("")}
            >
              {tr("drive_password_remove", undefined, "Remove password")}
            </Button>
          )}
          <Button variant="ghost" onClick={onClose}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            disabled={!next.trim() || saving || (needCurrent && !current)}
            loading={saving}
            onClick={() => void save(next.trim())}
          >
            {tr("save", undefined, "Save")}
          </Button>
        </div>
      }
    >
      <div className="space-y-3 px-5 py-4">
        <p className="text-sm text-[var(--color-muted)]">
          {locked
            ? tr(
                "drive_password_body_locked",
                undefined,
                "Devices already in the drive keep syncing whatever you choose; the password is asked only when a device joins.",
              )
            : tr(
                "drive_password_body_open",
                undefined,
                "Right now any device on your network can join. With a password, a device joining types it once. The console keeps only a hash of it.",
              )}
        </p>
        {needCurrent && (
          <Input
            type="password"
            autoComplete="current-password"
            label={tr("drive_password_current", undefined, "Current password")}
            value={current}
            onChange={(e) => setCurrent(e.target.value)}
            maxLength={200}
          />
        )}
        <Input
          type="password"
          autoComplete="new-password"
          label={tr("drive_password_new", undefined, "New password")}
          value={next}
          onChange={(e) => setNext(e.target.value)}
          maxLength={200}
        />
        {error && <p className="text-sm text-[var(--color-bad)]">{error}</p>}
      </div>
    </Modal>
  );
}
