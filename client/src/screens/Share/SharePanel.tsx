// Games › an app › Share (and Collection › an item › Share): pick what to send a friend, get a
// link and QR code, then allow or decline each friend who asks. The engine (share/mod.rs) checks
// the app again and refuses a retail game whatever this page sends.
//
// One panel, two faces: the choices, then (after Share) the link, the requests and the
// transfers. Each face puts its buttons in the panel's footer, which stays in view.

import { useEffect, useMemo, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Check, Clock, Copy, Share2, Users, X } from "lucide-react";

import type { CollectionLocation } from "../../api/collection";
import {
  cheatsGet,
  savesList,
  screenshotsList,
  videosList,
  type SaveEntry,
  type ScreenshotEntry,
} from "../../api/ps5";
import {
  shareCancel,
  shareCheck,
  shareCreate,
  shareDecide,
  shareGet,
  type ShareRequest,
  type ShareView,
} from "../../api/share";
import { Button, Callout, Modal, ProgressBar, SegmentedControl, Select } from "../../components";
import { GameIcon } from "../../components/GameIcon";
import { consoleAddr } from "../../lib/addr";
import { writeClipboard } from "../../lib/clipboard";
import { formatBytes } from "../../lib/format";
import { loadProfiles } from "../../lib/padProfiles";
import { useGameControlsStore } from "../../state/gameControls";
import { useKbmStore } from "../../state/kbm";
import { useTr } from "../../state/lang";
import { useRemotePlayStore } from "../../state/remotePlay";
import { QrCode } from "../Play/PhonePads";
import { GrowingNote, ItemRow, NameField, ShareLabel } from "./ShareParts";
import { ShareHistory } from "./ShareHistory";
import {
  DEFAULT_EXPIRY,
  EXPIRY_CHOICES,
  NOTE_MAX,
  appItem,
  appSources,
  createBody,
  mediaCounts,
  mediaOf,
  percent,
  saveShareName,
  savedShareName,
  timeLeft,
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

type Tr = ReturnType<typeof useTr>;

/** Where a face puts its buttons: the panel's footer, or (alone, in tests) under its body. */
function Footer({ slot, children }: { slot: HTMLElement | null; children: ReactNode }) {
  const row = <div className="flex w-full flex-wrap items-center justify-end gap-2">{children}</div>;
  return slot ? createPortal(row, slot) : <div className="pt-2">{row}</div>;
}

export function SharePanel({ target, onClose }: { target: ShareTarget; onClose: () => void }) {
  const tr = useTr();
  const [made, setMade] = useState<ShareView | null>(null);
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  return (
    <Modal
      open
      onClose={onClose}
      title={
        <span className="flex min-w-0 items-center gap-3">
          <GameIcon
            host={target.host ?? ""}
            titleId={target.titleId}
            gamePath={target.consolePath}
            size={40}
            rounded="rounded-[10px]"
            label={target.title}
          />
          <span className="flex min-w-0 flex-col">
            <span className="truncate" data-testid="share-title">
              {target.title}
            </span>
            <span className="truncate text-xs font-normal text-[var(--color-muted)]">
              {tr("share_header_sub", { id: target.titleId }, "Share · {id}")}
            </span>
          </span>
        </span>
      }
      size="lg"
      variant="sheet"
      bodyClassName="px-4 py-4 sm:px-6 sm:py-5"
      footer={<div ref={setSlot} className="w-full pb-[env(safe-area-inset-bottom)]" />}
    >
      <div data-testid="share-panel">
        {made ? (
          <ShareMade share={made} onChange={setMade} onDone={onClose} footerSlot={slot} />
        ) : (
          <SharePick target={target} onMade={setMade} onCancel={onClose} footerSlot={slot} />
        )}
      </div>
    </Modal>
  );
}

/** The app's line under its name: where it is shared from. */
function sourceLine(tr: Tr, app: AppSource): string {
  return app.kind === "console" ? tr("share_app_console", undefined, "The PS5's copy") : app.label;
}

/** The engine's refusal in one short line. */
function refusalLine(tr: Tr, why: string): string {
  if (/retail game/i.test(why)) return tr("share_app_retail_short", undefined, "Retail games can't be shared");
  if (/can't confirm|can.t tell/i.test(why)) return tr("share_app_unknown_short", undefined, "Can't confirm it's homebrew");
  return why;
}

export function SharePick({
  target,
  onMade,
  onCancel,
  verdict = null,
  footerSlot = null,
}: {
  target: ShareTarget;
  onMade: (s: ShareView) => void;
  onCancel?: () => void;
  /** The engine's verdict on the app to start from (tests; the panel asks it). */
  verdict?: string | null;
  footerSlot?: HTMLElement | null;
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
  const [media, setMedia] = useState<ScreenshotEntry[]>([]);
  const [withMedia, setWithMedia] = useState(false);
  const [cheats, setCheats] = useState<{ name: string; on: boolean }[]>([]);
  const [withCheats, setWithCheats] = useState(false);
  const gameControls = useGameControlsStore((s) => s.byTitle[target.titleId.toUpperCase()] ?? null);
  const kbmMode = useKbmStore((s) => s.modes[target.titleId.toUpperCase()] ?? null);
  const [padProfiles] = useState(() => loadProfiles());
  const hasControls = !!gameControls || !!kbmMode || Object.keys(padProfiles).length > 0;
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
      .then(([s, v]) => {
        if (!live) return;
        const all = [...s.items, ...v.items];
        const keep = new Set(mediaOf(all, target.titleId, target.title));
        setMedia(all.filter((e) => keep.has(e.path)));
      })
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
        pad_profiles: padProfiles,
      }
    : null;
  const mediaPaths = media.map((m) => m.path);
  const body = createBody({
    from,
    title: target.title,
    titleId: target.titleId,
    console: addr,
    app: appOk ? app : null,
    saves: pickedSaves,
    media: withMedia ? mediaPaths : [],
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
  const { shots, clips } = mediaCounts(mediaPaths);
  const savesSize = saves.reduce((n, s) => n + s.size, 0);
  const cheatsOn = cheats.filter((c) => c.on).length;

  return (
    <div className="space-y-5" data-testid="share-pick">
      <p className="text-sm text-[var(--color-muted)]">
        {tr(
          "share_intro_v2",
          undefined,
          "Pick what to send. Your friend opens the link and asks; nothing goes until you allow it.",
        )}
      </p>

      <section className="space-y-2" aria-labelledby="share-what">
        <ShareLabel id="share-what">{tr("share_what", undefined, "What to share")}</ShareLabel>
        {app && (
          <ItemRow
            testId="share-app"
            kind="app"
            name={tr("share_app_v2", undefined, "The app")}
            line={sourceLine(tr, app)}
            size={app.size ? formatBytes(app.size) : null}
            checked={withApp}
            onChange={setWithApp}
            busy={refusal === null}
            unavailable={refusal ? refusalLine(tr, refusal) : null}
            reason={refusal || null}
          >
            {sources.length > 1 && (
              <Select
                aria-label={tr("share_app_from", undefined, "Share the app from")}
                value={String(source)}
                onChange={(e) => setSource(Number(e.target.value))}
                options={sources.map((s, i) => ({ value: String(i), label: sourceLine(tr, s) }))}
              />
            )}
          </ItemRow>
        )}
        {refusal === null && app && (
          <p className="sr-only" role="status">
            {tr("share_app_checking", undefined, "Checking the app…")}
          </p>
        )}
        {saves.length > 0 && (
          <ItemRow
            testId="share-save"
            kind="save"
            name={tr("share_save_v2", undefined, "Save")}
            line={tr("share_save_line", undefined, "Your progress. Trophies and your account stay yours.")}
            size={formatBytes(saves.length > 1 && pickedSaves.length ? sumOf(saves, pickedSaves) : savesSize)}
            checked={pickedSaves.length > 0}
            onChange={(on) => setPickedSaves(on ? saves.map((s) => s.path) : [])}
          >
            {saves.length > 1 && (
              <div className="flex flex-wrap gap-1.5" role="group" aria-label={tr("share_save_slots", undefined, "Which saves")}>
                {saves.map((s) => {
                  const on = pickedSaves.includes(s.path);
                  return (
                    <button
                      key={s.path}
                      type="button"
                      aria-pressed={on}
                      onClick={() =>
                        setPickedSaves((p) => (on ? p.filter((x) => x !== s.path) : [...p, s.path]))
                      }
                      className="chip min-h-8 gap-1.5 px-3 text-xs max-md:min-h-11"
                      data-testid="share-save-slot"
                    >
                      {on && <Check size={12} aria-hidden />}
                      {tr("share_save_slot", { user: s.user_id, size: formatBytes(s.size) }, "User {user} · {size}")}
                    </button>
                  );
                })}
              </div>
            )}
          </ItemRow>
        )}
        {media.length > 0 && (
          <ItemRow
            testId="share-media"
            kind="media"
            name={tr("share_media_v2", undefined, "Screenshots and clips")}
            line={mediaLine(tr, shots, clips)}
            size={formatBytes(media.reduce((n, m) => n + m.size, 0))}
            checked={withMedia}
            onChange={setWithMedia}
          />
        )}
        {cheats.length > 0 && (
          <ItemRow
            testId="share-cheats"
            kind="cheats"
            name={tr("share_cheats_v2", undefined, "Cheats")}
            line={
              cheats.length === 1
                ? tr("share_cheats_line_one", { on: cheatsOn }, "1 cheat · {on} on now")
                : tr("share_cheats_line", { n: cheats.length, on: cheatsOn }, "{n} cheats · {on} on now")
            }
            checked={withCheats}
            onChange={setWithCheats}
          />
        )}
        {hasControls && (
          <ItemRow
            testId="share-controls"
            kind="controls"
            name={tr("share_controls_v2", undefined, "Controls")}
            line={tr("share_controls_line", undefined, "Button remaps, keyboard and mouse, macros")}
            checked={withControls}
            onChange={setWithControls}
          />
        )}
        <GrowingNote
          value={note}
          onChange={setNote}
          max={NOTE_MAX}
          placeholder={tr("share_note_v2", undefined, "A note for your friend (optional)")}
          testId="share-note"
        />
      </section>

      <section
        className="space-y-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
        aria-label={tr("share_link_settings", undefined, "Link settings")}
      >
        <NameField value={from} onChange={setFrom} testId="share-name" />
        <div className="space-y-2 border-t border-[var(--color-border)] pt-4">
          <ShareLabel>{tr("share_lifetime", undefined, "Link lifetime")}</ShareLabel>
          <SegmentedControl
            ariaLabel={tr("share_lifetime", undefined, "Link lifetime")}
            value={String(expires)}
            onChange={(v) => setExpires(Number(v))}
            segments={EXPIRY_CHOICES.map((s) => ({ value: String(s), label: expiryLabel(s) }))}
          />
        </div>
        <div className="space-y-2 border-t border-[var(--color-border)] pt-4" data-testid="share-who">
          <ShareLabel>{tr("share_who", undefined, "Who can use it")}</ShareLabel>
          <SegmentedControl
            ariaLabel={tr("share_who", undefined, "Who can use it")}
            value={oneUse ? "one" : "anyone"}
            onChange={(v) => setOneUse(v === "one")}
            segments={[
              { value: "one", label: tr("share_who_one", undefined, "One friend") },
              { value: "anyone", label: tr("share_who_anyone", undefined, "Anyone with the link") },
            ]}
          />
          <p className="text-xs text-[var(--color-muted)]" data-testid="share-who-hint">
            {oneUse
              ? tr("share_who_one_hint", undefined, "The link stops working after you allow the first person.")
              : tr("share_who_anyone_hint", undefined, "Anyone who opens it can ask. You allow each one, until it expires.")}
          </p>
        </div>
      </section>

      {error && (
        <Callout tone="error" title={tr("share_failed", undefined, "Couldn't make the share")}>
          <span data-testid="share-error">{error}</span>
        </Callout>
      )}

      <ShareHistory />

      <Footer slot={footerSlot}>
        {onCancel && (
          <Button variant="ghost" size="md" onClick={onCancel} data-testid="share-close">
            {tr("share_not_now", undefined, "Cancel")}
          </Button>
        )}
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
      </Footer>
    </div>
  );
}

function sumOf(saves: SaveEntry[], picked: string[]): number {
  return saves.filter((s) => picked.includes(s.path)).reduce((n, s) => n + s.size, 0);
}

function mediaLine(tr: Tr, shots: number, clips: number): string {
  const s =
    shots === 1
      ? tr("share_media_shot_one", undefined, "1 screenshot")
      : tr("share_media_shots", { n: shots }, "{n} screenshots");
  const c =
    clips === 1 ? tr("share_media_clip_one", undefined, "1 clip") : tr("share_media_clips", { n: clips }, "{n} clips");
  return shots && clips ? `${s}, ${c}` : clips ? c : s;
}

/** "Expires in 23 h 59 min", kept current while shown. `seconds` is the time left when it was
 *  read (each poll reads it again): the caller keys this on it, so the count runs from when the
 *  latest value came. Counting from when it first showed took the elapsed time off twice. */
function Countdown({ seconds }: { seconds: number }) {
  const tr = useTr();
  const [at] = useState(() => Date.now());
  const [now, setNow] = useState(at);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);
  const { h, m } = timeLeft(seconds - (now - at) / 1000);
  const text =
    h > 0
      ? tr("share_expires_hm", { h, m }, "Expires in {h} h {m} min")
      : m > 0
        ? tr("share_expires_m", { m }, "Expires in {m} min")
        : tr("share_expires_soon", undefined, "Expires in under a minute");
  return (
    <span className="inline-flex items-center gap-1.5" data-testid="share-expires">
      <Clock size={12} aria-hidden />
      {text}
    </span>
  );
}

export function ShareMade({
  share,
  onChange,
  onDone,
  footerSlot = null,
}: {
  share: ShareView;
  onChange: (s: ShareView) => void;
  onDone?: () => void;
  footerSlot?: HTMLElement | null;
}) {
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
  const canShare = typeof navigator !== "undefined" && typeof navigator.share === "function";
  const nativeShare = async () => {
    if (!link) return;
    try {
      await navigator.share({ title: share.title || "PS5Upload", text: tr("share_native_text", { title: share.title }, "{title}, shared with PS5Upload"), url: link });
    } catch {
      // Closed without sharing.
    }
  };
  const copy = async () => {
    if (!link) return;
    if (await writeClipboard(link)) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    }
  };
  const stateText =
    share.state === "expired"
      ? tr("share_state_expired", undefined, "This link has expired.")
      : share.state === "cancelled"
        ? tr("share_state_cancelled", undefined, "You cancelled this share.")
        : share.state === "used"
          ? tr("share_state_used", undefined, "This link has been used: it takes no one else.")
          : null;
  const showLink = !!link && share.state === "open";

  return (
    <div className="space-y-5" data-testid="share-made">
      {showLink && (
        <section className="flex flex-col items-center gap-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4 sm:flex-row sm:items-start">
          <div className="shrink-0 rounded-[12px] bg-white p-2">
            <QrCode text={link} size={148} testId="share-qr" label={tr("share_qr_label", undefined, "QR code of the share link")} />
          </div>
          <div className="w-full min-w-0 space-y-3">
            <p className="text-sm font-medium">{tr("share_link_ready", undefined, "Your link is ready")}</p>
            <p
              className="truncate rounded-[10px] border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2 font-mono text-xs"
              title={link}
              data-testid="share-url"
            >
              {link}
            </p>
            <div className="flex flex-wrap gap-2">
              <Button
                size="sm"
                variant="primary"
                leftIcon={copied ? <Check size={14} /> : <Copy size={14} />}
                onClick={() => void copy()}
                data-testid="share-copy"
              >
                {copied ? tr("share_copied", undefined, "Copied") : tr("share_copy", undefined, "Copy link")}
              </Button>
              {canShare && (
                <Button size="sm" variant="secondary" leftIcon={<Share2 size={14} />} onClick={() => void nativeShare()} data-testid="share-native">
                  {tr("share_native", undefined, "Share…")}
                </Button>
              )}
            </div>
            <p className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-[var(--color-muted)]">
              <Countdown key={share.expires_in_s} seconds={share.expires_in_s} />
              <span>
                {share.works === "anywhere"
                  ? tr("share_works_anywhere_v2", undefined, "Works on your Wi-Fi and over the internet")
                  : tr("share_works_wifi_v2", undefined, "Works on your Wi-Fi only")}
              </span>
            </p>
            <p className="text-xs text-[var(--color-muted)]">
              {tr("share_open_where_v2", undefined, "Your friend opens the link, or pastes it in PS5Upload › Share.")}
            </p>
          </div>
        </section>
      )}
      {stateText && (
        <p className="text-sm text-[var(--color-muted)]" data-testid="share-state">
          {stateText}
        </p>
      )}

      <section className="space-y-2" data-testid="share-requests" aria-labelledby="share-requests-h">
        <ShareLabel id="share-requests-h">{tr("share_requests", undefined, "Requests")}</ShareLabel>
        {share.requests.length === 0 && live && (
          <div className="flex items-center gap-3 rounded-[var(--radius-card)] border border-dashed border-[var(--color-border-strong)] px-4 py-4 text-sm text-[var(--color-muted)]">
            <Users size={16} aria-hidden />
            {tr("share_waiting_v2", undefined, "Waiting for your friend to open the link…")}
          </div>
        )}
        {share.requests.map((r) => (
          <RequestCard
            key={r.request}
            r={r}
            busy={busy}
            onAllow={() => void act(() => shareDecide(id, r.request, true))}
            onDecline={() => void act(() => shareDecide(id, r.request, false))}
          />
        ))}
      </section>

      <Footer slot={footerSlot}>
        {live && (
          <Button
            size="md"
            variant="ghost"
            leftIcon={<X size={14} />}
            disabled={busy}
            onClick={() => void act(() => shareCancel(id))}
            data-testid="share-cancel"
          >
            {tr("share_cancel", undefined, "Cancel share")}
          </Button>
        )}
        {onDone && (
          <Button size="md" variant="primary" onClick={onDone} data-testid="share-done">
            {tr("share_done", undefined, "Done")}
          </Button>
        )}
      </Footer>
    </div>
  );
}

function RequestCard({
  r,
  busy,
  onAllow,
  onDecline,
}: {
  r: ShareRequest;
  busy: boolean;
  onAllow: () => void;
  onDecline: () => void;
}) {
  const tr = useTr();
  const asking = r.state === "asking";
  const moving = r.state === "sending" || r.state === "allowed";
  return (
    <div
      className={[
        "space-y-3 rounded-[var(--radius-card)] border p-3",
        asking
          ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
          : "border-[var(--color-border)] bg-[var(--color-surface-raised)]",
      ].join(" ")}
      data-testid="share-request"
      data-state={r.state}
    >
      <div className="flex items-start gap-3">
        <span
          aria-hidden
          className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-[var(--color-surface-3)] text-sm font-semibold"
        >
          {(r.name.trim()[0] ?? "?").toUpperCase()}
        </span>
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium [overflow-wrap:anywhere]">
            {asking ? tr("share_asks", { name: r.name }, "{name} asks for") : r.name}
          </p>
          <p
            className="text-xs text-[var(--color-muted)] [overflow-wrap:anywhere]"
            data-testid={asking ? "share-request-wants" : "share-request-state"}
          >
            {asking ? r.wants_names.join(", ") : requestStateText(tr, r.state, r.bytes_done, r.bytes_total)}
          </p>
        </div>
      </div>
      {asking && (
        <div className="flex flex-wrap justify-end gap-2">
          <Button size="sm" variant="ghost" disabled={busy} onClick={onDecline} data-testid="share-decline">
            {tr("share_decline", undefined, "Decline")}
          </Button>
          <Button size="sm" variant="primary" disabled={busy} onClick={onAllow} data-testid="share-allow">
            {tr("share_allow", undefined, "Allow")}
          </Button>
        </div>
      )}
      {moving && (
        <ProgressBar
          value={r.bytes_total ? percent(r.bytes_done, r.bytes_total) / 100 : null}
          label={tr("share_sending", { name: r.name }, "Sending to {name}")}
        />
      )}
    </div>
  );
}

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
