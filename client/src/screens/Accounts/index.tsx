import { useCallback, useEffect, useRef, useState } from "react";
import {
  CircleUserRound,
  Crop,
  Maximize2,
  UserPen,
  Check,
  Copy,
  Info,
  UserPlus,
  Trash2,
} from "lucide-react";

import {
  PageHeader,
  Card,
  Button,
  ErrorCard,
  SuccessCard,
  ConnectionGate,
  EmptyState,
  Spinner,
  Badge,
} from "../../components";
import { useTr } from "../../state/lang";
import { useConfirm } from "../../components/ConfirmDialog";
import { NpSignInSection } from "./NpSignInSection";
import { accountIdToB64, formatAccountId, parseAccountId } from "./accountId";
import { UserInfoBackup } from "./UserInfoBackup";
import { userSlots } from "./userSlots";
import { RemotePlaySection } from "./RemotePlaySection";
import { useConnectionStore } from "../../state/connection";
import { mgmtAddr } from "../../lib/addr";
import { pickPath } from "../../lib/pickPath";
import { writeClipboard } from "../../lib/clipboard";
import { isTauriEnv } from "../../lib/tauriEnv";
import {
  profileInfo,
  profileApplyAvatar,
  profileAvatarPreview,
  profileAvatarCurrent,
  profileSetUsername,
  profileRenameUser,
  profileActivate,
  userDelete,
  type ProfileInfo,
  type ProfileSlot,
  type SquareMode,
} from "../../api/ps5";

/** Display label for a console user (name, or a hex-uid placeholder). */
function userLabel(uidHex: string, username: string): string {
  return username.trim() ? username : uidHex;
}

/** The account list's anchor: Remote Play's "Show accounts" scrolls here. */
const USERS_ANCHOR = "accounts-users";

/**
 * Accounts: the console's users and accounts, their avatars, and Remote Play.
 *
 * Profile and Remote Play were two screens. Remote Play needs an activated, signed-in account,
 * so its checklist kept sending people to Profile and back; on one page the order on screen is
 * the order things are done: the account (name, account ID), its avatar, then Remote Play,
 * whose sign-in step is Profile's np-fake-signin. Old /profile and /remote-play links land
 * here (/remote-play on its section).
 */
export default function AccountsScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const addr = host?.trim() ? mgmtAddr(host) : "";

  return (
    <div className="app-page">
      <PageHeader
        icon={CircleUserRound}
        title={tr("accounts_title", undefined, "Accounts")}
        description={tr(
          "accounts_description",
          undefined,
          "The console's users and accounts: rename them, give an account its ID, change avatars, and set up Remote Play.",
        )}
      />
      <ConnectionGate require="payload">
        {/* key on addr so switching console fully resets per-console profile
            state (avatar target uid, picked image, username draft). Without
            the remount, the prior console's targetUid/imagePath leak into the
            new console and an avatar Apply could write to the wrong uid. */}
        {addr ? <AccountsBody key={addr} addr={addr} /> : null}
      </ConnectionGate>
    </div>
  );
}

function AccountsBody({ addr }: { addr: string }) {
  const [info, setInfo] = useState<ProfileInfo | null>(null);

  const [infoError, setInfoError] = useState<string | null>(null);

  const refreshInfo = useCallback(async () => {
    setInfoError(null);
    try {
      const r = await profileInfo(addr);
      setInfo(r);
    } catch (e) {
      setInfoError(`${e}`);
    }
  }, [addr]);

  useEffect(() => {
    void refreshInfo();
  }, [refreshInfo]);

  return (
    <div className="space-y-6">
      {infoError && <ErrorCard title={infoError} onRetry={() => void refreshInfo()} />}
      <UsernameSection addr={addr} info={info} onChanged={refreshInfo} />
      <UserInfoBackup addr={addr} />

      <RemotePlaySection signIn={<NpSignInSection addr={addr} info={info} />} />
    </div>
  );
}

// ─── Avatar ──────────────────────────────────────────────────────────────────

/** Change one user's avatar: opened from that user's row, so the user is never picked twice. */
function AvatarEditor({
  addr,
  uid,
  label,
  username,
  onApplied,
}: {
  addr: string;
  uid: number;
  /** The user's name (or hex id), for the confirmation. */
  label: string;
  username: string;
  onApplied: () => void;
}) {
  const tr = useTr();
  const { confirm, dialog: confirmDialog } = useConfirm();
  const [imagePath, setImagePath] = useState<string | null>(null);
  const [browserFileName, setBrowserFileName] = useState<string | null>(null);
  const [readingImage, setReadingImage] = useState(false);
  const imageInput = useRef<HTMLInputElement>(null);
  const [mode, setMode] = useState<SquareMode>("crop");
  const [crop, setCrop] = useState({ x: 50, y: 50 });
  const [preview, setPreview] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const targetUid: number = uid;
  // The selected user's CURRENT avatar (PNG data URL), shown in the picture box
  // by default until the user picks a new image. null = none / not yet loaded.
  const [currentAvatar, setCurrentAvatar] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const [applyOk, setApplyOk] = useState<string | null>(null);
  const [applyError, setApplyError] = useState<string | null>(null);

  // Load the selected user's CURRENT avatar into the picture box whenever the
  // target user (or console) changes. Best-effort: a user with a stock PSN
  // avatar has no readable PNG, so we just clear to the placeholder.
  useEffect(() => {
    let cancelled = false;
    profileAvatarCurrent(targetUid, addr)
      .then((url) => {
        if (!cancelled) setCurrentAvatar(url);
      })
      .catch(() => {
        if (!cancelled) setCurrentAvatar(null);
      });
    return () => {
      cancelled = true;
    };
  }, [targetUid, addr]);

  // Regenerate the crop/fit preview whenever the image or mode changes.
  useEffect(() => {
    if (!imagePath) {
      setPreview(null);
      return;
    }
    let cancelled = false;
    setPreviewError(null);
    setPreview(null);
    void (async () => {
      try {
        const url = await profileAvatarPreview(imagePath, mode, crop);
        if (!cancelled) setPreview(url);
      } catch (e) {
        if (!cancelled) {
          setPreview(null);
          setPreviewError(`${e}`);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [imagePath, mode, crop]);

  async function readBrowserImage(file?: File) {
    if (!file) return; // Cancelling the picker keeps the current selection.
    setImagePath(null);
    setCrop({ x: 50, y: 50 });
    setBrowserFileName(null);
    setApplyOk(null);
    setApplyError(null);
    if (!file.size || file.size > 8 * 1024 * 1024) {
      setApplyError(tr("profile.avatar.imageSize", "Choose an image of 8 MiB or smaller."));
      return;
    }
    setReadingImage(true);
    try {
      const data = await new Promise<string>((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => typeof reader.result === "string"
          ? resolve(reader.result) : reject(new Error("Could not read image"));
        reader.onerror = () => reject(reader.error ?? new Error("Could not read image"));
        reader.onabort = () => reject(new Error("Image selection cancelled"));
        reader.readAsDataURL(file);
      });
      setImagePath(data);
      setBrowserFileName(file.name);
    } catch (e) {
      setApplyError(`${e}`);
    } finally {
      setReadingImage(false);
    }
  }

  async function pickImage() {
    if (!isTauriEnv()) {
      imageInput.current?.click();
      return;
    }
    setApplyOk(null);
    setApplyError(null);
    // `pickPath`, not plugin-dialog's `open` directly (#278). On Android the
    // native picker hands back a `content://` Storage Access Framework URI,
    // and the avatar path is read with `std::fs` by the engine — so the
    // preview failed with "HTTP 400 read image content://…" while every other
    // picker in the app worked, because they already route through here.
    // `pickPath` sends Android to the in-app real-path browser and keeps the
    // native dialog (filters and all) on desktop.
    const sel = await pickPath({
      mode: "file",
      filters: [
        {
          name: tr("profile.avatar.imageFilter", "Image"),
          extensions: ["png", "jpg", "jpeg", "webp", "bmp", "gif"],
        },
      ],
    });
    if (typeof sel === "string") {
      setBrowserFileName(null);
      setCrop({ x: 50, y: 50 });
      setImagePath(sel);
    }
  }

  async function applyAvatar() {
    if (!imagePath) return;
    const ok = await confirm({
      title: tr("profile.avatar.confirmTitle", undefined, "Change avatar?"),
      message: tr(
        "profile.avatar.confirmBody",
        { user: label },
        `Replace the profile avatar for ${label}? The current avatar is overwritten.`,
      ),
    });
    if (!ok) return;
    setApplying(true);
    setApplyError(null);
    setApplyOk(null);
    try {
      const r = await profileApplyAvatar(
        imagePath,
        mode,
        targetUid,
        username || null,
        addr,
        crop,
      );
      setApplyOk(
        tr(
          "profile.avatar.applied",
          { n: r.files_copied },
          // No "may take a moment to refresh" tail — the reboot notice
          // below is the single source of truth (you must restart to see
          // it), and the two together read as a contradiction.
          `Avatar applied (${r.files_copied} files written).`,
        ),
      );
      onApplied();
    } catch (e) {
      setApplyError(`${e}`);
    } finally {
      setApplying(false);
    }
  }

  const fileName = browserFileName ?? (imagePath ? imagePath.split(/[\\/]/).pop() : null);
  return (
    <div className="mt-3 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      {confirmDialog}
      <div className="flex flex-col gap-4 sm:flex-row">
        {/* Preview */}
        <div className="flex shrink-0 flex-col items-center gap-2">
          <div className="grid h-44 w-44 place-items-center overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)]">
            {/* A picked image's preview wins; otherwise show the selected
                user's CURRENT avatar; otherwise the placeholder. */}
            {preview || currentAvatar ? (
              <img
                src={preview ?? currentAvatar ?? ""}
                alt={tr("profile.avatar.previewAlt", "Avatar preview")}
                className="h-full w-full object-cover"
              />
            ) : (
              <CircleUserRound
                size={64}
                className="text-[var(--color-surface-3)]"
              />
            )}
          </div>
          {previewError && (
            <p className="max-w-44 text-center text-xs text-[var(--color-warn)]">
              {previewError}
            </p>
          )}
        </div>

        {/* Controls */}
        <div className="flex min-w-0 flex-1 flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <input
              ref={imageInput}
              type="file"
              accept="image/png,image/jpeg,image/webp,image/bmp,image/gif"
              hidden
              onChange={(event) => {
                const file = event.currentTarget.files?.[0];
                event.currentTarget.value = "";
                void readBrowserImage(file);
              }}
            />
            <Button variant="secondary" size="sm" onClick={pickImage} disabled={applying || readingImage}>
              {tr("profile.avatar.pick", "Choose image…")}
            </Button>
            {fileName && (
              <span className="truncate text-xs text-[var(--color-muted)]">
                {fileName}
              </span>
            )}
          </div>

          {/* Crop / fit toggle */}
          <div className="flex gap-2">
            <ModeButton
              active={mode === "crop"}
              icon={<Crop size={13} />}
              label={tr("profile.avatar.crop", "Crop")}
              onClick={() => { if (!applying) setMode("crop"); }}
            />
            <ModeButton
              active={mode === "fit"}
              icon={<Maximize2 size={13} />}
              label={tr("profile.avatar.fit", "Fit")}
              onClick={() => { if (!applying) setMode("fit"); }}
            />
          </div>
          <p className="text-xs text-[var(--color-muted)]">
            {mode === "crop"
              ? tr(
                  "profile.avatar.cropHint",
                  "Center-crop to a square (fills the frame, trims the long edges).",
                )
              : tr(
                  "profile.avatar.fitHint",
                  "Fit the whole image into a square (adds transparent bars).",
                )}
          </p>

          {imagePath && mode === "crop" && <div className="space-y-2">
            {(["x", "y"] as const).map((axis) => <label key={axis} className="block text-xs">
              {axis === "x" ? tr("profile.avatar.horizontal", "Horizontal position") : tr("profile.avatar.vertical", "Vertical position")}
              <input type="range" min={0} max={100} value={crop[axis]} disabled={applying}
                className="block w-full" onChange={(event) => setCrop((position) => ({ ...position, [axis]: Number(event.target.value) }))} />
            </label>)}
            <Button variant="secondary" disabled={applying} onClick={() => setCrop({ x: 50, y: 50 })}>
              {tr("profile.avatar.center", "Center image")}
            </Button>
          </div>}

          <div className="flex items-center gap-2 pt-1">
            <Button
              variant="primary"
              size="sm"
              disabled={!imagePath || !preview || !!previewError || applying || readingImage}
              loading={applying}
              onClick={applyAvatar}
            >
              {tr("profile.avatar.apply", "Apply avatar")}
            </Button>
          </div>

          {applyOk && <SuccessCard title={applyOk} />}
          {applyOk && (
            <p className="mt-2 flex items-start gap-1.5 text-xs text-[var(--color-muted)]">
              <Info size={12} className="mt-0.5 shrink-0" />
              {tr(
                "profile.reboot_notice",
                "Restart the PS5 to see this change — avatar and username updates only show on the console after a reboot.",
              )}
            </p>
          )}
          {applyError && <ErrorCard title={applyError} />}
        </div>
      </div>
    </div>
  );
}

function ModeButton({
  active,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className="chip min-h-8 gap-1.5 px-3.5 text-xs font-medium max-md:min-h-11"
    >
      {icon}
      {label}
    </button>
  );
}

// ─── Username (offline accounts) ─────────────────────────────────────────────

function UsernameSection({
  addr,
  info,
  onChanged,
}: {
  addr: string;
  info: ProfileInfo | null;
  onChanged: () => void;
}) {
  const tr = useTr();
  const users = info?.users ?? [];
  const slots = info?.slots ?? [];
  const slotOf = userSlots(users, slots);
  // Slots no user goes with (an offline-activated name with no user on this PS5).
  const otherSlots = slots.filter(
    (s) => ![...slotOf.values()].some((m) => m.slot === s.slot) && (s.name.trim() || s.activated),
  );

  return (
    <div id={USERS_ANCHOR} className="scroll-mt-4">
    <Card>
      <div className="mb-1 flex items-center gap-2">
        <UserPen size={16} className="text-[var(--color-accent)]" />
        <h2 className="text-sm font-semibold">
          {tr("accounts_users_title", undefined, "Users")}
        </h2>
      </div>
      <p className="mb-4 text-xs text-[var(--color-muted)]">
        {tr(
          "profile.username.description2",
          "Each user on this PS5 with their name (16 characters at most), avatar and account ID. Click the picture to change the avatar.",
        )}
      </p>

      {info === null ? (
        // Still loading the first profile fetch. Showing the "No console
        // users" empty state here would briefly tell every connected user
        // they have no profiles — gate it on a completed load instead.
        <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
          <Spinner size={12} />
          {tr("profile.username.loading", "Reading console users…")}
        </div>
      ) : users.length === 0 ? (
        <EmptyState
          title={tr("profile.username.empty", "No console users")}
          message={tr(
            "profile.username.emptyHint",
            "Sign in to a profile on the PS5, then refresh.",
          )}
        />
      ) : (
        <div className="card-flow">
          {users.map((u) => (
            <UserRow
              key={u.uid}
              addr={addr}
              uid={u.uid}
              uidHex={u.uid_hex}
              name={u.username}
              slot={slotOf.get(u.uid) ?? null}
              onChanged={onChanged}
            />
          ))}
        </div>
      )}

      {/* The console has no create-user call a payload can reach (#460):
          say where to do it instead of offering a button that can't work. */}
      {info !== null && (
        <p className="mt-4 flex items-start gap-1.5 text-xs text-[var(--color-muted)]">
          <UserPlus size={14} className="mt-px shrink-0" />
          {tr(
            "profile.username.addOnConsole",
            "To add a user, do it on the PS5: Switch User, then Add User.",
          )}
        </p>
      )}

      {/* These rows ARE the account on a normal console — a PSN-linked
          profile lives in slot 1 like any other. Calling the section
          "Offline-account slots" filed everyone's real account under an
          advanced case, which is how the account id ended up looking like
          something only offline activation cared about. Only say "offline"
          when one of the slots actually was activated that way. */}
      {otherSlots.length > 0 && (
        <div className="mt-5 border-t border-[var(--color-border)] pt-4">
          <h3 className="mb-2 text-xs font-semibold text-[var(--color-muted)]">
            {tr("profile.username.otherSlotsTitle", "Other account slots")}
          </h3>
          <div className="space-y-2">
            {otherSlots.map((s) => (
              <SlotRow
                key={s.slot}
                addr={addr}
                slot={s.slot}
                name={s.name}
                accountId={s.id}
                activated={s.activated}
                onChanged={onChanged}
              />
            ))}
          </div>
        </div>
      )}
    </Card>
    </div>
  );
}

function UserRow({
  addr,
  uid,
  uidHex,
  name,
  slot,
  onChanged,
}: {
  addr: string;
  uid: number;
  uidHex: string;
  name: string;
  /** The account slot that goes with this user (its name is the user's), if one does. */
  slot: ProfileSlot | null;
  onChanged: () => void;
}) {
  const tr = useTr();
  const { confirm, dialog: confirmDialog } = useConfirm();
  const [draft, setDraft] = useState(name);
  const [saved, setSaved] = useState(name);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedOk, setSavedOk] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [avatarOpen, setAvatarOpen] = useState(false);
  const [avatar, setAvatar] = useState<string | null>(null);
  const [avatarRev, setAvatarRev] = useState(0);
  useEffect(() => {
    let live = true;
    profileAvatarCurrent(uid, addr)
      .then((url) => live && setAvatar(url))
      .catch(() => live && setAvatar(null));
    return () => {
      live = false;
    };
  }, [uid, addr, avatarRev]);

  useEffect(() => {
    setDraft(name);
    setSaved(name);
  }, [name]);

  const dirty = draft.trim() !== saved && draft.trim().length > 0;

  async function save() {
    if (!dirty) return;
    setSaving(true);
    setError(null);
    setSavedOk(false);
    let renamed = false;
    try {
      await profileRenameUser(uid, draft.trim(), addr);
      renamed = true;
      // The slot carries the same name (it is how the account is found for this user): keep
      // them one name, so there is one field to edit.
      if (slot) await profileSetUsername(slot.slot, draft.trim(), addr);
      setSaved(draft.trim());
      setSavedOk(true);
    } catch (e) {
      setError(`${e}`);
    } finally {
      setSaving(false);
      // Reread whenever the console changed, even if the slot half failed.
      if (renamed) onChanged();
    }
  }

  async function doDelete() {
    const confirmed = await confirm({
      title: tr("profile.username.deleteTitle", undefined, "Delete User"),
      message: tr(
        "profile.username.deleteConfirm",
        { name, uid },
        `Delete user "${name}" (uid ${uid})? This cannot be undone.`,
      ),
      destructive: true,
    });
    if (!confirmed) return;
    setDeleting(true);
    setError(null);
    try {
      await userDelete(uid, true, addr);
      onChanged();
    } catch (e) {
      setError(`${e}`);
    } finally {
      setDeleting(false);
    }
  }

  return (
    <div className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
      {confirmDialog}
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => setAvatarOpen((v) => !v)}
          title={tr("profile.avatar.change", undefined, "Change avatar")}
          aria-expanded={avatarOpen}
          className="grid h-9 w-9 shrink-0 place-items-center overflow-hidden rounded-full border border-[var(--color-border)] bg-[var(--color-surface-2)] hover:ring-2 hover:ring-[var(--color-accent)]"
        >
          {avatar ? (
            <img src={avatar} alt="" className="h-full w-full object-cover" />
          ) : (
            <CircleUserRound size={18} className="text-[var(--color-muted)]" />
          )}
        </button>
        <input
          className="min-w-0 flex-1 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-2 py-1 text-sm"
          value={draft}
          maxLength={16}
          placeholder={tr("profile.username.placeholder", "User name")}
          onChange={(e) => {
            setDraft(e.target.value);
            setSavedOk(false);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
          }}
        />
        {slot?.activated && (
          <Badge tone="good" variant="soft" size="md" icon={Check}>
            {tr("profile.username.activated", "active")}
          </Badge>
        )}
        <Button
          variant="secondary"
          size="sm"
          disabled={!dirty || saving}
          loading={saving}
          onClick={save}
        >
          {tr("profile.username.save", "Save")}
        </Button>
        <button
          type="button"
          onClick={doDelete}
          disabled={deleting}
          title={tr("profile.username.deleteTitle", "Delete User")}
          className="shrink-0 rounded-full border border-[var(--color-border)] p-1.5 text-[var(--color-warn)] hover:bg-[var(--color-surface-3)] disabled:opacity-50"
        >
          {deleting ? <Spinner size={14} /> : <Trash2 size={14} />}
        </button>
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-2 text-xs">
        <span className="font-mono text-[var(--color-muted)]" title={tr("profile.user_id", undefined, "User ID")}>
          {uidHex}
        </span>
        <button
          type="button"
          className="text-[var(--color-accent)] hover:underline"
          onClick={() => setAvatarOpen((v) => !v)}
        >
          {tr("profile.avatar.change", undefined, "Change avatar")}
        </button>
      </div>
      {avatarOpen && (
        <AvatarEditor
          addr={addr}
          uid={uid}
          label={userLabel(uidHex, name)}
          username={name}
          onApplied={() => {
            setAvatarRev((n) => n + 1);
            onChanged();
          }}
        />
      )}
      {slot && (
        <AccountIdLine addr={addr} slot={slot.slot} accountId={slot.id} onChanged={onChanged} />
      )}
      {error && <p className="mt-1 text-xs text-[var(--color-bad)]">{error}</p>}
      {savedOk && (
        <p className="mt-1 text-xs text-[var(--color-good)]">
          {tr(
            "profile.reboot_notice",
            "Restart the PS5 to see this change — avatar and username updates only show on the console after a reboot.",
          )}
        </p>
      )}
    </div>
  );
}

/** A monospace id with a copy button. "—" is a real state, not a value, so
 *  it renders plainly with nothing to copy. */
function CopyableId({
  value,
  tr,
}: {
  value: string;
  tr: ReturnType<typeof useTr>;
}) {
  const [copied, setCopied] = useState(false);
  if (!value || value === "—")
    return <span className="font-mono">{value || "—"}</span>;
  return (
    <span className="inline-flex items-center gap-1">
      <span className="font-mono">{value}</span>
      <button
        type="button"
        title={tr("copy", undefined, "Copy")}
        className="text-[var(--color-muted)] hover:text-[var(--color-accent)]"
        onClick={() => {
          void writeClipboard(value).then((ok) => {
            if (!ok) return;
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          });
        }}
      >
        {copied ? <Check size={12} /> : <Copy size={12} />}
      </button>
    </span>
  );
}

/** A slot's account ID: shown plainly (hex, and the encoding Remote Play pairing wants), changed
 *  only deliberately. */
function AccountIdLine({
  addr,
  slot,
  accountId,
  onChanged,
}: {
  addr: string;
  slot: number;
  accountId: string;
  onChanged: () => void;
}) {
  const tr = useTr();
  const { confirm, dialog: confirmDialog } = useConfirm();
  const [error, setError] = useState<string | null>(null);
  const [savedOk, setSavedOk] = useState(false);
  const [editingId, setEditingId] = useState(false);
  const [idDraft, setIdDraft] = useState("");
  const [idBusy, setIdBusy] = useState(false);

  /** Write a new account id to this slot.
   *
   *  Gated behind an explicit confirm that names the actual consequence. The
   *  account id is what the console matches saves against, so changing it on
   *  a slot that already has one makes that profile's existing saves
   *  unreadable until the old id is put back — which is why the dialog shows
   *  the old value: it is the only copy the user will get.
   */
  async function saveAccountId() {
    const next = parseAccountId(idDraft);
    if (next === null) return;
    const current = formatAccountId(accountId);
    const hadId = current !== "—";
    const ok = await confirm({
      title: tr("profile.accountId.confirm_title", undefined, "Change account ID?"),
      message: hadId
        ? tr(
            "profile.accountId.confirm_replace",
            { slot: String(slot), old: current, next },
            `Slot ${slot} already has account ID ${current}.\n\nChanging it to ${next} means saves made under ${current} will no longer be recognised as belonging to this profile. They are not deleted — putting ${current} back restores access.\n\nWrite it down before continuing. Change the account ID?`,
          )
        : tr(
            "profile.accountId.confirm_set",
            { slot: String(slot), next },
            `Set slot ${slot}'s account ID to ${next}?\n\nThis activates the offline account. Saves made from now on are tied to this ID — if you change it later they will stop being recognised until you set it back.`,
          ),
      destructive: hadId,
    });
    if (!ok) return;
    setIdBusy(true);
    setError(null);
    try {
      // profileActivate writes the id and sets the slot's activated flags —
      // the same path offact uses. Passing an explicit id skips the
      // derive-from-name behaviour. Sent as a hex STRING: Number() would
      // round any id above 2^53 and activate a different account.
      await profileActivate(slot, next, addr);
      setEditingId(false);
      setSavedOk(true);
      onChanged();
    } catch (e) {
      setError(`${e}`);
    } finally {
      setIdBusy(false);
    }
  }

  return (
    <>
      {confirmDialog}
      {/* Account id. Read-only by default: this is the value the console uses
          to decide which saves belong to this profile, so it is shown plainly
          and changed only deliberately. */}
      <div className="mt-2 flex flex-wrap items-center gap-2 text-xs">
        <span className="text-[var(--color-muted)]">
          {tr("profile.accountId.label", "Account ID")}
        </span>
        {editingId ? (
          <>
            <input
              className="w-52 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-2 py-1 font-mono text-xs"
              value={idDraft}
              spellCheck={false}
              placeholder="0x0123456789abcdef"
              onChange={(e) => setIdDraft(e.target.value)}
            />
            <Button
              variant="secondary"
              size="sm"
              disabled={idBusy || parseAccountId(idDraft) === null}
              loading={idBusy}
              onClick={() => void saveAccountId()}
            >
              {tr("profile.accountId.apply", "Apply")}
            </Button>
            <button
              type="button"
              className="text-[var(--color-muted)] hover:underline"
              onClick={() => setEditingId(false)}
            >
              {tr("cancel", undefined, "Cancel")}
            </button>
            {idDraft.trim() !== "" && parseAccountId(idDraft) === null && (
              <span className="text-[var(--color-bad)]">
                {tr(
                  "profile.accountId.invalid",
                  "Enter 1-16 hex digits (not zero).",
                )}
              </span>
            )}
          </>
        ) : (
          <>
            <CopyableId value={formatAccountId(accountId)} tr={tr} />
            {/* The same 8 bytes in the encoding Remote Play pairing wants.
                Two screens in this app both say "account ID" and want
                different encodings, so pasting the hex into the Remote Play
                field fails — showing both here is what makes that
                survivable without explaining it. */}
            {accountIdToB64(accountId) && (
              <>
                <span className="text-[var(--color-muted)]">
                  {tr("profile.accountId.b64_label", "for Remote Play")}
                </span>
                <CopyableId value={accountIdToB64(accountId)} tr={tr} />
              </>
            )}
            <button
              type="button"
              className="text-[var(--color-accent)] hover:underline"
              onClick={() => {
                setIdDraft(
                  formatAccountId(accountId) === "—"
                    ? ""
                    : formatAccountId(accountId),
                );
                setEditingId(true);
              }}
            >
              {tr("profile.accountId.change", "Change")}
            </button>
          </>
        )}
      </div>
      {error && <p className="mt-1 text-xs text-[var(--color-bad)]">{error}</p>}
      {savedOk && (
        <p className="mt-1 text-xs text-[var(--color-good)]">
          {tr(
            "profile.reboot_notice",
            "Restart the PS5 to see this change — avatar and username updates only show on the console after a reboot.",
          )}
        </p>
      )}
    </>
  );
}

/** An account slot no console user goes with (a slot activated offline for a name that has no
 *  user on this PS5): its name and account ID. */
function SlotRow({
  addr,
  slot,
  name,
  accountId,
  activated,
  onChanged,
}: {
  addr: string;
  slot: number;
  name: string;
  accountId: string;
  activated: boolean;
  onChanged: () => void;
}) {
  const tr = useTr();
  const [draft, setDraft] = useState(name);
  const [saved, setSaved] = useState(name);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Re-sync when the server-confirmed name changes (after a refetch).
  useEffect(() => {
    setDraft(name);
    setSaved(name);
  }, [name]);

  const dirty = draft.trim() !== saved && draft.trim().length > 0;

  async function save() {
    if (!dirty) return;
    setSaving(true);
    setError(null);
    try {
      await profileSetUsername(slot, draft.trim(), addr);
      setSaved(draft.trim());
      onChanged();
    } catch (e) {
      setError(`${e}`);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
      <div className="flex items-center gap-2">
        <span className="w-12 shrink-0 text-xs tabular-nums text-[var(--color-muted)]">
          {tr("profile.username.slot", { n: slot }, `Slot ${slot}`)}
        </span>
        <input
          className="min-w-0 flex-1 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-2 py-1 text-sm"
          value={draft}
          maxLength={31}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
          }}
        />
        {activated && (
          <Badge tone="good" variant="soft" size="md" icon={Check}>
            {tr("profile.username.activated", "active")}
          </Badge>
        )}
        <Button
          variant="secondary"
          size="sm"
          disabled={!dirty || saving}
          loading={saving}
          onClick={save}
        >
          {tr("profile.username.save", "Save")}
        </Button>
      </div>
      {error && <p className="mt-1 text-xs text-[var(--color-bad)]">{error}</p>}
      <AccountIdLine addr={addr} slot={slot} accountId={accountId} onChanged={onChanged} />
    </div>
  );
}
