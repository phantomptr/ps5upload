// Games › an app › Share (and Collection › an item › Share): pick what to send a friend, get a
// link and QR code, then allow or decline each friend who asks. The engine (share/mod.rs) checks
// the app again and refuses a retail game whatever this page sends.

import { useEffect, useMemo, useState } from "react";
import { Check, Copy, Share2, X } from "lucide-react";

import type { CollectionLocation } from "../../api/collection";
import { cheatsGet, savesList, screenshotsList, videosList, type SaveEntry } from "../../api/ps5";
import {
  shareCancel,
  shareCheck,
  shareCreate,
  shareDecide,
  shareGet,
  type ShareView,
} from "../../api/share";
import { Button, Callout, Checkbox, Input, Modal, ProgressBar, Select, Textarea, Toggle } from "../../components";
import { consoleAddr } from "../../lib/addr";
import { writeClipboard } from "../../lib/clipboard";
import { formatBytes } from "../../lib/format";
import { loadProfiles } from "../../lib/padProfiles";
import { useGameControlsStore } from "../../state/gameControls";
import { useKbmStore } from "../../state/kbm";
import { useTr } from "../../state/lang";
import { useRemotePlayStore } from "../../state/remotePlay";
import { QrCode } from "../Play/PhonePads";
import { ShareHistory } from "./ShareHistory";
import {
  DEFAULT_EXPIRY,
  EXPIRY_CHOICES,
  NOTE_MAX,
  appItem,
  appSources,
  createBody,
  mediaOf,
  percent,
  saveShareName,
  savedShareName,
  type AppSource,
} from "./shareModel";

export interface ShareTarget {
  titleId: string;
  title: string;
  /** The connected console's host, when its helper answers. */
  host: string | null;
  /** The app's folder on that console, when it is one. */
  consolePath: string | null;
  /** The Collection's copies of it. */
  copies: CollectionLocation[];
}

const POLL_MS = 1500;

export function SharePanel({ target, onClose }: { target: ShareTarget; onClose: () => void }) {
  const tr = useTr();
  const [made, setMade] = useState<ShareView | null>(null);
  return (
    <Modal
      open
      onClose={onClose}
      title={tr("share_title", { title: target.title }, "Share {title}")}
      titleIcon={<Share2 size={16} />}
      size="lg"
      variant="sheet"
    >
      <div data-testid="share-panel" className="space-y-4">
        {made ? (
          <ShareMade share={made} onChange={setMade} />
        ) : (
          <SharePick target={target} onMade={setMade} />
        )}
        <ShareHistory />
      </div>
    </Modal>
  );
}

export function SharePick({
  target,
  onMade,
  verdict = null,
}: {
  target: ShareTarget;
  onMade: (s: ShareView) => void;
  /** The engine's verdict on the app to start from (tests; the panel asks it). */
  verdict?: string | null;
}) {
  const tr = useTr();
  const addr = target.host ? consoleAddr(target.host) : null;
  const sources = useMemo(() => appSources(target.consolePath, target.copies), [target.consolePath, target.copies]);
  const [source, setSource] = useState(0);
  const app: AppSource | null = sources[source] ?? null;
  const [withApp, setWithApp] = useState(true);
  // The engine's verdict on the app: null while asked, "" when it may be shared.
  const [refusal, setRefusal] = useState<string | null>(verdict);
  const [saves, setSaves] = useState<SaveEntry[]>([]);
  const [pickedSaves, setPickedSaves] = useState<string[]>([]);
  const [media, setMedia] = useState<string[]>([]);
  const [withMedia, setWithMedia] = useState(false);
  const [cheats, setCheats] = useState<{ name: string; on: boolean }[]>([]);
  const [withCheats, setWithCheats] = useState(false);
  const gameControls = useGameControlsStore((s) => s.byTitle[target.titleId.toUpperCase()] ?? null);
  const kbmMode = useKbmStore((s) => s.modes[target.titleId.toUpperCase()] ?? null);
  const [withControls, setWithControls] = useState(false);
  const [note, setNote] = useState("");
  const [from, setFrom] = useState(() => savedShareName());
  const [expires, setExpires] = useState<number>(DEFAULT_EXPIRY);
  const [oneUse, setOneUse] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The app: asked of the engine (retail is refused there, not here).
  useEffect(() => {
    if (!app) {
      setRefusal("");
      return;
    }
    let live = true;
    setRefusal(null);
    shareCheck({ title_id: target.titleId, items: [appItem(app, addr)] })
      .then((r) => live && setRefusal(r[0] && !r[0].ok ? (r[0].error ?? "") || "?" : ""))
      .catch(() => live && setRefusal(""));
    return () => {
      live = false;
    };
  }, [app, addr, target.titleId]);

  // What the console has of this game: saves, screenshots and clips, cheats.
  useEffect(() => {
    if (!addr) return;
    let live = true;
    savesList(addr, 0)
      .then((r) => {
        if (!live) return;
        setSaves(r.saves.filter((s) => s.title_id === target.titleId && s.kind === "ps5"));
      })
      .catch(() => {});
    Promise.all([screenshotsList(addr).catch(() => ({ items: [] })), videosList(addr).catch(() => ({ items: [] }))])
      .then(([s, v]) => live && setMedia(mediaOf([...s.items, ...v.items], target.titleId, target.title)))
      .catch(() => {});
    cheatsGet(target.titleId, addr)
      .then((r) => live && setCheats(r.mods.map((m) => ({ name: m.name, on: m.on }))))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [addr, target.titleId, target.title]);

  const appOk = !!app && withApp && refusal === "";
  const controls = withControls
    ? {
        title_id: target.titleId,
        game: gameControls,
        kbm_mode: kbmMode,
        keymap: useRemotePlayStore.getState().keymap,
        pad_profiles: loadProfiles(),
      }
    : null;
  const body = createBody({
    from,
    title: target.title,
    titleId: target.titleId,
    console: addr,
    app: appOk ? app : null,
    saves: pickedSaves,
    media: withMedia ? media : [],
    cheats: withCheats ? cheats : null,
    controls,
    note,
    expiresInS: expires,
    oneUse,
  });
  const nothing = body.items.length === 0;

  const share = async () => {
    setBusy(true);
    setError(null);
    saveShareName(from);
    try {
      onMade(await shareCreate(body));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const expiryLabel = (s: number) =>
    s === 3600
      ? tr("share_expiry_1h", undefined, "1 hour")
      : s === 24 * 3600
        ? tr("share_expiry_24h", undefined, "24 hours")
        : tr("share_expiry_7d", undefined, "7 days");

  return (
    <div className="space-y-4" data-testid="share-pick">
      <p className="text-sm text-[var(--color-muted)]">
        {tr(
          "share_intro",
          undefined,
          "Choose what to send. Your friend opens the link, picks what they want, and you allow it. The files go straight from this computer to theirs.",
        )}
      </p>

      <fieldset className="space-y-2">
        <legend className="text-sm font-semibold">{tr("share_what", undefined, "What to share")}</legend>
        {sources.length > 0 && (
          <div className="space-y-1" data-testid="share-app">
            <Checkbox
              checked={withApp && refusal === ""}
              disabled={refusal !== ""}
              onChange={setWithApp}
              label={tr("share_app", undefined, "The app itself")}
            />
            {sources.length > 1 && (
              <Select
                aria-label={tr("share_app_from", undefined, "Share the app from")}
                value={String(source)}
                onChange={(e) => setSource(Number(e.target.value))}
                options={sources.map((s, i) => ({
                  value: String(i),
                  label: s.kind === "console" ? tr("share_app_console", undefined, "The PS5's copy") : s.label,
                }))}
              />
            )}
            {refusal === null && (
              <p className="text-xs text-[var(--color-muted)]">{tr("share_app_checking", undefined, "Checking the app…")}</p>
            )}
            {refusal && (
              <p className="text-xs text-[var(--color-warn)]" data-testid="share-app-refused">
                {refusal}
              </p>
            )}
          </div>
        )}
        {saves.map((s) => (
          <Checkbox
            key={s.path}
            checked={pickedSaves.includes(s.path)}
            onChange={(on) => setPickedSaves((p) => (on ? [...p, s.path] : p.filter((x) => x !== s.path)))}
            label={tr("share_save_of", { size: formatBytes(s.size) }, "A save ({size})")}
            hint={tr("share_save_hint", undefined, "Your progress in the game. Trophies and your account stay yours.")}
          />
        ))}
        {media.length > 0 && (
          <Checkbox
            checked={withMedia}
            onChange={setWithMedia}
            label={tr("share_media", { n: media.length }, "Screenshots and clips: {n}")}
          />
        )}
        {cheats.length > 0 && (
          <Checkbox
            checked={withCheats}
            onChange={setWithCheats}
            label={tr("share_cheats", { n: cheats.filter((c) => c.on).length }, "Cheats and which are on (on now: {n})")}
          />
        )}
        <Checkbox
          checked={withControls}
          onChange={setWithControls}
          label={tr("share_controls", undefined, "Controls: button remaps, keyboard and mouse, macros")}
        />
        <p className="text-xs text-[var(--color-muted)]" data-testid="share-stream-none">
          {tr("share_stream_none", undefined, "Stream settings aren't kept per game, so there are none to share.")}
        </p>
        <Textarea
          label={tr("share_note", undefined, "A short note (optional)")}
          value={note}
          maxLength={NOTE_MAX}
          rows={2}
          onChange={(e) => setNote(e.target.value)}
        />
      </fieldset>

      <div className="grid gap-3 sm:grid-cols-2">
        <Input
          label={tr("share_your_name", undefined, "Your name")}
          value={from}
          maxLength={40}
          onChange={(e) => setFrom(e.target.value)}
          data-testid="share-name"
        />
        <Select
          label={tr("share_expiry", undefined, "The link works for")}
          value={String(expires)}
          onChange={(e) => setExpires(Number(e.target.value))}
          options={EXPIRY_CHOICES.map((s) => ({ value: String(s), label: expiryLabel(s) }))}
        />
      </div>
      <Toggle
        checked={oneUse}
        onChange={setOneUse}
        label={tr("share_one_use", undefined, "One friend only")}
        hint={tr("share_one_use_hint", undefined, "The link stops working once you allow someone.")}
      />

      {error && (
        <Callout tone="error" title={tr("share_failed", undefined, "Couldn't make the share")}>
          <span data-testid="share-error">{error}</span>
        </Callout>
      )}
      <div className="flex flex-wrap justify-end gap-2">
        <Button
          variant="primary"
          size="md"
          leftIcon={<Share2 size={14} />}
          disabled={nothing || busy || refusal === null}
          loading={busy}
          onClick={() => void share()}
          data-testid="share-go"
        >
          {tr("share_go", undefined, "Share")}
        </Button>
      </div>
    </div>
  );
}

export function ShareMade({ share, onChange }: { share: ShareView; onChange: (s: ShareView) => void }) {
  const tr = useTr();
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const live = share.state === "open" || share.state === "used";
  const id = share.id;

  // The friends asking, while the panel is open (read on demand, nothing in the background).
  useEffect(() => {
    if (!live) return;
    let on = true;
    const t = setInterval(() => {
      shareGet(id)
        .then((s) => on && onChange(s))
        .catch(() => {});
    }, POLL_MS);
    return () => {
      on = false;
      clearInterval(t);
    };
  }, [id, live, onChange]);

  const act = async (f: () => Promise<ShareView>) => {
    setBusy(true);
    try {
      onChange(await f());
    } catch {
      /* the next read shows the state */
    } finally {
      setBusy(false);
    }
  };
  const link = share.url;
  const stateText =
    share.state === "expired"
      ? tr("share_state_expired", undefined, "This link has expired.")
      : share.state === "cancelled"
        ? tr("share_state_cancelled", undefined, "You cancelled this share.")
        : share.state === "used"
          ? tr("share_state_used", undefined, "This link has been used: it takes no one else.")
          : null;

  return (
    <div className="space-y-4" data-testid="share-made">
      {link && live && (
        <div className="space-y-2">
          <div className="flex justify-center">
            <QrCode text={link} size={160} testId="share-qr" label={tr("share_qr_label", undefined, "QR code of the share link")} />
          </div>
          <p className="break-all font-mono text-xs" data-testid="share-url">
            {link}
          </p>
          <p className="text-xs text-[var(--color-muted)]">
            {share.works === "anywhere"
              ? tr("share_works_anywhere", undefined, "Works anywhere: on your Wi-Fi and over the internet.")
              : tr("share_works_wifi", undefined, "Works on your Wi-Fi only.")}{" "}
            {tr(
              "share_link_hint",
              { hours: Math.max(1, Math.round(share.expires_in_s / 3600)) },
              "Your friend opens it in PS5Upload (Play › Receive). It stops working in about {hours} h.",
            )}
          </p>
          <Button
            size="sm"
            variant="primary"
            leftIcon={copied ? <Check size={14} /> : <Copy size={14} />}
            onClick={() => void writeClipboard(link).then((ok) => setCopied(ok))}
            data-testid="share-copy"
          >
            {copied ? tr("share_copied", undefined, "Copied") : tr("share_copy", undefined, "Copy link")}
          </Button>
        </div>
      )}
      {stateText && (
        <p className="text-sm text-[var(--color-muted)]" data-testid="share-state">
          {stateText}
        </p>
      )}

      <div className="space-y-2" data-testid="share-requests">
        {share.requests.length === 0 && live && (
          <p className="text-sm text-[var(--color-muted)]">
            {tr("share_waiting", undefined, "Nobody has opened the link yet.")}
          </p>
        )}
        {share.requests.map((r) => (
          <div
            key={r.request}
            className="space-y-2 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-3"
            data-testid="share-request"
          >
            <p className="text-sm [overflow-wrap:anywhere]">
              {tr("share_wants", { name: r.name, items: r.wants_names.join(", ") }, "{name} wants: {items}")}
            </p>
            {r.state === "asking" && (
              <div className="flex flex-wrap gap-2">
                <Button
                  size="sm"
                  variant="primary"
                  disabled={busy}
                  onClick={() => void act(() => shareDecide(id, r.request, true))}
                  data-testid="share-allow"
                >
                  {tr("share_allow", undefined, "Allow")}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={busy}
                  onClick={() => void act(() => shareDecide(id, r.request, false))}
                  data-testid="share-decline"
                >
                  {tr("share_decline", undefined, "Decline")}
                </Button>
              </div>
            )}
            {(r.state === "sending" || r.state === "allowed") && (
              <ProgressBar
                value={r.bytes_total ? percent(r.bytes_done, r.bytes_total) / 100 : null}
                label={tr("share_sending", { name: r.name }, "Sending to {name}")}
              />
            )}
            {r.state !== "asking" && (
              <p className="text-xs text-[var(--color-muted)]" data-testid="share-request-state">
                {requestStateText(tr, r.state, r.bytes_done, r.bytes_total)}
              </p>
            )}
          </div>
        ))}
      </div>

      {live && (
        <div className="flex justify-end">
          <Button
            size="sm"
            variant="ghost"
            leftIcon={<X size={14} />}
            disabled={busy}
            onClick={() => void act(() => shareCancel(id))}
            data-testid="share-cancel"
          >
            {tr("share_cancel", undefined, "Cancel share")}
          </Button>
        </div>
      )}
    </div>
  );
}

type Tr = ReturnType<typeof useTr>;

function requestStateText(tr: Tr, state: string, done: number, total: number): string {
  switch (state) {
    case "allowed":
      return tr("share_req_allowed", undefined, "Allowed. Starting…");
    case "sending":
      return tr(
        "share_req_sending",
        { done: formatBytes(done), total: formatBytes(total) },
        "Sending: {done} of {total}",
      );
    case "done":
      return tr("share_req_done", undefined, "Sent.");
    case "declined":
      return tr("share_req_declined", undefined, "Declined.");
    case "cancelled":
      return tr("share_req_cancelled", undefined, "Cancelled.");
    case "left":
      return tr("share_req_left", undefined, "They left before you answered.");
    default:
      return tr("share_req_failed", undefined, "It didn't finish.");
  }
}
