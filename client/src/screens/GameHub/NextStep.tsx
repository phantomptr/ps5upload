import type { ReactNode } from "react";
import { useNavigate } from "react-router";
import { ArrowRight, Download, Hammer, Send, Shrink } from "lucide-react";

import type { CollectionOffer } from "../../api/collection";
import type { GameView } from "../../api/games";
import { Button, Card } from "../../components";
import { hostOf } from "../../lib/addr";
import { openInConvert } from "../../lib/collectionActions";
import { shrinkCandidate } from "../../lib/collectionInsights";
import { bestSendable } from "../../lib/collectionSend";
import { formatCollectionBytes } from "../../lib/collectionView";
import {
  buildExtension,
  useConvertQueue,
  type ConvertItem,
} from "../../state/convertQueue";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { installOffers } from "./collectionInstall";

/** Where the game is, in one line, and the one thing most worth doing next with the connected
 *  console: install the newer update this computer has, send the game it lacks, add missing
 *  DLC, or make the copy here smaller. Nothing when there is nothing to do. */
export function NextStep({
  view,
  connectedHost,
  consoleName,
  consoles,
  onSend,
}: {
  view: GameView | null;
  /** The connected console when its helper answers; "" otherwise. */
  connectedHost: string;
  consoleName: string;
  /** How many consoles are saved. */
  consoles: number;
  onSend: () => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  if (!view) return null;
  const copies = view.copies;
  const entry = connectedHost
    ? view.consoles.find((c) => hostOf(c.host) === hostOf(connectedHost))
    : undefined;
  const onConsoles = view.consoles.filter((c) => c.installed).length;
  const copyBytes = copies.reduce((n, l) => n + l.size_bytes, 0);
  const queue = (offers: CollectionOffer[]) => {
    const n = installOffers(
      connectedHost,
      offers.map((offer) => ({ game: { title: view.title }, offer })),
    );
    pushNotification(
      "info",
      tr("collection.queued", { n }, "{n} packages queued for install"),
      {
        link: "/send",
      },
    );
  };

  let step: {
    text: string;
    action: string;
    icon: ReactNode;
    run: () => void;
  } | null = null;
  const shrink = shrinkCandidate({
    platform: view.platform,
    locations: copies,
  });
  if (entry?.installed && entry.update) {
    const update = entry.update;
    step = {
      text: tr(
        "game_next_update",
        { v: update.version, name: consoleName },
        "Update {v} is on this computer; {name} does not have it yet.",
      ),
      action: tr(
        "collection.install_update",
        { v: update.version },
        "Install update {v}",
      ),
      icon: <Download size={13} />,
      run: () => queue([update]),
    };
  } else if (
    entry &&
    !entry.installed &&
    (entry.base || bestSendable(copies))
  ) {
    step = {
      text: tr(
        "game_next_send",
        { name: consoleName },
        "{name} does not have this game yet.",
      ),
      action: tr("game_next_send_go", { name: consoleName }, "Send to {name}"),
      icon: <Send size={13} />,
      run: onSend,
    };
  } else if (entry?.installed && entry.dlc_missing.length > 0) {
    const dlc = entry.dlc_missing;
    step = {
      text: tr(
        "game_next_dlc",
        { n: dlc.length, name: consoleName },
        "{n} DLC on this computer are missing on {name}.",
      ),
      action: tr(
        "collection.install_dlc",
        { n: dlc.length },
        "Install missing DLC ({n})",
      ),
      icon: <Download size={13} />,
      run: () => queue(dlc),
    };
  } else if (shrink) {
    step = {
      text: tr(
        "game_next_shrink",
        { size: formatCollectionBytes(shrink.size_bytes) },
        "The {size} copy here could take less room as a compressed image (.ffpfsc).",
      ),
      action: tr("collection.shrink", undefined, "Make it smaller…"),
      icon: <Shrink size={13} />,
      run: () => openInConvert(navigate, [shrink], { compress: true }),
    };
  }

  const where = [
    copies.length === 1
      ? tr(
          "game_where_here_one",
          { size: formatCollectionBytes(copyBytes) },
          "One copy on this computer ({size})",
        )
      : copies.length > 0
        ? tr(
            "game_where_here",
            { n: copies.length, size: formatCollectionBytes(copyBytes) },
            "{n} copies on this computer ({size})",
          )
        : tr("game_where_none_here", undefined, "No copy on this computer"),
    consoles > 0
      ? tr(
          "game_where_consoles",
          { n: onConsoles, m: consoles },
          "installed on {n} of {m} PS5s",
        )
      : null,
  ].filter(Boolean);

  return (
    <div
      className="surface-panel flex flex-wrap items-center gap-3 !rounded-[var(--radius-card)] px-5 py-3 text-sm"
      data-testid="game-next-step"
    >
      <span className="min-w-0 flex-1">
        <span className="text-[var(--color-muted)]">{where.join(" · ")}</span>
        {step && (
          <>
            <ArrowRight
              size={12}
              className="mx-2 inline text-[var(--color-muted)]"
              aria-hidden
            />
            {step.text}
          </>
        )}
      </span>
      {step && (
        <Button
          size="sm"
          variant="primary"
          leftIcon={step.icon}
          onClick={step.run}
        >
          {step.action}
        </Button>
      )}
    </div>
  );
}

/** This game's builds in Convert (from any of its copies): where each stands, and Install for
 *  a finished package. Nothing when Convert has none. */
export function ConvertBuildsCard({
  view,
  connectedHost,
}: {
  view: GameView | null;
  connectedHost: string;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const items = useConvertQueue((s) => s.items);
  if (!view) return null;
  const sources = new Set(view.copies.map((l) => l.absolute_path));
  const id = view.title_id.toUpperCase();
  const mine = items.filter(
    (i) => sources.has(i.source) || i.source.toUpperCase().includes(id),
  );
  if (mine.length === 0) return null;
  const status = (i: ConvertItem) =>
    ({
      pending: tr("game_build_pending", undefined, "Waiting"),
      running: tr("game_build_running", undefined, "Building"),
      installing: tr("game_build_installing", undefined, "Installing"),
      handed: tr("game_build_handed", undefined, "In the install queue"),
      done: tr("game_build_done", undefined, "Built"),
      failed: tr("game_build_failed", undefined, "Failed"),
      cancelled: tr("cq_cancelled", undefined, "Cancelled"),
    })[i.status];
  return (
    <Card>
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <h2 className="flex items-center gap-3 text-[1.0625rem] font-semibold tracking-[-0.01em]">
          <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
            <Hammer size={15} aria-hidden />
          </span>
          {tr(
            "game_builds_title_v2",
            { n: mine.length },
            "Built in Workshop ({n})",
          )}
        </h2>
        <Button
          size="sm"
          variant="secondary"
          onClick={() => navigate("/workshop")}
        >
          {tr("open_workshop", undefined, "Open Workshop")}
        </Button>
      </div>
      <ul className="flex flex-col gap-1.5" data-testid="game-builds">
        {mine.map((i) => {
          const pkg =
            i.packagePath && /\.pkg$/i.test(i.packagePath)
              ? i.packagePath
              : null;
          return (
            <li
              key={i.id}
              className="flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] px-3.5 py-2.5 text-xs"
            >
              <span className="font-medium">{buildExtension(i.build)}</span>
              <span
                className={
                  i.status === "failed"
                    ? "text-[var(--color-bad)]"
                    : i.status === "done"
                      ? "text-[var(--color-good)]"
                      : "text-[var(--color-muted)]"
                }
                title={i.error}
              >
                {status(i)}
              </span>
              <span
                className="min-w-0 flex-1 truncate font-mono text-[0.6875rem] text-[var(--color-muted)]"
                title={i.packagePath ?? i.source}
              >
                {i.packagePath ?? i.source}
              </span>
              {pkg && i.status === "done" && connectedHost && (
                <Button
                  size="sm"
                  variant="secondary"
                  leftIcon={<Download size={13} />}
                  onClick={() => {
                    const n = installOffers(connectedHost, [
                      {
                        game: { title: view.title },
                        offer: {
                          path: pkg,
                          name: pkg.split(/[\\/]/).pop() ?? pkg,
                          version: "",
                          content_id: "",
                          title: view.title,
                          size_bytes: 0,
                          category: "gd",
                        },
                      },
                    ]);
                    pushNotification(
                      "info",
                      tr(
                        "collection.queued",
                        { n },
                        "{n} packages queued for install",
                      ),
                      { link: "/send" },
                    );
                  }}
                >
                  {tr("game_build_install", undefined, "Install")}
                </Button>
              )}
            </li>
          );
        })}
      </ul>
    </Card>
  );
}
