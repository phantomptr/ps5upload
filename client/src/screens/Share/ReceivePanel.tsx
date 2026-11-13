// Share › Receive: open a friend's Share link. Their name and what they offer show first; nothing
// arrives until Accept, and then only once they allow it. When it is here: install the app,
// use the save, use the controls, read the note.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { CheckCircle2, Download, Inbox, X } from "lucide-react";

import type { CollectionLocation } from "../../api/collection";
import { localFs } from "../../api/localFs";
import { savesList, type SaveEntry } from "../../api/ps5";
import {
  receiveAccept,
  receiveCancel,
  receiveData,
  receiveGet,
  receiveOpen,
  receiveUseSave,
  type OfferedItem,
  type ReceiveView,
} from "../../api/share";
import { Button, Callout, Input, ProgressBar, SegmentedControl, Select, Spinner } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { consoleAddr } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { readControls } from "../../lib/gameControls";
import { useConnectionStore } from "../../state/connection";
import { enqueueInstall } from "../../state/consoleQueueBridge";
import { useGameControlsStore } from "../../state/gameControls";
import { useKbmStore } from "../../state/kbm";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { useRemotePlayStore } from "../../state/remotePlay";
import { SendToPs5 } from "../GameHub/SendToPs5";
import { ItemRow, KIND_ICON, NameField } from "./ShareParts";
import { percent, saveShareName, savedShareName } from "./shareModel";

const POLL_MS = 1000;
const ENDED = ["done", "declined", "cancelled", "failed", "refused"];

type Tr = ReturnType<typeof useTr>;

export function ReceivePanel({ pending, onEnded }: { pending?: string | null; onEnded?: () => void }) {
  const tr = useTr();
  const host = useConnectionStore((s) => (s.payloadStatus === "up" ? (s.host?.trim() ?? "") : ""));
  const [link, setLink] = useState(pending ?? "");
  const [name, setName] = useState(() => savedShareName());
  const [id, setId] = useState<string | null>(null);
  const [view, setView] = useState<ReceiveView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // A new link handed over while this panel shows goes into the form (not over a running receive).
  const [shown, setShown] = useState(pending);
  if (pending !== shown) {
    setShown(pending);
    if (pending && !id) setLink(pending);
  }

  // While a receive runs, its state (read on demand; stops once it ended).
  const ended = !!view && ENDED.includes(view.state);
  // `onEnded` must be stable (the screen passes a useCallback), or this runs every render.
  useEffect(() => {
    if (ended) onEnded?.();
  }, [ended, onEnded]);
  useEffect(() => {
    if (!id || ended) return;
    let on = true;
    const read = () =>
      receiveGet(id)
        .then((v) => on && setView(v))
        .catch(() => {});
    void read();
    const t = setInterval(read, POLL_MS);
    return () => {
      on = false;
      clearInterval(t);
    };
  }, [id, ended]);

  const open = async () => {
    setBusy(true);
    setError(null);
    saveShareName(name);
    try {
      setView(null);
      setId(await receiveOpen(link.trim(), name.trim()));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const reset = () => {
    setId(null);
    setView(null);
    setLink("");
  };

  return (
    <div className="min-w-0 space-y-4" data-testid="receive-panel">
      {!id && (
        <form
          className={CARD}
          onSubmit={(e) => {
            e.preventDefault();
            void open();
          }}
        >
          {/* Titled like a Card: the page header already carries the big icon. */}
          <div>
            <h2 className="flex items-start gap-2 text-[0.9375rem] font-semibold tracking-[-0.01em]">
              <Inbox size={16} aria-hidden className="mt-[3px] shrink-0 text-[var(--color-accent-bright)]" />
              {tr("receive_title", undefined, "Receive what a friend shared")}
            </h2>
            <p className="mt-1 text-sm text-[var(--color-muted)]">
              {tr("receive_intro_v2", undefined, "Paste their link. You see what they offer before anything arrives.")}
            </p>
          </div>
          <Input
            label={tr("receive_link", undefined, "Share link")}
            value={link}
            placeholder="https://…/share/#room=…"
            onChange={(e) => setLink(e.target.value)}
            data-testid="receive-link"
          />
          <NameField value={name} onChange={setName} testId="receive-name" />
          {error && (
            <p className="text-sm text-[var(--color-bad)]" data-testid="receive-error">
              {error}
            </p>
          )}
          <div className="flex justify-end border-t border-[var(--color-border)] pt-4">
            <Button type="submit" variant="primary" size="md" disabled={!link.trim() || busy} loading={busy} data-testid="receive-open">
              {tr("receive_open", undefined, "Open")}
            </Button>
          </div>
        </form>
      )}
      {id && !view && <Connecting />}
      {id && view && <ReceiveFlow view={view} host={host} onDone={reset} onView={setView} />}
    </div>
  );
}

// The shared Card's look (components/Card.tsx): glass edge, raised fill, elevation.
const CARD = "elev-1 space-y-4 rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-4 sm:p-5";

function Connecting() {
  const tr = useTr();
  return (
    <div className={`${CARD} flex items-center gap-3`} data-testid="receive-connecting">
      <Spinner size={16} />
      <p className="text-sm text-[var(--color-muted)]">{tr("receive_connecting", undefined, "Connecting to your friend…")}</p>
    </div>
  );
}

/** Who shared, as the card's header: their initial, name and what. */
function FromHeader({ from, title, sub }: { from: string; title: string; sub?: ReactNode }) {
  return (
    <div className="flex items-center gap-3">
      <span aria-hidden className="grid h-10 w-10 shrink-0 place-items-center rounded-full bg-[var(--color-accent)] text-base font-semibold text-[var(--color-accent-contrast)]">
        {(from.trim()[0] ?? "?").toUpperCase()}
      </span>
      <div className="min-w-0">
        <h3 className="text-base font-semibold [overflow-wrap:anywhere]">{title}</h3>
        {sub && <p className="text-xs text-[var(--color-muted)]">{sub}</p>}
      </div>
    </div>
  );
}

/** The refused item's short line. */
function refusedLine(tr: Tr, why: string): string {
  if (/retail game/i.test(why)) return tr("share_app_retail_short", undefined, "Retail games can't be shared");
  if (/can't confirm|can.t tell/i.test(why)) return tr("share_app_unknown_short", undefined, "Can't confirm it's homebrew");
  return why;
}

interface ReceiveFlowProps {
  view: ReceiveView;
  host: string;
  onDone: () => void;
  onView: (v: ReceiveView) => void;
}

/** While connecting there are no items yet. The flow below starts its selection from the
 *  items it first sees, so it only mounts once they are here: mounted earlier, nothing was
 *  ever pre-selected. */
export function ReceiveFlow(props: ReceiveFlowProps) {
  if (props.view.state === "connecting") return <Connecting />;
  return <ReceiveFlowOpen {...props} />;
}

function ReceiveFlowOpen({ view, host, onDone, onView }: ReceiveFlowProps) {
  const tr = useTr();
  const [picked, setPicked] = useState<string[]>(() => view.items.filter((i) => !i.refused).map((i) => i.id));
  const [then, setThen] = useState<"keep" | "install">("keep");
  const [busy, setBusy] = useState(false);
  const from = view.from || tr("receive_a_friend", undefined, "Your friend");

  const accept = async () => {
    setBusy(true);
    try {
      await receiveAccept(view.id, picked);
      onView({ ...view, state: "asking", accepted: picked });
    } catch (e) {
      pushNotification("error", e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const cancel = async () => {
    setBusy(true);
    try {
      await receiveCancel(view.id);
    } catch {
      /* already over */
    } finally {
      setBusy(false);
    }
  };

  if (view.state === "offered") {
    const size = view.items.filter((i) => picked.includes(i.id)).reduce((n, i) => n + i.size, 0);
    return (
      <div className={CARD} data-testid="receive-offer">
        <FromHeader
          from={from}
          title={
            view.title
              ? tr("receive_offer_title", { name: from, title: view.title }, "{name} shared {title}")
              : tr("receive_offer_title_plain", { name: from }, "{name} shared something with you")
          }
          sub={view.title_id ?? undefined}
        />
        <section className="space-y-2" aria-label={tr("receive_pick", undefined, "What to take")}>
          {view.items.map((i) => (
            <ItemRow
              key={i.id}
              testId="receive-item"
              kind={i.kind}
              name={i.name}
              size={formatBytes(i.size)}
              line={offeredLine(tr, i)}
              checked={picked.includes(i.id)}
              onChange={(on) => setPicked((p) => (on ? [...p, i.id] : p.filter((x) => x !== i.id)))}
              unavailable={i.refused ? refusedLine(tr, i.refused) : null}
              reason={i.refused}
            />
          ))}
        </section>
        {view.items.some((i) => i.kind === "app" && !i.refused && picked.includes(i.id)) && host && (
          <div className="space-y-2">
            <p className="text-xs font-semibold uppercase tracking-[0.06em] text-[var(--color-muted)]">
              {tr("receive_then", undefined, "When the app arrives")}
            </p>
            <SegmentedControl
              ariaLabel={tr("receive_then", undefined, "When the app arrives")}
              value={then}
              onChange={(v) => setThen(v as "keep" | "install")}
              segments={[
                { value: "keep", label: tr("receive_keep_v2", undefined, "Keep in Collection") },
                { value: "install", label: tr("receive_install_v2", undefined, "Install to my PS5") },
              ]}
            />
          </div>
        )}
        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-[var(--color-border)] pt-4">
          {size > 0 && (
            <span className="me-auto text-xs tabular-nums text-[var(--color-muted)]" data-testid="receive-total">
              {tr("receive_total", { size: formatBytes(size) }, "{size} in all")}
            </span>
          )}
          <Button variant="ghost" size="md" disabled={busy} onClick={() => void cancel()} data-testid="receive-decline">
            {tr("receive_no_thanks", undefined, "No thanks")}
          </Button>
          <Button variant="primary" size="md" disabled={picked.length === 0 || busy} onClick={() => void accept()} data-testid="receive-accept">
            {tr("receive_accept", undefined, "Accept")}
          </Button>
        </div>
      </div>
    );
  }
  if (view.state === "asking" || view.state === "receiving" || view.state === "reconnecting") {
    const pct = percent(view.bytes_done, view.bytes_total);
    return (
      <div className={CARD} data-testid="receive-progress">
        <FromHeader
          from={from}
          title={view.title || from}
          sub={view.state === "receiving" && view.path ? pathText(tr, view.path) : undefined}
        />
        <p className="text-sm" data-testid="receive-progress-text">
          {view.state === "asking"
            ? tr("receive_asking", { name: from }, "Waiting for {name} to allow it…")
            : view.state === "reconnecting"
              ? tr("receive_reconnecting", undefined, "The connection dropped. Connecting again…")
              : tr(
                  "receive_receiving",
                  { done: formatBytes(view.bytes_done), total: formatBytes(view.bytes_total) },
                  "Receiving: {done} of {total}",
                )}
        </p>
        <div className="flex items-center gap-3">
          <ProgressBar
            className="flex-1"
            value={view.state === "asking" ? null : pct / 100}
            label={tr("receive_progress", undefined, "Receiving")}
          />
          {view.state !== "asking" && <span className="w-10 text-end text-xs tabular-nums text-[var(--color-muted)]">{pct}%</span>}
        </div>
        <div className="flex justify-end border-t border-[var(--color-border)] pt-4">
          <Button size="md" variant="ghost" leftIcon={<X size={14} />} disabled={busy} onClick={() => void cancel()} data-testid="receive-cancel">
            {tr("receive_cancel", undefined, "Cancel")}
          </Button>
        </div>
      </div>
    );
  }
  if (view.state === "done") {
    return (
      <div className={CARD} data-testid="receive-done">
        <div className="flex items-center gap-3">
          <span aria-hidden className="grid h-10 w-10 shrink-0 place-items-center rounded-full bg-[var(--color-good-soft)] text-[var(--color-good)]">
            <CheckCircle2 size={20} />
          </span>
          <div className="min-w-0">
            <h3 className="text-base font-semibold">{tr("receive_done", { name: from }, "Received from {name}")}</h3>
            {view.dest && (
              <p className="truncate font-mono text-xs text-[var(--color-muted)]" title={view.dest} data-testid="receive-dest">
                {tr("receive_where", { path: view.dest }, "Saved in {path}")}
              </p>
            )}
          </div>
        </div>
        <div className="space-y-2">
          {view.received.map((r) => {
            const item = view.items.find((i) => i.id === r.item);
            return item ? (
              <ReceivedItem key={r.item} rx={view} item={item} dir={r.file || r.path} host={host} autoInstall={then === "install"} />
            ) : null;
          })}
        </div>
        <div className="flex justify-end border-t border-[var(--color-border)] pt-4">
          <Button size="md" variant="primary" onClick={onDone} data-testid="receive-another">
            {tr("receive_done_button", undefined, "Done")}
          </Button>
        </div>
      </div>
    );
  }
  return (
    <div className={CARD} data-testid="receive-ended">
      <p className="text-sm" data-testid="receive-ended-text">
        {endedText(tr, view, from)}
      </p>
      <div className="flex justify-end">
        <Button size="md" variant="secondary" onClick={onDone} data-testid="receive-another">
          {tr("receive_another", undefined, "Open another link")}
        </Button>
      </div>
    </div>
  );
}

/** An offered item's line: what it is. */
function offeredLine(tr: Tr, i: OfferedItem): string {
  switch (i.kind) {
    case "app":
      return i.title_id ?? tr("receive_kind_app", undefined, "The app");
    case "save":
      return tr("receive_kind_save", undefined, "Goes into your own save of the game");
    case "media":
      return tr("receive_kind_media", { n: i.files }, "{n} files");
    case "cheats":
      return tr("receive_kind_cheats", undefined, "Their cheat file and which are on");
    case "controls":
      return tr("receive_kind_controls", undefined, "Button remaps, keyboard and mouse, macros");
    case "note":
      return tr("receive_kind_note", undefined, "A note from them");
    default:
      return "";
  }
}

function pathText(tr: Tr, path: string): string {
  if (path === "lan") return tr("receive_path_lan", undefined, "Over your home network.");
  if (path === "tailscale") return tr("receive_path_tailscale", undefined, "Over Tailscale.");
  return tr("receive_path_internet", undefined, "Straight over the internet.");
}

function endedText(tr: Tr, v: ReceiveView, from: string): string {
  switch (v.state) {
    case "declined":
      return tr("receive_declined", { name: from }, "{name} declined.");
    case "cancelled":
      return v.error ?? tr("receive_cancelled", undefined, "Cancelled.");
    case "refused":
      return refusedText(tr, v.error);
    default:
      return tr("receive_failed", { why: v.error ?? "" }, "It didn't finish. {why}").trim();
  }
}

function refusedText(tr: Tr, why: string | null): string {
  switch (why) {
    case "expired":
      return tr("receive_refused_expired", undefined, "This link has expired. Ask for a new one.");
    case "used":
      return tr("receive_refused_used", undefined, "This link was already used. Ask for a new one.");
    case "cancelled":
      return tr("receive_refused_cancelled", undefined, "Your friend cancelled this share.");
    case "busy":
      return tr("receive_refused_busy", undefined, "Your friend is busy. Try again in a minute.");
    default:
      return tr("receive_refused", undefined, "This share isn't open any more.");
  }
}

const SINGLE_FILE = /\.(pkg|exfat|ffpkg|ffpfs|ffpfsc)$/i;

/** A received app as a Collection copy, for Send to PS5. */
async function appLocation(item: OfferedItem, dir: string): Promise<CollectionLocation | null> {
  let path = dir;
  let size = item.size;
  let type = item.format ?? "folder";
  if (type !== "folder" && SINGLE_FILE.test(dir)) {
    // The engine named the file itself.
    path = dir;
  } else if (type !== "folder") {
    const entries = await localFs.listDir(dir).catch(() => []);
    const f = entries.find((e) => !e.is_dir && SINGLE_FILE.test(e.name));
    if (!f) return null;
    path = f.path;
    size = f.size;
  } else {
    type = "folder";
  }
  return {
    root: "",
    container: "",
    name: item.name,
    type,
    path: item.name,
    absolute_path: path,
    size_bytes: size,
    added_ts: 0,
  };
}

function ReceivedItem({
  rx,
  item,
  dir,
  host,
  autoInstall,
}: {
  rx: ReceiveView;
  item: OfferedItem;
  dir: string;
  host: string;
  autoInstall: boolean;
}) {
  const tr = useTr();
  const Icon = KIND_ICON[item.kind];
  return (
    <div
      className="space-y-2 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-3"
      data-testid={`received-${item.kind}`}
    >
      <p className="flex items-center gap-2 text-sm font-medium [overflow-wrap:anywhere]">
        <Icon size={15} aria-hidden className="shrink-0 text-[var(--color-muted)]" />
        {item.name}
      </p>
      {item.kind === "app" && <ReceivedApp item={item} dir={dir} host={host} autoInstall={autoInstall} />}
      {item.kind === "save" && <ReceivedSave rx={rx} item={item} host={host} />}
      {(item.kind === "controls" || item.kind === "note" || item.kind === "cheats" || item.kind === "stream") && (
        <ReceivedData rx={rx} item={item} />
      )}
      {item.kind === "media" && (
        <p className="break-all text-xs text-[var(--color-muted)]">{tr("receive_where", { path: dir }, "Saved in {path}")}</p>
      )}
    </div>
  );
}

function ReceivedApp({ item, dir, host, autoInstall }: { item: OfferedItem; dir: string; host: string; autoInstall: boolean }) {
  const tr = useTr();
  const [loc, setLoc] = useState<CollectionLocation | null>(null);
  const [show, setShow] = useState(false);
  const started = useRef(false);
  useEffect(() => {
    let live = true;
    void appLocation(item, dir).then((l) => live && setLoc(l));
    return () => {
      live = false;
    };
  }, [item, dir]);
  const install = () => {
    if (!loc || !host) return;
    if (loc.type === "pkg") {
      try {
        enqueueInstall({
          host,
          request: { via: "stream", source: loc.absolute_path },
          displayName: item.name,
          sizeBytes: loc.size_bytes,
        });
        pushNotification("info", tr("receive_install_queued", { name: item.name }, "{name} is queued to install"), { link: "/send" });
      } catch (e) {
        pushNotification("error", e instanceof Error ? e.message : String(e));
      }
      return;
    }
    setShow(true);
  };
  // "Install it to my PS5" chosen before: start once it is here.
  useEffect(() => {
    if (loc && host && autoInstall && !started.current) {
      started.current = true;
      install();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loc, host]);
  return (
    <div className="space-y-2">
      <p className="text-xs text-[var(--color-muted)]">
        {tr("receive_app_in_collection", undefined, "It's in your Collection now.")}
      </p>
      {host && loc && !show && (
        <Button size="sm" variant="primary" leftIcon={<Download size={14} />} onClick={install} data-testid="receive-install">
          {tr("receive_install_now", undefined, "Install to my PS5")}
        </Button>
      )}
      {show && loc && host && <SendToPs5 game={{ title: item.name }} host={host} copies={[loc]} initial={loc} />}
    </div>
  );
}

function ReceivedSave({ rx, item, host }: { rx: ReceiveView; item: OfferedItem; host: string }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [targets, setTargets] = useState<SaveEntry[]>([]);
  const [target, setTarget] = useState("");
  const [busy, setBusy] = useState(false);
  const titleId = item.title_id ?? rx.title_id ?? "";
  useEffect(() => {
    if (!host || !titleId) return;
    let live = true;
    savesList(consoleAddr(host), 0)
      .then((r) => {
        if (!live) return;
        const t = r.saves.filter((s) => s.title_id === titleId && s.kind === "ps5");
        setTargets(t);
        setTarget(t[0]?.path ?? "");
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [host, titleId]);
  if (!host) {
    return <p className="text-xs text-[var(--color-muted)]">{tr("receive_save_connect", undefined, "Connect your PS5 to use this save.")}</p>;
  }
  if (targets.length === 0) {
    return (
      <p className="text-xs text-[var(--color-muted)]" data-testid="receive-save-none">
        {tr(
          "receive_save_start_game",
          undefined,
          "Start the game once and save on your PS5, then come back: the save goes into your own save of the game.",
        )}
      </p>
    );
  }
  const t = targets.find((s) => s.path === target);
  const use = async () => {
    if (!t) return;
    const user = String(t.user_id);
    const ok = await confirm({
      title: tr("receive_save_confirm_title", { title: item.name }, "Use {title}?"),
      message: tr(
        "saves_use_shared_confirm_body_v2",
        { user },
        "{user}'s save is replaced with the one in the file, and the PS5 keeps it as {user}'s. {user}'s current save is backed up first and put back if anything fails. Close the game first.",
      ),
      destructive: true,
      confirmLabel: tr("saves_use_shared_confirm_label", undefined, "Use it"),
    });
    if (!ok) return;
    setBusy(true);
    try {
      await receiveUseSave(rx.id, item.id, consoleAddr(host), t.path, t.user_id);
      pushNotification("success", tr("receive_save_used", undefined, "The save is in. Start the game to load it."));
    } catch (e) {
      pushNotification("error", e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="space-y-2">
      {dialog}
      <p className="text-xs text-[var(--color-muted)]">
        {tr("receive_save_note", undefined, "Your trophies and your account stay your own; only the game's progress changes.")}
      </p>
      {targets.length > 1 && (
        <Select
          label={tr("receive_save_into", undefined, "Into the save of")}
          value={target}
          onChange={(e) => setTarget(e.target.value)}
          options={targets.map((s) => ({ value: s.path, label: tr("receive_save_user", { user: s.user_id }, "User {user}") }))}
        />
      )}
      <Button size="sm" variant="primary" disabled={busy || !t} loading={busy} onClick={() => void use()} data-testid="receive-use-save">
        {tr("receive_use_save", undefined, "Use this save")}
      </Button>
    </div>
  );
}

function ReceivedData({ rx, item }: { rx: ReceiveView; item: OfferedItem }) {
  const tr = useTr();
  const [files, setFiles] = useState<Record<string, unknown> | null>(null);
  const [applied, setApplied] = useState(false);
  useEffect(() => {
    let live = true;
    receiveData(rx.id, item.id)
      .then((d) => live && setFiles(d.files))
      .catch(() => live && setFiles({}));
    return () => {
      live = false;
    };
  }, [rx.id, item.id]);
  if (!files) return null;
  const note = typeof files["note.txt"] === "string" ? (files["note.txt"] as string) : null;
  const controls = files["controls.json"] as
    | { title_id?: string; game?: unknown; kbm_mode?: string | null; keymap?: Record<string, unknown> }
    | undefined;
  const cheats = files["cheats-on.json"] as { on?: string[] } | undefined;
  const applyControls = () => {
    if (!controls) return;
    const tid = (controls.title_id ?? rx.title_id ?? "").toUpperCase();
    if (/^[A-Z]{4}\d{5}$/.test(tid)) {
      if (controls.game) useGameControlsStore.getState().update(tid, readControls(controls.game));
      if (controls.kbm_mode) useKbmStore.getState().setMode(tid, controls.kbm_mode as never);
    }
    setApplied(true);
  };
  return (
    <div className="space-y-2">
      {note && (
        <p className="whitespace-pre-wrap text-sm [overflow-wrap:anywhere]" data-testid="receive-note">
          {note}
        </p>
      )}
      {controls && (
        <div className="flex flex-wrap gap-2">
          <Button size="sm" variant="primary" disabled={applied} onClick={applyControls} data-testid="receive-use-controls">
            {applied ? tr("receive_controls_used", undefined, "Using them") : tr("receive_use_controls", undefined, "Use these controls")}
          </Button>
          {controls.keymap && (
            <Button
              size="sm"
              variant="secondary"
              onClick={() => useRemotePlayStore.getState().setKeymap(controls.keymap as never)}
              data-testid="receive-use-keymap"
            >
              {tr("receive_use_keymap", undefined, "Also use their keyboard keys")}
            </Button>
          )}
        </div>
      )}
      {cheats?.on && (
        <p className="text-xs text-[var(--color-muted)]">
          {cheats.on.length > 0
            ? tr("receive_cheats_on", { list: cheats.on.join(", ") }, "They play with: {list}")
            : tr("receive_cheats_none", undefined, "They play with none on.")}
        </p>
      )}
      {files["stream.json"] !== undefined && (
        <Callout tone="info" title={tr("receive_stream", undefined, "Stream settings")}>
          {tr("receive_stream_body", undefined, "Kept with the other things you received.")}
        </Callout>
      )}
    </div>
  );
}
