import { formatDate } from "../../lib/formatDate";
import { useState } from "react";
import { useNavigate } from "react-router";
import { Copy, Download, FolderOpen, Link2, PackageOpen, ShieldCheck, Trash2, Upload } from "lucide-react";

import type { CollectionLocation } from "../../api/collection";
import { Badge, OverflowMenu, type OverflowMenuItem } from "../../components";
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
import { hostOf } from "../../lib/addr";
import { gamePath } from "../../lib/gamePage";
import { installOffers } from "./collectionInstall";
import { ActivityLine } from "./ActivityLine";
import { isConvertible } from "../../lib/collectionConvert";
import { startCopyChecks, useCopyCheck } from "../../state/copyChecks";
import { CopyCheckLine } from "./CopyCheckLine";
import { humanizeCaught } from "../../lib/humanizeError";

// One copy of a game on the drives: what it is, where, and what can be done with it. One
// line of facts, one of path, the primary action as a button and the rest behind a menu, so
// a game with several copies reads as a list rather than a stack of panels.

function kindTone(kind?: string) {
  return kind === "patch" ? "accent" : kind === "dlc" ? "ps5" : "neutral";
}

/** PARAM.SFO category from the collection's pkg kind: orders installs base → update → DLC. */
function categoryOf(kind: string | undefined): string {
  return kind === "patch" ? "gp" : kind === "dlc" ? "ac" : "gd";
}

export function CopyRow({
  loc,
  host,
  game,
  onTrash,
  onSend,
}: {
  loc: CollectionLocation;
  host: string;
  /** The game this copy belongs to, for the install queue's label and link. */
  game: { title: string; titleId: string };
  onTrash?: (paths: string[]) => void;
  /** Opens the game's send panel with this copy picked. */
  onSend: () => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const [activity] = useCopyActivity([loc.absolute_path], host);
  const connectedHost = useConnectionStore((s) => s.host);
  const [linkBusy, setLinkBusy] = useState(false);
  const check = useCopyCheck(loc.absolute_path);
  const checking = check?.state === "queued" || check?.state === "running";
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
  const usable = loc.type === "pkg" && p?.complete !== false && !p?.error;
  const path = loc.absolute_path || loc.path;

  const install = () => {
    installOffers(hostOf(host), [
      {
        game: { title: game.title },
        offer: {
          path: loc.absolute_path,
          name: loc.path.split(/[\\/]/).pop() || loc.path,
          version: p?.version ?? "",
          content_id: p?.content_id ?? "",
          title: p?.title ?? game.title,
          size_bytes: loc.size_bytes,
          category: categoryOf(p?.kind),
        },
      },
    ]);
    pushNotification("info", tr("collection.queued", { n: 1 }, "{n} packages queued for install"), {
      link: gamePath(game.titleId),
    });
  };
  const copyLink = async () => {
    setLinkBusy(true);
    try {
      const link = await createPkgLink(host, loc.absolute_path);
      await writeClipboard(link.url);
      pushNotification("info", tr("collection.link_copied", undefined, "Install link copied"), {
        body: link.url,
      });
    } catch (e) {
      pushNotification(
        "error",
        tr(
          "collection.link_failed",
          { error: humanizeCaught(e) },
          "Could not make a link: {error}",
        ),
      );
    } finally {
      setLinkBusy(false);
    }
  };

  // The one thing most people want from this copy: send a folder or image, install a package.
  const primary =
    host && sendable
      ? { icon: <Upload size={12} />, label: tr("collection.send_go", undefined, "Send to PS5"), run: onSend }
      : host && usable
        ? { icon: <Download size={12} />, label: tr("game_install", undefined, "Install"), run: install }
        : null;
  const more: OverflowMenuItem[] = [
    {
      icon: <Copy size={12} />,
      label: tr("collection.copy_path", undefined, "Copy path"),
      onSelect: () => {
        void writeClipboard(path).then((ok) => {
          if (ok) pushNotification("info", tr("collection.path_copied", undefined, "Path copied"), { body: path });
        });
      },
    },
    ...(host && usable && !onServer
      ? [
          {
            icon: <Link2 size={12} />,
            label: tr("collection.copy_link", undefined, "Copy install link"),
            onSelect: () => void copyLink(),
            loading: linkBusy,
            title: tr(
              "collection.copy_link_hint_v2",
              undefined,
              "A link this PS5 can install the package from by itself (a browser or a package installer on the console). It works while this computer is on, until you stop it under Serving or the app quits.",
            ),
          },
        ]
      : []),
    ...(isConvertible(loc)
      ? [
          {
            // Folders, images, archives and base packages are what Convert takes: to a package,
            // another image, or (from a package) a folder.
            icon: <PackageOpen size={12} />,
            label: tr("collection.convert", undefined, "Convert…"),
            onSelect: () => navigate("/workshop", { state: { source: loc.absolute_path } }),
            title: tr(
              "collection.convert_hint_v2",
              undefined,
              "Open it in Workshop: make an installable package, or a different game image.",
            ),
          },
        ]
      : []),
    {
      icon: <ShieldCheck size={12} />,
      label: check
        ? tr("copycheck.again", undefined, "Check it again")
        : tr("copycheck.go", undefined, "Check this copy"),
      title: tr(
        "collection.check_hint",
        undefined,
        "Read every copy back to find one that is damaged or cut short. Runs on this computer, one copy at a time.",
      ),
      disabled: checking,
      onSelect: () => void startCopyChecks([loc.absolute_path]),
    },
    ...(canReveal
      ? [
          {
            icon: <FolderOpen size={12} />,
            label: tr("collection.show_in_files", undefined, "Show in file manager"),
            onSelect: () => void openLocalPath(loc.absolute_path),
          },
        ]
      : []),
    ...(onTrash && !onServer
      ? [
          {
            icon: <Trash2 size={12} />,
            label: noTrash
              ? tr("collection.delete_action", undefined, "Delete…")
              : tr("collection.trash_go", undefined, "Move to Trash"),
            onSelect: () => onTrash([loc.absolute_path]),
            destructive: true,
          },
        ]
      : []),
  ];

  return (
    <li
      className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] px-3.5 py-2.5 text-xs"
      data-testid="game-copy"
    >
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="font-medium text-[var(--color-text)]">{locationKind(loc.type)}</span>
        {kindLabel && (
          <Badge tone={kindTone(p?.kind)} size="sm" title={p?.kind_reason}>
            {kindLabel}
            {p?.kind_confident === false ? " ?" : ""}
          </Badge>
        )}
        {p?.title && p.kind === "dlc" && <span className="text-[var(--color-text)]">{p.title}</span>}
        {p?.version && <span className="text-[var(--color-muted)]">v{p.version}</span>}
        {p?.min_fw && p.platform === "PS5" && (
          <span className="text-[var(--color-muted)]">
            {tr("collection.copy_fw", { fw: p.min_fw }, "FW {fw}+")}
          </span>
        )}
        {p?.region && <span className="text-[var(--color-muted)]">{p.region}</span>}
        {p?.complete === false && (
          <Badge tone="warn" size="sm">
            {tr("collection.incomplete", undefined, "Incomplete")}
          </Badge>
        )}
        <span className="tabular-nums text-[var(--color-muted)]">{formatCollectionBytes(loc.size_bytes)}</span>
        {activity && (
          <ActivityLine
            activity={activity}
            onOpen={() =>
              navigate(queueLinkFor(host, connectedHost))
            }
          />
        )}
        <span className="ms-auto flex items-center gap-1">
          {primary && (
            <button
              type="button"
              onClick={primary.run}
              className="inline-flex min-h-8 items-center gap-1.5 rounded-full border border-[var(--color-accent)] px-3 py-1 text-xs font-medium text-[var(--color-text)] hover:bg-[var(--color-accent-soft)]"
            >
              {primary.icon}
              {primary.label}
            </button>
          )}
          <OverflowMenu
            items={more}
            ariaLabel={tr("collection.more_actions", undefined, "More actions for this copy")}
            buttonTitle={tr("collection.more_actions", undefined, "More actions for this copy")}
          />
        </span>
      </div>
      <div className="mt-1 break-all font-mono text-[0.6875rem] text-[var(--color-muted)]" title={path}>
        {path}
      </div>
      <CopyCheckLine path={loc.absolute_path} />
      {(loc.added_at || p?.content_id || p?.error || (p?.kind_confident === false && p.kind_reason)) && (
        <div className="mt-0.5 text-[0.6875rem] text-[var(--color-muted)]">
          {loc.added_at
            ? loc.date_source === "created"
              ? tr("collection.added_on", { date: formatDate(new Date(loc.added_at)) }, "Added {date}")
              : tr("collection.modified_on", { date: formatDate(new Date(loc.added_at)) }, "Last changed {date}")
            : null}
          {p?.content_id && <span className="ms-2 font-mono">{p.content_id}</span>}
          {p?.error && (
            <span className="ms-2 text-[var(--color-warn)]">
              {tr("collection.unreadable", { why: p.error }, "Could not be read: {why}")}
            </span>
          )}
          {p?.kind_confident === false && p.kind_reason && (
            <span className="ms-2">{tr("collection.unsure", { why: p.kind_reason }, "Not certain what this is: {why}")}</span>
          )}
        </div>
      )}
    </li>
  );
}
