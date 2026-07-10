import { useCallback, useEffect, useMemo, useState } from "react";
import { open as openDialog, confirm } from "@tauri-apps/plugin-dialog";
import {
  CircleUserRound,
  ImageIcon,
  Crop,
  Maximize2,
  UserPen,
  Check,
  Info,
  Loader2,
} from "lucide-react";

import {
  PageHeader,
  Card,
  Button,
  ErrorCard,
  SuccessCard,
  ConnectionGate,
  EmptyState,
} from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { mgmtAddr } from "../../lib/addr";
import { isTauriEnv } from "../../lib/tauriEnv";
import {
  profileInfo,
  profileApplyAvatar,
  profileAvatarPreview,
  profileAvatarCurrent,
  profileSetUsername,
  profileRenameUser,
  type ProfileInfo,
  type SquareMode,
} from "../../api/ps5";

/** Display label for a console user (name, or a hex-uid placeholder). */
function userLabel(uidHex: string, username: string): string {
  return username.trim() ? username : uidHex;
}

export default function ProfileScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const addr = host?.trim() ? mgmtAddr(host) : "";

  return (
    <div className="mx-auto max-w-3xl">
      <PageHeader
        icon={CircleUserRound}
        title={tr("profile.title", "Profile")}
        description={tr(
          "profile.description",
          "Change the console's profile avatar and the offline-account username.",
        )}
      />
      <ConnectionGate require="payload">
        {/* key on addr so switching console fully resets per-console profile
            state (avatar target uid, picked image, username draft). Without
            the remount, the prior console's targetUid/imagePath leak into the
            new console and an avatar Apply could write to the wrong uid. */}
        {addr ? <ProfileBody key={addr} addr={addr} /> : null}
      </ConnectionGate>
    </div>
  );
}

function ProfileBody({ addr }: { addr: string }) {
  const [info, setInfo] = useState<ProfileInfo | null>(null);
  const [loadingInfo, setLoadingInfo] = useState(false);
  const [infoError, setInfoError] = useState<string | null>(null);

  const refreshInfo = useCallback(async () => {
    setLoadingInfo(true);
    setInfoError(null);
    try {
      const r = await profileInfo(addr);
      setInfo(r);
    } catch (e) {
      setInfoError(`${e}`);
    } finally {
      setLoadingInfo(false);
    }
  }, [addr]);

  useEffect(() => {
    void refreshInfo();
  }, [refreshInfo]);

  return (
    <div className="space-y-6">
      {infoError && <ErrorCard title={infoError} />}
      <AvatarSection
        addr={addr}
        info={info}
        loadingInfo={loadingInfo}
        onApplied={refreshInfo}
      />
      <UsernameSection addr={addr} info={info} onChanged={refreshInfo} />
    </div>
  );
}

// ─── Avatar ──────────────────────────────────────────────────────────────────

function AvatarSection({
  addr,
  info,
  loadingInfo,
  onApplied,
}: {
  addr: string;
  info: ProfileInfo | null;
  loadingInfo: boolean;
  onApplied: () => void;
}) {
  const tr = useTr();
  const [imagePath, setImagePath] = useState<string | null>(null);
  const [mode, setMode] = useState<SquareMode>("crop");
  const [preview, setPreview] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [targetUid, setTargetUid] = useState<number | null>(null);
  // The selected user's CURRENT avatar (PNG data URL), shown in the picture box
  // by default until the user picks a new image. null = none / not yet loaded.
  const [currentAvatar, setCurrentAvatar] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const [applyOk, setApplyOk] = useState<string | null>(null);
  const [applyError, setApplyError] = useState<string | null>(null);

  // Build the selectable user list: every enumerated user, plus the
  // foreground user if it wasn't enumerated (uid != 0). Memoized so it's a
  // stable dependency for the target-defaulting effect.
  const users = useMemo(() => info?.users ?? [], [info]);
  const foreground = info && info.uid !== 0 ? info.uid : null;

  // Default the target once info arrives: foreground user, else the first
  // enumerated user.
  useEffect(() => {
    if (targetUid != null || !info) return;
    setTargetUid(foreground ?? users[0]?.uid ?? null);
  }, [info, foreground, users, targetUid]);

  // Load the selected user's CURRENT avatar into the picture box whenever the
  // target user (or console) changes. Best-effort: a user with a stock PSN
  // avatar has no readable PNG, so we just clear to the placeholder.
  useEffect(() => {
    if (targetUid == null) {
      setCurrentAvatar(null);
      return;
    }
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
    void (async () => {
      try {
        const url = await profileAvatarPreview(imagePath, mode);
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
  }, [imagePath, mode]);

  async function pickImage() {
    setApplyOk(null);
    setApplyError(null);
    const sel = await openDialog({
      multiple: false,
      filters: [
        {
          name: tr("profile.avatar.imageFilter", "Image"),
          extensions: ["png", "jpg", "jpeg", "webp", "bmp", "gif"],
        },
      ],
    });
    if (typeof sel === "string") setImagePath(sel);
  }

  async function applyAvatar() {
    if (!imagePath || targetUid == null) return;
    const u = users.find((x) => x.uid === targetUid);
    const label = u
      ? userLabel(u.uid_hex, u.username)
      : `0x${targetUid.toString(16).toUpperCase().padStart(8, "0")}`;
    const ok = await confirm(
      tr(
        "profile.avatar.confirmBody",
        { user: label },
        `Replace the profile avatar for ${label}? The current avatar is overwritten.`,
      ),
      { title: tr("profile.avatar.confirmTitle", "Change avatar?") },
    );
    if (!ok) return;
    setApplying(true);
    setApplyError(null);
    setApplyOk(null);
    try {
      const r = await profileApplyAvatar(
        imagePath,
        mode,
        targetUid,
        u?.username || null,
        addr,
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

  const fileName = imagePath ? imagePath.split(/[\\/]/).pop() : null;
  const noTarget = !loadingInfo && users.length === 0 && foreground == null;

  return (
    <Card>
      <div className="mb-4 flex items-center gap-2">
        <ImageIcon size={16} className="text-[var(--color-accent)]" />
        <h2 className="text-sm font-semibold">
          {tr("profile.avatar.title", "Avatar")}
        </h2>
      </div>

      {noTarget && (
        <div className="mb-3 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-3 text-xs text-[var(--color-muted)]">
          {tr(
            "profile.avatar.noUser",
            "No console user found. Sign in to a profile on the PS5, then refresh.",
          )}
        </div>
      )}

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
            {isTauriEnv() && (
              <Button variant="secondary" size="sm" onClick={pickImage}>
                {tr("profile.avatar.pick", "Choose image…")}
              </Button>
            )}
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
              onClick={() => setMode("crop")}
            />
            <ModeButton
              active={mode === "fit"}
              icon={<Maximize2 size={13} />}
              label={tr("profile.avatar.fit", "Fit")}
              onClick={() => setMode("fit")}
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

          {/* Target user */}
          <label className="flex flex-col gap-1 text-xs text-[var(--color-muted)]">
            {tr("profile.avatar.targetUser", "Apply to user")}
            <select
              className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1.5 text-sm text-[var(--color-text)]"
              value={targetUid ?? ""}
              onChange={(e) =>
                setTargetUid(e.target.value ? Number(e.target.value) : null)
              }
            >
              {foreground != null &&
                !users.some((u) => u.uid === foreground) && (
                  <option value={foreground}>
                    {userLabel(info?.uid_hex ?? "", info?.username ?? "")} (
                    {tr("profile.avatar.foreground", "active")})
                  </option>
                )}
              {users.map((u) => (
                <option key={u.uid} value={u.uid}>
                  {userLabel(u.uid_hex, u.username)}
                  {foreground === u.uid
                    ? ` (${tr("profile.avatar.foreground", "active")})`
                    : ""}
                </option>
              ))}
            </select>
          </label>

          <div className="flex items-center gap-2 pt-1">
            <Button
              variant="primary"
              size="sm"
              disabled={!imagePath || targetUid == null || applying}
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
    </Card>
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
      className={`flex items-center gap-1.5 rounded-md border px-3 py-1.5 text-xs font-medium transition ${
        active
          ? "border-[var(--color-accent)] bg-[var(--color-accent)]/10 text-[var(--color-accent)]"
          : "border-[var(--color-border)] text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
      }`}
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

  return (
    <Card>
      <div className="mb-1 flex items-center gap-2">
        <UserPen size={16} className="text-[var(--color-accent)]" />
        <h2 className="text-sm font-semibold">
          {tr("profile.username.title", "Username")}
        </h2>
      </div>
      <p className="mb-4 text-xs text-[var(--color-muted)]">
        {tr(
          "profile.username.description",
          "Rename a console user. The PS5 limits names to 16 characters.",
        )}
      </p>

      {info === null ? (
        // Still loading the first profile fetch. Showing the "No console
        // users" empty state here would briefly tell every connected user
        // they have no profiles — gate it on a completed load instead.
        <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
          <Loader2 size={12} className="animate-spin" />
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
        <div className="space-y-2">
          {users.map((u) => (
            <UserRow
              key={u.uid}
              addr={addr}
              uid={u.uid}
              uidHex={u.uid_hex}
              name={u.username}
              onChanged={onChanged}
            />
          ))}
        </div>
      )}

      {/* Offline-account slots are an advanced case — only shown when the
          console actually has them. */}
      {slots.length > 0 && (
        <div className="mt-5 border-t border-[var(--color-border)] pt-4">
          <h3 className="mb-2 text-xs font-semibold text-[var(--color-muted)]">
            {tr("profile.username.slotsTitle", "Offline-account slots")}
          </h3>
          <div className="space-y-2">
            {slots.map((s) => (
              <SlotRow
                key={s.slot}
                addr={addr}
                slot={s.slot}
                name={s.name}
                activated={s.activated}
                onChanged={onChanged}
              />
            ))}
          </div>
        </div>
      )}
    </Card>
  );
}

function UserRow({
  addr,
  uid,
  uidHex,
  name,
  onChanged,
}: {
  addr: string;
  uid: number;
  uidHex: string;
  name: string;
  onChanged: () => void;
}) {
  const tr = useTr();
  const [draft, setDraft] = useState(name);
  const [saved, setSaved] = useState(name);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedOk, setSavedOk] = useState(false);

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
    try {
      await profileRenameUser(uid, draft.trim(), addr);
      setSaved(draft.trim());
      setSavedOk(true);
      onChanged();
    } catch (e) {
      setError(`${e}`);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
      <div className="flex items-center gap-2">
        <span
          className="shrink-0 font-mono text-xs text-[var(--color-muted)]"
          title={uidHex}
        >
          {uidHex}
        </span>
        <input
          className="min-w-0 flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-sm"
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
      {error && <p className="mt-1 text-xs text-[var(--color-warn)]">{error}</p>}
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

function SlotRow({
  addr,
  slot,
  name,
  activated,
  onChanged,
}: {
  addr: string;
  slot: number;
  name: string;
  activated: boolean;
  onChanged: () => void;
}) {
  const tr = useTr();
  const [draft, setDraft] = useState(name);
  const [saved, setSaved] = useState(name);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedOk, setSavedOk] = useState(false);

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
    setSavedOk(false);
    try {
      await profileSetUsername(slot, draft.trim(), addr);
      setSaved(draft.trim());
      setSavedOk(true);
      onChanged();
    } catch (e) {
      setError(`${e}`);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
      <div className="flex items-center gap-2">
        <span className="w-12 shrink-0 text-xs tabular-nums text-[var(--color-muted)]">
          {tr("profile.username.slot", { n: slot }, `Slot ${slot}`)}
        </span>
        <input
          className="min-w-0 flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-sm"
          value={draft}
          maxLength={31}
          onChange={(e) => {
            setDraft(e.target.value);
            setSavedOk(false);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
          }}
        />
        {activated && (
          <span className="flex shrink-0 items-center gap-1 rounded-full bg-[var(--color-good)]/15 px-2 py-0.5 text-xs text-[var(--color-good)]">
            <Check size={11} />
            {tr("profile.username.activated", "active")}
          </span>
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
      {error && <p className="mt-1 text-xs text-[var(--color-warn)]">{error}</p>}
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
