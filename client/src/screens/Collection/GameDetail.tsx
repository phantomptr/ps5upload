import { useState } from "react";
import { useNavigate } from "react-router";
import {
  Check,
  Copy,
  Download,
  FolderOpen,
  Link2,
  PackageOpen,
  Trash2,
  Upload,
} from "lucide-react";

import type {
  CollectionGame,
  CollectionLocation,
  CollectionOffer,
  GameConsoleState,
} from "../../api/collection";
import { Badge, Button, Modal, PlatformBadge } from "../../components";
import { pushNotification } from "../../state/notifications";
import { createPkgLink } from "../../state/pkgLinks";
import { installOffers } from "./collectionInstall";
import { writeClipboard } from "../../lib/clipboard";
import {
  extraCopies,
  formatCollectionBytes,
  locationKind,
} from "../../lib/collectionView";
import { openLocalPath } from "../../lib/openLocalPath";
import { isRemotePath } from "../../lib/remotePath";
import { isTauriEnv } from "../../lib/tauriEnv";
import { engineIsOnThisDevice } from "../../state/engine";
import { useCollectionStore } from "../../state/collection";
import { useTr } from "../../state/lang";
import { CollectionCover } from "./CollectionCover";
import { SendToPs5 } from "./SendToPs5";
import { ActivityLine } from "./ActivityLine";
import { useCopyActivity } from "../../state/copyActivity";
import { bestSendable, sendKind } from "../../lib/collectionSend";
import { useCopiesLabel } from "./CollectionCard";

function CopyButton({ text, label }: { text: string; label: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      onClick={async () => {
        if (await writeClipboard(text)) {
          setDone(true);
          setTimeout(() => setDone(false), 1200);
        }
      }}
      className="inline-flex items-center gap-1 rounded-md border border-[var(--color-border)] px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
      title={label}
      aria-label={label}
    >
      {done ? <Check size={12} /> : <Copy size={12} />}
      {label}
    </button>
  );
}

/** Hosts the package for the selected console and copies the link it can install from. */
function InstallLinkButton({ host, path }: { host: string; path: string }) {
  const tr = useTr();
  const [state, setState] = useState<"idle" | "busy" | "done">("idle");
  return (
    <button
      type="button"
      disabled={state === "busy"}
      onClick={async () => {
        setState("busy");
        try {
          const link = await createPkgLink(host, path);
          await writeClipboard(link.url);
          setState("done");
          pushNotification(
            "info",
            tr("collection.link_copied", undefined, "Install link copied"),
            { body: link.url },
          );
          setTimeout(() => setState("idle"), 1500);
        } catch (e) {
          setState("idle");
          pushNotification(
            "error",
            tr(
              "collection.link_failed",
              { error: e instanceof Error ? e.message : String(e) },
              "Could not make a link: {error}",
            ),
          );
        }
      }}
      className="inline-flex items-center gap-1 rounded-md border border-[var(--color-border)] px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)] disabled:opacity-60"
      title={tr(
        "collection.copy_link_hint_v2",
        undefined,
        "A link this PS5 can install the package from by itself (a browser or a package installer on the console). It works while this computer is on, until you stop it under Serving or the app quits.",
      )}
    >
      {state === "done" ? <Check size={12} /> : <Link2 size={12} />}
      {tr("collection.copy_link", undefined, "Copy install link")}
    </button>
  );
}

function kindTone(kind?: string) {
  return kind === "patch" ? "accent" : kind === "dlc" ? "ps5" : "neutral";
}

function LocationRow({
  loc,
  host,
  onTrash,
  onSend,
}: {
  loc: CollectionLocation;
  host: string;
  onTrash?: (paths: string[]) => void;
  /** Opens the game's send panel with this copy picked. */
  onSend: () => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const [activity] = useCopyActivity([loc.absolute_path], host);
  const sendable = !!sendKind(loc) && loc.pkg?.complete !== false && !loc.pkg?.error;
  const noTrash = useCollectionStore(
    (s) => s.settings?.trash_available === false,
  );
  const p = loc.pkg;
  const kindLabel =
    p?.kind === "patch"
      ? tr("collection.kind.patch", undefined, "Update")
      : p?.kind === "dlc"
        ? tr("collection.kind.dlc", undefined, "DLC")
        : p?.kind === "base"
          ? tr("collection.kind.base", undefined, "Game")
          : null;
  // A copy on a saved server is read in place: the tools that touch files here do not apply.
  const onServer = isRemotePath(loc.absolute_path);
  // A file manager can open it only when the engine's disk is this computer's.
  const canReveal = isTauriEnv() && engineIsOnThisDevice() && !onServer;
  return (
    <li className="rounded-lg border border-[var(--color-border)] p-3 text-xs">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium text-[var(--color-text)]">
          {locationKind(loc.type)}
        </span>
        {kindLabel && (
          <Badge tone={kindTone(p?.kind)} size="sm" title={p?.kind_reason}>
            {kindLabel}
            {p?.kind_confident === false ? " ?" : ""}
          </Badge>
        )}
        {p?.version && (
          <span className="text-[var(--color-muted)]">v{p.version}</span>
        )}
        {p?.region && (
          <span className="text-[var(--color-muted)]">{p.region}</span>
        )}
        {p?.complete === false && (
          <Badge tone="warn" size="sm">
            {tr("collection.incomplete", undefined, "Incomplete")}
          </Badge>
        )}
        <span className="ml-auto tabular-nums text-[var(--color-muted)]">
          {formatCollectionBytes(loc.size_bytes)}
        </span>
      </div>
      {p?.title && p.kind === "dlc" && (
        <div className="mt-1 text-[var(--color-text)]">{p.title}</div>
      )}
      {p?.error && (
        <div className="mt-1 text-[var(--color-warn)]">
          {tr(
            "collection.unreadable",
            { why: p.error },
            "Could not be read: {why}",
          )}
        </div>
      )}
      {p?.kind_confident === false && p.kind_reason && (
        <div className="mt-1 text-[var(--color-muted)]">
          {tr(
            "collection.unsure",
            { why: p.kind_reason },
            "Not certain what this is: {why}",
          )}
        </div>
      )}
      <div className="mt-1.5 break-all font-mono text-[0.6875rem] text-[var(--color-muted)]">
        {loc.absolute_path || loc.path}
      </div>
      <div className="mt-1 text-[0.6875rem] text-[var(--color-muted)]">
        {loc.added_at
          ? loc.date_source === "created"
            ? tr(
                "collection.added_on",
                { date: new Date(loc.added_at).toLocaleString() },
                "Added {date}",
              )
            : tr(
                "collection.modified_on",
                { date: new Date(loc.added_at).toLocaleString() },
                "Last changed {date}",
              )
          : null}
        {p?.content_id && (
          <span className="ml-2 font-mono">{p.content_id}</span>
        )}
      </div>
      <div className="mt-2 flex flex-wrap gap-1.5">
        {host && sendable && (
          <button
            type="button"
            onClick={onSend}
            className="inline-flex items-center gap-1 rounded-md border border-[var(--color-accent)] px-2 py-1 text-xs font-medium text-[var(--color-text)] hover:bg-[var(--color-surface-3)]"
          >
            <Upload size={12} />
            {tr("collection.send_go", undefined, "Send to PS5")}
          </button>
        )}
        <CopyButton
          text={loc.absolute_path || loc.path}
          label={tr("collection.copy_path", undefined, "Copy path")}
        />
        {host &&
          !onServer &&
          loc.type === "pkg" &&
          p?.complete !== false &&
          !p?.error && (
            <InstallLinkButton host={host} path={loc.absolute_path} />
          )}
        {loc.type !== "pkg" && (
          // Folders, images and archives are what Convert takes: to a package, or another image.
          <button
            type="button"
            onClick={() =>
              navigate("/convert", { state: { source: loc.absolute_path } })
            }
            className="inline-flex items-center gap-1 rounded-md border border-[var(--color-border)] px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
            title={tr(
              "collection.convert_hint",
              undefined,
              "Open it in Convert: make an installable package, or a different game image.",
            )}
          >
            <PackageOpen size={12} />
            {tr("collection.convert", undefined, "Convert…")}
          </button>
        )}
        {canReveal && (
          <button
            type="button"
            onClick={() => void openLocalPath(loc.absolute_path)}
            className="inline-flex items-center gap-1 rounded-md border border-[var(--color-border)] px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
          >
            <FolderOpen size={12} />
            {tr("collection.show_in_files", undefined, "Show in file manager")}
          </button>
        )}
        {onTrash && !onServer && (
          <button
            type="button"
            onClick={() => onTrash([loc.absolute_path])}
            className="inline-flex items-center gap-1 rounded-md border border-[var(--color-border)] px-2 py-1 text-xs text-[var(--color-bad)] hover:border-[var(--color-bad)]"
          >
            <Trash2 size={12} />
            {noTrash
              ? tr("collection.delete_action", undefined, "Delete…")
              : tr("collection.trash_go", undefined, "Move to Trash")}
          </button>
        )}
      </div>
      {activity && (
        <div className="mt-2">
          <ActivityLine activity={activity} onOpen={() => navigate("installing" in activity && activity.installing ? "/install" : "/upload")} />
        </div>
      )}
    </li>
  );
}

/** What the selected console has of this game, and the buttons that bring it the rest. */
function ConsoleSection({
  game,
  host,
  state,
  sendPath,
  setSendPath,
}: {
  game: CollectionGame;
  host: string;
  state?: GameConsoleState;
  /** The copy the send panel is open for, or null when it is closed. */
  sendPath: string | null;
  setSendPath: (p: string | null) => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const [queued, setQueued] = useState(false);
  const sending = sendPath !== null;
  const sendables = game.locations.filter(
    (l) => sendKind(l) && l.pkg?.complete !== false && !l.pkg?.error,
  );
  const best = bestSendable(game.locations);
  // What the console section's buttons started: the base, update or DLC packages.
  const offerPaths = [
    ...(state?.base ? [state.base.path] : []),
    ...(state?.update ? [state.update.path] : []),
    ...(state?.dlc_missing ?? []).map((d) => d.path),
  ];
  const activities = useCopyActivity(offerPaths, host);
  const installActivity =
    activities.find((a) => a && a.phase !== "done" && a.phase !== "failed") ??
    activities.find((a) => a) ??
    null;
  if (!host) {
    return (
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "collection.no_console",
          undefined,
          "Connect a PS5 to see whether it has this game and install it from here.",
        )}
      </p>
    );
  }
  if (!state) {
    return (
      <p className="text-xs text-[var(--color-muted)]">
        {tr("collection.console_reading", undefined, "Reading this PS5…")}
      </p>
    );
  }
  const queue = (offers: CollectionOffer[]) => {
    const n = installOffers(
      host,
      offers.map((offer) => ({ game, offer })),
    );
    setQueued(true);
    pushNotification(
      "info",
      tr("collection.queued", { n }, "{n} packages queued for install"),
      { link: "/install" },
    );
  };
  const all = [
    ...(state.base ? [state.base] : []),
    ...(state.update ? [state.update] : []),
    ...state.dlc_missing,
  ];
  const uploadOnly = !state.installed && !state.base && state.non_package_copy;
  return (
    <div className="text-sm">
      <div className="text-[var(--color-text)]">
        {state.installed
          ? state.installed_version
            ? tr(
                "collection.on_console_v",
                { v: state.installed_version },
                "Installed on this PS5 · version {v}",
              )
            : tr("collection.on_console", undefined, "Installed on this PS5")
          : tr("collection.not_on_console", undefined, "Not on this PS5")}
      </div>
      {state.registered_from && (
        <div className="mt-0.5 break-all font-mono text-[0.6875rem] text-[var(--color-muted)]">
          {state.registered_from}
        </div>
      )}
      <div className="mt-2 flex flex-wrap gap-2">
        {!state.installed && state.base && (
          <Button
            size="sm"
            variant="primary"
            leftIcon={<Download size={13} />}
            onClick={() => queue(all)}
          >
            {all.length > 1
              ? tr(
                  "collection.install_with",
                  { n: all.length - 1 },
                  "Install with its {n} add-ons",
                )
              : tr("collection.install", undefined, "Install on this PS5")}
          </Button>
        )}
        {state.installed && state.update && (
          <Button
            size="sm"
            variant="primary"
            leftIcon={<Download size={13} />}
            onClick={() => queue([state.update as CollectionOffer])}
          >
            {tr(
              "collection.install_update",
              { v: state.update.version },
              "Install update {v}",
            )}
          </Button>
        )}
        {state.installed && state.dlc_missing.length > 0 && (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<Download size={13} />}
            onClick={() => queue(state.dlc_missing)}
          >
            {tr(
              "collection.install_dlc",
              { n: state.dlc_missing.length },
              "Install DLC ({n})",
            )}
          </Button>
        )}
        {uploadOnly && best && (
          <Button
            size="sm"
            variant={sending ? "secondary" : "primary"}
            leftIcon={<Upload size={13} />}
            onClick={() => setSendPath(sending ? null : best.absolute_path)}
            aria-expanded={sending}
          >
            {tr("collection.send_go", undefined, "Send to PS5")}
          </Button>
        )}
      </div>
      {uploadOnly && !sending && (
        <p className="mt-1 text-xs text-[var(--color-muted)]">
          {tr(
            "collection.send_hint",
            undefined,
            "This game is a folder, image or archive, not a package: send it to the PS5, where ShadowMount+ picks it up.",
          )}
        </p>
      )}
      {sending && (
        <div id={`send-${game.game_id}`}>
          <SendToPs5
            key={sendPath}
            game={game}
            host={host}
            copies={sendables}
            initial={sendables.find((l) => l.absolute_path === sendPath) ?? sendables[0]}
          />
        </div>
      )}
      {(queued || installActivity) && (
        <div className="mt-2">
          {installActivity ? (
            <ActivityLine activity={installActivity} onOpen={() => navigate("/install")} />
          ) : (
            <p className="text-xs text-[var(--color-good)]">
              {tr("collection.queued_short", undefined, "Queued. Progress shows in Install Package.")}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/** One game: every copy and add-on, newest first, with what each one is and where it is. */
export function GameDetail({
  game,
  host,
  consoleState,
  onTrash,
  onClose,
}: {
  game: CollectionGame;
  /** The selected console when its helper answers; "" otherwise. */
  host: string;
  consoleState?: GameConsoleState;
  /** Moves copies to the Trash, after a confirmation. */
  onTrash?: (paths: string[]) => void;
  onClose: () => void;
}) {
  const tr = useTr();
  const copiesLabel = useCopiesLabel();
  const [sendPath, setSendPath] = useState<string | null>(null);
  const openSend = (path: string) => {
    setSendPath(path);
    // The panel is up in "This PS5"; bring it into view from the copy that asked for it.
    requestAnimationFrame(() =>
      document
        .getElementById(`send-${game.game_id}`)
        ?.scrollIntoView({ behavior: "smooth", block: "nearest" }),
    );
  };
  const extra = extraCopies(game).filter((l) => !isRemotePath(l.absolute_path));
  const extraBytes = extra.reduce((n, l) => n + l.size_bytes, 0);
  return (
    <Modal
      open
      onClose={onClose}
      size="xl"
      title={game.title}
      bodyClassName="p-4 sm:p-5"
    >
      <div className="flex flex-col gap-4 sm:flex-row">
        <div className="w-32 shrink-0 overflow-hidden rounded-xl border border-[var(--color-border)] sm:w-40">
          <CollectionCover game={game} />
        </div>
        <div className="min-w-0 flex-1 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            <PlatformBadge platform={game.platform.toLowerCase()} />
            <span className="font-mono text-xs">{game.game_id}</span>
            {game.is_duplicate && (
              <Badge tone="warn" size="sm">
                {tr("collection.duplicate", undefined, "Duplicate")}
              </Badge>
            )}
          </div>
          <div className="mt-2 text-[var(--color-muted)]">
            {copiesLabel(game)} · {formatCollectionBytes(game.total_size_bytes)}
          </div>
          {game.first_added_at && (
            <div className="mt-1 text-xs text-[var(--color-muted)]">
              {tr(
                "collection.first_added",
                { date: new Date(game.first_added_at).toLocaleDateString() },
                "First copy arrived {date}",
              )}
            </div>
          )}
          {game.title_source === "online" && (
            <div className="mt-1 text-xs text-[var(--color-muted)]">
              {tr(
                "collection.title_online",
                undefined,
                "Title looked up online: this game's files do not name it.",
              )}
            </div>
          )}
          <div className="mt-3 flex flex-wrap gap-1.5">
            <CopyButton
              text={game.title}
              label={tr("collection.copy_name", undefined, "Copy name")}
            />
            <CopyButton
              text={game.game_id}
              label={tr("collection.copy_id", undefined, "Copy game ID")}
            />
          </div>
          {onTrash && extra.length > 0 && (
            <div className="mt-3">
              <Button
                size="sm"
                variant="danger"
                leftIcon={<Trash2 size={13} />}
                onClick={() => onTrash(extra.map((l) => l.absolute_path))}
                title={tr(
                  "collection.keep_largest_hint",
                  undefined,
                  "Keeps the largest full copy and moves the other copies to the Trash. Updates and DLC stay.",
                )}
              >
                {tr(
                  "collection.keep_largest",
                  { size: formatCollectionBytes(extraBytes) },
                  "Keep the largest copy, free {size}",
                )}
              </Button>
            </div>
          )}
        </div>
      </div>
      <h3 className="mb-2 mt-5 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {tr("collection.this_console", undefined, "This PS5")}
      </h3>
      <div className="rounded-lg border border-[var(--color-border)] p-3">
        <ConsoleSection
          // Per console: "Queued" and the send panel's drive belong to the console they were for.
          key={host}
          game={game}
          host={host}
          state={consoleState}
          sendPath={sendPath}
          setSendPath={setSendPath}
        />
      </div>
      <h3 className="mb-2 mt-5 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {tr(
          "collection.locations",
          { n: game.locations.length },
          "On disk ({n})",
        )}
      </h3>
      <ul className="flex flex-col gap-2">
        {game.locations.map((l) => (
          <LocationRow
            key={`${l.root}|${l.path}`}
            loc={l}
            host={host}
            onTrash={onTrash}
            onSend={() => openSend(l.absolute_path)}
          />
        ))}
      </ul>
    </Modal>
  );
}
