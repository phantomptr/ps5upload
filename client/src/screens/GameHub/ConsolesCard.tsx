import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { Download, Monitor, Play, RefreshCw, Square, Upload } from "lucide-react";

import type { CollectionLocation } from "../../api/collection";
import type { ConsoleEntry, GameView } from "../../api/games";
import { Badge, Button, Card, Spinner } from "../../components";
import { bestSendable, sendKind } from "../../lib/collectionSend";
import {
  ageLabel,
  gamePath,
  hasTitleId,
  needsReread,
  offersAfterReread,
  queueLinkFor,
  rowActions,
  type RowAction,
} from "../../lib/gamePage";
import { useConnectionStore } from "../../state/connection";
import { useCopyActivity } from "../../state/copyActivity";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { ActivityLine } from "./ActivityLine";
import { installOffers } from "./collectionInstall";
import { SendToPs5 } from "./SendToPs5";

/** Live enough to say "now": read in the last minute. */
const LIVE_SEC = 60;

/** Every saved console and this game on it: what it has, how old that is, and what to do. */
export function ConsolesCard({
  titleId,
  consoles,
  connected,
  view,
  refresh,
  onPlay,
  launching,
  running = false,
  onStop,
  stopping = false,
  sendHost,
  setSendHost,
}: {
  titleId: string;
  /** The saved consoles, in the roster's order. */
  consoles: { host: string; name: string }[];
  /** The connected console's IP when its helper answers; "" otherwise. */
  connected: string;
  view: GameView | null;
  refresh: (host: string) => Promise<ConsoleEntry>;
  onPlay: () => void;
  launching: boolean;
  /** The game is running on the connected console: its row offers Close game instead of Play. */
  running?: boolean;
  onStop?: () => void;
  stopping?: boolean;
  /** The console whose send panel is open, or null. */
  sendHost: string | null;
  setSendHost: (host: string | null) => void;
}) {
  const tr = useTr();
  // The connected console first, then the roster's order.
  const hosts = [...new Set(consoles.map((c) => c.host).filter(Boolean))].sort(
    (a, b) => Number(b === connected) - Number(a === connected),
  );
  const nameOf = (h: string) => consoles.find((c) => c.host === h)?.name || h;
  return (
    <Card>
      <h2 className="mb-3 flex items-center gap-2 text-sm font-semibold">
        <Monitor size={16} className="text-[var(--color-muted)]" />
        {tr("game_consoles_title", undefined, "Consoles")}
      </h2>
      {!hasTitleId(titleId) ? (
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "game_consoles_no_id",
            undefined,
            "This copy has no title ID (no param.json), so the consoles cannot say whether they have it. You can still send it from the copies below.",
          )}
        </p>
      ) : hosts.length === 0 ? (
        <p className="text-sm text-[var(--color-muted)]">
          {tr("game_consoles_none", undefined, "Add a console under Connection to see whether it has this game.")}
        </p>
      ) : (
        <ul className="divide-y divide-[var(--color-border)]" data-testid="game-consoles">
          {hosts.map((h) => (
            <ConsoleRow
              key={h}
              host={h}
              name={nameOf(h)}
              connected={h === connected}
              title={view?.title ?? titleId}
              titleId={titleId}
              entry={view?.consoles.find((c) => c.host === h)}
              copies={view?.copies ?? []}
              refresh={refresh}
              onPlay={onPlay}
              launching={launching}
              running={running && h === connected}
              onStop={onStop}
              stopping={stopping}
              sending={sendHost === h}
              setSending={(on) => setSendHost(on ? h : null)}
            />
          ))}
        </ul>
      )}
    </Card>
  );
}

function ConsoleRow({
  host,
  name,
  connected,
  title,
  titleId,
  entry,
  copies,
  refresh,
  onPlay,
  launching,
  running,
  onStop,
  stopping,
  sending,
  setSending,
}: {
  host: string;
  name: string;
  connected: boolean;
  title: string;
  titleId: string;
  entry: ConsoleEntry | undefined;
  copies: CollectionLocation[];
  refresh: (host: string) => Promise<ConsoleEntry>;
  onPlay: () => void;
  launching: boolean;
  running: boolean;
  onStop?: () => void;
  stopping: boolean;
  sending: boolean;
  setSending: (on: boolean) => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);

  const actions = rowActions(entry, copies, connected);
  const sendables = copies.filter((l) => sendKind(l) && l.pkg?.complete !== false && !l.pkg?.error);
  const offerPaths = [
    ...(entry?.base ? [entry.base.path] : []),
    ...(entry?.update ? [entry.update.path] : []),
    ...(entry?.dlc_missing ?? []).map((d) => d.path),
    ...sendables.map((l) => l.absolute_path),
  ];
  const activities = useCopyActivity(offerPaths, host);
  const activity =
    activities.find((a) => a && a.phase !== "done" && a.phase !== "failed") ?? activities.find((a) => a) ?? null;
  const connectedHost = useConnectionStore((s) => s.host ?? "");

  const doRefresh = async () => {
    setBusy(true);
    setFailed(null);
    try {
      await refresh(host);
    } catch (e) {
      setFailed(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const install = async (kind: "install" | "update" | "dlc", chosen: NonNullable<ConsoleEntry["base"]>[]) => {
    // A saved state can be old: what the console has now decides what goes in. When it cannot
    // be read, the choice stands (the queue still refuses to erase an installed game).
    let offers = chosen;
    if (entry && needsReread(entry.read_at, now)) {
      const fresh = await refresh(host).catch(() => null);
      if (fresh) offers = offersAfterReread(kind, fresh, copies);
    }
    if (offers.length === 0) {
      pushNotification("info", tr("game_already_on", { name }, "{name} has it already"));
      return;
    }
    const n = installOffers(
      host,
      offers.map((offer) => ({ game: { title }, offer })),
    );
    pushNotification(
      "info",
      tr("game_queued_on", { n, name }, "{n} packages queued for {name}"),
      { link: gamePath(titleId) },
    );
  };

  const state = !entry
    ? tr("game_state_unknown", undefined, "Not checked yet")
    : entry.installed
      ? entry.version
        ? tr("game_state_installed_v", { v: entry.version }, "Installed · v{v}")
        : tr("game_state_installed", undefined, "Installed")
      : tr("game_state_not_installed", undefined, "Not installed");
  const age = entry
    ? ageLabel(entry.read_at, now, connected && now / 1000 - entry.read_at < LIVE_SEC, tr)
    : "";

  const button = (a: RowAction) => {
    switch (a.kind) {
      case "play":
        if (running && onStop) {
          return (
            <Button
              key="play"
              size="sm"
              variant="danger"
              leftIcon={stopping ? <Spinner size={13} /> : <Square size={13} />}
              disabled={stopping}
              onClick={onStop}
            >
              {stopping
                ? tr("installed_stopping", undefined, "Closing…")
                : tr("installed_stop", undefined, "Close game")}
            </Button>
          );
        }
        return (
          <Button
            key="play"
            size="sm"
            variant="primary"
            leftIcon={launching ? <Spinner size={13} /> : <Play size={13} />}
            disabled={launching}
            onClick={onPlay}
          >
            {launching ? tr("game_hub_launching", undefined, "Starting…") : tr("game_play", undefined, "Play")}
          </Button>
        );
      case "install":
        return (
          <Button key="install" size="sm" variant="primary" leftIcon={<Download size={13} />} onClick={() => void install("install", a.offers)}>
            {a.offers.length > 1
              ? tr("collection.install_with", { n: a.offers.length - 1 }, "Install with its {n} add-ons")
              : tr("game_install", undefined, "Install")}
          </Button>
        );
      case "update":
        return (
          <Button key="update" size="sm" variant="primary" leftIcon={<Download size={13} />} onClick={() => void install("update", [a.offer])}>
            {tr("collection.install_update", { v: a.offer.version }, "Install update {v}")}
          </Button>
        );
      case "dlc":
        return (
          <Button key="dlc" size="sm" variant="secondary" leftIcon={<Download size={13} />} onClick={() => void install("dlc", a.offers)}>
            {tr("collection.install_dlc", { n: a.offers.length }, "Install DLC ({n})")}
          </Button>
        );
      case "send":
        return (
          <Button
            key="send"
            size="sm"
            variant={sending ? "secondary" : "primary"}
            leftIcon={<Upload size={13} />}
            aria-expanded={sending}
            onClick={() => setSending(!sending)}
          >
            {tr("collection.send_go", undefined, "Send to PS5")}
          </Button>
        );
    }
  };

  const best = bestSendable(copies);
  return (
    <li className="py-3 first:pt-0 last:pb-0" data-testid={`game-console-${host}`}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <span className="font-medium">{name}</span>
            {connected && (
              <Badge tone="good" size="sm">
                {tr("game_connected", undefined, "Connected")}
              </Badge>
            )}
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            <span className={entry?.installed ? "text-[var(--color-text)]" : undefined}>{state}</span>
            {age && <span> · {age}</span>}
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {actions.map(button)}
          <Button
            size="sm"
            variant="ghost"
            aria-label={tr("game_refresh_console", { name }, "Check {name} again")}
            title={tr("game_refresh_console", { name }, "Check {name} again")}
            leftIcon={busy ? <Spinner size={13} /> : <RefreshCw size={13} />}
            disabled={busy}
            onClick={() => void doRefresh()}
          />
        </div>
      </div>
      {failed && (
        <p className="mt-1.5 text-xs text-[var(--color-bad)]" role="alert">
          {tr("game_refresh_failed", { name, why: failed }, "Couldn't reach {name}: {why}")}
        </p>
      )}
      {sending && best && (
        <SendToPs5 key={host} game={{ title }} host={host} copies={sendables} initial={best} />
      )}
      {activity && !sending && (
        <div className="mt-2">
          <ActivityLine
            activity={activity}
            onOpen={() =>
              navigate(queueLinkFor(host, connectedHost, "installing" in activity && activity.installing))
            }
          />
        </div>
      )}
    </li>
  );
}
