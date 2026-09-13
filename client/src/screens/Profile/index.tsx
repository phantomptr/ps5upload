import { useCallback, useEffect, useMemo, useState } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  CircleUserRound,
  ImageIcon,
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
  Select,
} from "../../components";
import { useTr } from "../../state/lang";
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
  userCreate,
  userDelete,
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
    <div className="mx-auto max-w-3xl p-6">
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
          <Select
            label={tr("profile.avatar.targetUser", "Apply to user")}
            value={targetUid ?? ""}
            onChange={(e) =>
              setTargetUid(e.target.value ? Number(e.target.value) : null)
            }
          >
            {foreground != null && !users.some((u) => u.uid === foreground) && (
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
          </Select>

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

  const [showCreate, setShowCreate] = useState(false);

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

      {/* Create user */}
      {info !== null && (
        <div className="mt-4">
          {showCreate ? (
            <CreateUserRow
              addr={addr}
              onDone={() => {
                setShowCreate(false);
                onChanged();
              }}
              onCancel={() => setShowCreate(false)}
            />
          ) : (
            <Button
              variant="secondary"
              size="sm"
              onClick={() => setShowCreate(true)}
            >
              <UserPlus size={14} className="mr-1" />
              {tr("profile.username.createUser", "Create User")}
            </Button>
          )}
        </div>
      )}

      {/* These rows ARE the account on a normal console — a PSN-linked
          profile lives in slot 1 like any other. Calling the section
          "Offline-account slots" filed everyone's real account under an
          advanced case, which is how the account id ended up looking like
          something only offline activation cared about. Only say "offline"
          when one of the slots actually was activated that way. */}
      {slots.length > 0 && (
        <div className="mt-5 border-t border-[var(--color-border)] pt-4">
          <h3 className="mb-2 text-xs font-semibold text-[var(--color-muted)]">
            {slots.some((s) => s.offline_activated)
              ? tr("profile.username.slotsTitle", "Offline-account slots")
              : tr("profile.username.accountsTitle", "Accounts")}
          </h3>
          <div className="space-y-2">
            {slots.map((s) => (
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
  const [deleting, setDeleting] = useState(false);

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

  async function doDelete() {
    const confirmed = await confirm(
      tr(
        "profile.username.deleteConfirm",
        { name, uid },
        `Delete user "${name}" (uid ${uid})? This cannot be undone.`,
      ),
      {
        title: tr("profile.username.deleteTitle", "Delete User"),
        kind: "warning",
      },
    );
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
        <button
          type="button"
          onClick={doDelete}
          disabled={deleting}
          title={tr("profile.username.deleteTitle", "Delete User")}
          className="shrink-0 rounded-md border border-[var(--color-border)] p-1.5 text-[var(--color-warn)] hover:bg-[var(--color-surface-3)] disabled:opacity-50"
        >
          {deleting ? <Spinner size={14} /> : <Trash2 size={14} />}
        </button>
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
    </div>
  );
}

function CreateUserRow({
  addr,
  onDone,
  onCancel,
}: {
  addr: string;
  onDone: () => void;
  onCancel: () => void;
}) {
  const tr = useTr();
  const [name, setName] = useState("");
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function create() {
    const trimmed = name.trim();
    if (!trimmed) return;
    setCreating(true);
    setError(null);
    try {
      await userCreate(trimmed, addr);
      onDone();
    } catch (e) {
      setError(`${e}`);
    } finally {
      setCreating(false);
    }
  }

  return (
    <div className="rounded-md border border-[var(--color-accent)]/40 bg-[var(--color-surface)] p-2">
      <div className="flex items-center gap-2">
        <UserPlus size={14} className="shrink-0 text-[var(--color-accent)]" />
        <input
          autoFocus
          className="min-w-0 flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-sm"
          value={name}
          maxLength={16}
          placeholder={tr("profile.username.newPlaceholder", "New user name")}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void create();
            if (e.key === "Escape") onCancel();
          }}
        />
        <Button
          variant="primary"
          size="sm"
          disabled={!name.trim() || creating}
          loading={creating}
          onClick={create}
        >
          {tr("profile.username.createBtn", "Create")}
        </Button>
        <Button
          variant="ghost"
          size="sm"
          disabled={creating}
          onClick={onCancel}
        >
          {tr("profile.username.cancel", "Cancel")}
        </Button>
      </div>
      {error && <p className="mt-1 text-xs text-[var(--color-bad)]">{error}</p>}
    </div>
  );
}

/* An account id is 64 bits, so BigInt is the obvious tool and the wrong one:
 * the Vite build targets safari13 for old Android WebViews, and BigInt
 * literals are ES2020 that CANNOT be down-levelled — rolldown emits them
 * as-is with a TOLERATED_TRANSFORM warning, and on a WebView without BigInt
 * that is a parse-time SyntaxError for the whole chunk, taking out the entire
 * bundle rather than just this screen. (Same trap as lib/wakeState.ts; there
 * is now a lint rule so it cannot be walked into a third time.)
 *
 * These are string-to-string conversions, which are exact at any width.
 */

/** Decimal string -> lower-case hex, no prefix. Schoolbook repeated division,
 *  so it is exact for values far beyond 2^53. Null when not a decimal. */
function decimalToHex(dec: string): string | null {
  const trimmed = dec.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  let digits = trimmed.split("").map((c) => c.charCodeAt(0) - 48);
  const out: string[] = [];
  while (digits.length > 0) {
    let remainder = 0;
    const next: number[] = [];
    for (const d of digits) {
      const cur = remainder * 10 + d;
      const q = Math.floor(cur / 16);
      remainder = cur % 16;
      if (next.length > 0 || q > 0) next.push(q);
    }
    out.push("0123456789abcdef"[remainder]);
    digits = next;
  }
  return out.length > 0 ? out.reverse().join("") : "0";
}

/** Validate what the user typed and return it canonically as "0x…".
 *
 *  Accepts an optional 0x prefix and any case; rejects anything that is not
 *  1-16 hex digits, and rejects zero (zero means "no account", which is what
 *  clearing a slot is for). Returns a string rather than a number so a full
 *  64-bit id survives — see the note above. */
export function parseAccountId(raw: string): string | null {
  const t = raw.trim().replace(/^0x/i, "").toLowerCase();
  if (!/^[0-9a-f]{1,16}$/.test(t)) return null;
  const significant = t.replace(/^0+/, "");
  if (significant === "") return null;
  return "0x" + significant;
}

/** Normalise whatever the API sent into bare lower-case hex digits, or null.
 *
 *  The payload sends the id as 0x-prefixed hex ("0x%016llx" — see
 *  runtime.c), which is also how offact and every other tool talk about it.
 *  Decimal is accepted too: older builds sent it that way, and favourites
 *  and saved values outlive the build that wrote them.
 *
 *  Never goes through Number or BigInt — an account id is 64-bit, so the
 *  first would round it and the second does not exist on the WebViews we
 *  target. Hex is handled as text and decimal by long division. */
function accountIdHexDigits(id: string | null | undefined): string | null {
  if (!id) return null;
  const t = id.trim().toLowerCase();
  if (/^0x[0-9a-f]{1,16}$/.test(t)) return t.slice(2);
  if (/^\d+$/.test(t)) return decimalToHex(t);
  return null;
}

/** Render an id for display: 0x-prefixed lower-case hex, no padding.
 *
 *  "—" when the slot has no id, which is a real state and not an error. */
export function formatAccountId(id: string | null | undefined): string {
  const hex = accountIdHexDigits(id);
  if (hex === null) return "—";
  const significant = hex.replace(/^0+/, "");
  return significant === "" ? "—" : "0x" + significant;
}

/** Convert an account id to the base64 form Remote Play pairing wants.
 *
 *  Both are renderings of the same 8 registry bytes. offact reads those
 *  bytes straight into a `uint64_t` (offact.c:63) — on the console's x86-64
 *  that IS the little-endian reading — and the base64 is taken over the
 *  bytes in memory order, so the hex digits are emitted low byte first.
 *  Verified against both consoles: 0x7a356e99a9e2205c -> "XCDiqZluNXo=".
 *
 *  Bytes only, never a number: pairing rejects anything that does not
 *  decode to exactly 8 bytes, and a rounded id would decode to 8 perfectly
 *  valid bytes of the WRONG account. Returns "" when there is no id. */
export function accountIdToB64(id: string | null | undefined): string {
  const hex = accountIdHexDigits(id);
  if (hex === null || hex.replace(/^0+/, "") === "") return "";
  const padded = hex.padStart(16, "0");
  let bin = "";
  for (let i = 14; i >= 0; i -= 2) {
    bin += String.fromCharCode(parseInt(padded.slice(i, i + 2), 16));
  }
  try {
    return btoa(bin);
  } catch {
    return "";
  }
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
  const [savedOk, setSavedOk] = useState(false);
  const [editingId, setEditingId] = useState(false);
  const [idDraft, setIdDraft] = useState("");
  const [idBusy, setIdBusy] = useState(false);

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
    const ok = await confirm(
      hadId
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
      {
        title: tr("profile.accountId.confirm_title", "Change account ID?"),
        kind: "warning",
      },
    );
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
      {/* Account id. Read-only by default: this is the value the console uses
          to decide which saves belong to this profile, so it is shown plainly
          and changed only deliberately. */}
      <div className="mt-2 flex flex-wrap items-center gap-2 pl-14 text-xs">
        <span className="text-[var(--color-muted)]">
          {tr("profile.accountId.label", "Account ID")}
        </span>
        {editingId ? (
          <>
            <input
              className="w-52 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 font-mono text-xs"
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
    </div>
  );
}
