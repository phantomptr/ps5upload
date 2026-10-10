import { formatDate } from "../../lib/formatDate";
import { useState } from "react";
import { useNavigate } from "react-router";
import { Check, Copy, FolderOpen, Link2, PackageOpen, Trash2, Upload } from "lucide-react";

import type { CollectionLocation } from "../../api/collection";
import { Badge } from "../../components";
import { pushNotification } from "../../state/notifications";
import { createPkgLink } from "../../state/pkgLinks";
import { writeClipboard } from "../../lib/clipboard";
import { formatCollectionBytes, locationKind } from "../../lib/collectionView";
import { openLocalPath } from "../../lib/openLocalPath";
import { isRemotePath } from "../../lib/remotePath";
import { isTauriEnv } from "../../lib/tauriEnv";
import { queueLinkFor } from "../../lib/gamePage";
import { engineIsOnThisDevice } from "../../state/engine";
import { useCollectionStore } from "../../state/collection";
import { useConnectionStore } from "../../state/connection";
import { useCopyActivity } from "../../state/copyActivity";
import { useTr } from "../../state/lang";
import { sendKind } from "../../lib/collectionSend";
import { ActivityLine } from "./ActivityLine";

// One copy of a game on the drives: what it is, where, and what can be done with it.

export function CopyButton({ text, label }: { text: string; label: string }) {
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
      className="inline-flex min-h-8 items-center gap-1.5 rounded-full border border-[var(--color-border-strong)] px-3 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
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
      className="inline-flex min-h-8 items-center gap-1.5 rounded-full border border-[var(--color-border-strong)] px-3 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)] disabled:opacity-60"
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

export function CopyRow({
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
  const connectedHost = useConnectionStore((s) => s.host);
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
    <li className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-4 text-xs">
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
                { date: formatDate(new Date(loc.added_at)) },
                "Added {date}",
              )
            : tr(
                "collection.modified_on",
                { date: formatDate(new Date(loc.added_at)) },
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
            className="inline-flex min-h-8 items-center gap-1.5 rounded-full border border-[var(--color-accent)] px-3 py-1 text-xs font-medium text-[var(--color-text)] hover:bg-[var(--color-accent-soft)]"
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
            className="inline-flex min-h-8 items-center gap-1.5 rounded-full border border-[var(--color-border-strong)] px-3 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
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
            className="inline-flex min-h-8 items-center gap-1.5 rounded-full border border-[var(--color-border-strong)] px-3 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
          >
            <FolderOpen size={12} />
            {tr("collection.show_in_files", undefined, "Show in file manager")}
          </button>
        )}
        {onTrash && !onServer && (
          <button
            type="button"
            onClick={() => onTrash([loc.absolute_path])}
            className="inline-flex min-h-8 items-center gap-1.5 rounded-full border border-[var(--color-border-strong)] px-3 py-1 text-xs text-[var(--color-bad)] hover:border-[var(--color-bad)]"
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
