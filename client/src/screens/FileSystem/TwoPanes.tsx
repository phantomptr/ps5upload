// Files' two panes (#952): today's PS5 listing on one side, and on the other a place the app can
// already read and write: this computer, a saved server or another console. Copy and Move between
// them run the transfers Files already had (see panePlan.ts for which one each pair uses).
//
// Not a second file manager: the other side only browses and ticks. Renaming, deleting and the
// rest stay on the PS5 side, which is the Files listing itself.

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { ArrowRight, ChevronUp, File as FileIcon, Folder, HardDrive, RefreshCw } from "lucide-react";

import { Button } from "../../components/Button";
import { useConfirm } from "../../components/ConfirmDialog";
import { localFs } from "../../api/localFs";
import { fsDelete } from "../../api/ps5";
import type { Connection } from "../../api/remote";
import { consoleAddr, hostOf } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { humanizeCaught } from "../../lib/humanizeError";
import { krakenKeptPaths } from "../../lib/krakenGuard";
import {
  CONSOLE_ROOTS,
  findInPlace,
  listPlace,
  placeParent,
  placeTransferPath,
  type Place,
  type PlaceEntry,
} from "../../lib/placeList";
import { isTauriEnv } from "../../lib/tauriEnv";
import { queueConsoleSend, whenQueueRowEnds } from "../../state/consoleSendQueue";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { useRosterStore, withConsolePrefix } from "../../state/roster";
import { mayDeleteSourceAfterUpload } from "../../state/uploadQueue";
import type { FsPickedItem } from "../../state/fsUploadPlan";
import {
  copyLooksWhole,
  namesAlreadyThere,
  paneRoute,
  readOtherPane,
  runPaneTransfer,
  writeOtherPane,
  type CopyOutcome,
  type PaneBlock,
  type PaneOp,
  type Toward,
} from "./panePlan";

/** One ticked item on either side. `path` is where it is on its own side. */
export interface PaneItem {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
}

/** What the PS5 side (the Files screen) lends the two panes. */
export interface PsSide {
  /** The console Files is on, and the folder it shows. */
  host: string;
  path: string;
  /** Ticked rows, and every name in the folder (to ask before replacing). */
  selected: PaneItem[];
  names: string[];
  /** A download is running on this console: Files runs one at a time. */
  downloadBusy: boolean;
  /** Files' Download of `item` (in the PS5 folder `from`) into a folder already chosen: on this
   *  computer, or `remote://` on a saved server. `move` (a saved server only): the engine checks
   *  every file and folder of the copy, then deletes the source itself (#953). */
  download: (
    item: PaneItem,
    to: { dir: string; from: string; move?: boolean },
    server?: Connection,
  ) => Promise<CopyOutcome>;
  /** Files' Add files, into the folder shown. */
  upload: (paths: string[], known: FsPickedItem[]) => Promise<void>;
  clearSelection: () => void;
  refresh: () => void;
}

const join = (dir: string, name: string) => `${dir === "/" ? "" : dir.replace(/\/+$/, "")}/${name}`;

function blockText(tr: ReturnType<typeof useTr>, block: PaneBlock): string {
  if (block === "no-delete-server")
    return tr("two_panes_no_move_server", undefined, "No Move from here: the app doesn't delete files on saved servers.");
  if (block === "files-only")
    return tr(
      "two_panes_move_files_only",
      undefined,
      "Move takes files only here: the app can't check every file of a copied folder. Copy folders.",
    );
  return isTauriEnv()
    ? tr("two_panes_no_move_computer", undefined, "No Move from here: the app never deletes files on this computer.")
    : tr("two_panes_no_move_app_computer", undefined, "No Move from here: the app never deletes files on its computer.");
}

/** The PS5 listing (children) and the other side, with Copy and Move between them. */
export function TwoPanes({
  ps,
  servers,
  hidden,
  children,
}: {
  ps: PsSide;
  servers: Connection[];
  /** A view (search, space, snapshots) covers the listing: the other side steps aside too. */
  hidden: boolean;
  children: ReactNode;
}) {
  const tr = useTr();
  const { confirm, dialog: confirmNode } = useConfirm();
  const profiles = useRosterStore((s) => s.profiles);
  const here = hostOf(ps.host);
  const consoles = useMemo(() => profiles.filter((p) => hostOf(p.host) !== here), [profiles, here]);
  const nameOfConsole = useCallback(
    (h: string) => profiles.find((p) => hostOf(p.host) === hostOf(h))?.name || hostOf(h),
    [profiles],
  );

  const pane = useOtherPane(here, servers);
  const { place, dir } = pane;
  const placeName = !place
    ? ""
    : place.kind === "computer"
      ? computerLabel(tr)
      : place.kind === "server"
        ? (servers.find((s) => s.id === place.id)?.name ?? place.id)
        : nameOfConsole(place.host);
  const psName = nameOfConsole(here);

  const [starting, setStarting] = useState(false);
  const sourceOf = (toward: Toward): PaneItem[] =>
    toward === "other"
      ? ps.selected
      : pane.entries
          .filter((e) => pane.selected.has(e.name))
          .map((e) => ({ name: e.name, path: e.path, isDir: e.is_dir, size: e.size }));

  const run = async (toward: Toward, op: PaneOp) => {
    if (!place || dir === null) return;
    const items = sourceOf(toward);
    const route = paneRoute(place, toward, op, items.some((i) => i.isDir));
    if (!route.ok) return;
    if (items.length === 0) return;
    const fromDir = ps.path;
    const toDir = toward === "other" ? dir : ps.path;
    setStarting(true);
    try {
      // Names already in the destination are named before anything runs. Into the PS5 from
      // this computer or a server, Add files asks about each itself.
      if (route.via !== "upload") {
        const destNames = toward === "other" ? pane.entries.map((e) => e.name) : ps.names;
        const clash = namesAlreadyThere(items.map((i) => i.name), destNames);
        if (clash.length > 0) {
          const ok = await confirm({
            title: tr("two_panes_replace_title", undefined, "Replace what is already there?"),
            message: tr(
              "two_panes_replace_body",
              undefined,
              "These are already in the folder they go to, and are replaced:",
            ),
            details: clash,
            confirmLabel: tr("two_panes_replace_ok", undefined, "Replace"),
            destructive: true,
          });
          if (!ok) return;
        }
      }
      if (op === "move") {
        const srcHost = toward === "other" ? here : place.kind === "console" ? place.host : null;
        const kept = srcHost
          ? await krakenKeptPaths(srcHost, items.map((i) => ({ path: i.path, isDir: i.isDir }))).catch(() => [])
          : [];
        const ok = await confirm({
          title: tr("two_panes_move_ask", undefined, "Move the ticked items?"),
          message:
            tr(
              "two_panes_move_body",
              undefined,
              "Each item is copied first. The original is deleted only after its copy is there in full.",
            ) +
            (kept.length > 0
              ? `\n\n${tr(
                  "fs_delete_kraken_warning",
                  { n: kept.length },
                  "Kraken Mount uses {n} of these as games: deleting them removes those games from the PS5.",
                )}`
              : ""),
          details: items.map((i) => i.name),
          confirmLabel: tr("two_panes_move_ok", undefined, "Move"),
        });
        if (!ok) return;
      }
    } finally {
      setStarting(false);
    }

    if (route.via === "upload") {
      ps.clearSelection();
      pane.clearSelection();
      await ps.upload(
        items.map((i) => placeTransferPath(place, i.path)),
        items.map((i) => ({ src: placeTransferPath(place, i.path), kind: i.isDir ? "folder" : "file" })),
      );
      return;
    }

    // Each copy, how it ends, and how it is checked.
    let copy: (item: PaneItem) => Promise<CopyOutcome>;
    const unsettled = new Set<string>();
    let destPlace: Place;
    if (route.via === "console-send") {
      const otherHost = (place as { kind: "console"; host: string }).host;
      const from = toward === "other" ? here : otherHost;
      const to = toward === "other" ? otherHost : here;
      destPlace = { kind: "console", host: to };
      const ids = queueConsoleSend(
        from,
        to,
        items.map((i) => ({ path: i.path, name: i.name, size: i.isDir ? 0 : i.size, isDir: i.isDir })),
        items.map((i) => join(toDir, i.name)),
      );
      pushNotification(
        "info",
        tr("fs_send_queued", { n: items.length, name: nameOfConsole(to) }, `${items.length} queued for ${nameOfConsole(to)}`),
      );
      const ends = new Map(items.map((it, i) => [it, whenQueueRowEnds(ids[i])]));
      copy = async (item) => {
        const end = await ends.get(item)!;
        if (end.status !== "done") return "failed";
        // The console has not confirmed saving every file: the source stays, to send again.
        if (!mayDeleteSourceAfterUpload({ deleteSourceAfterUpload: true }, end.row.live)) unsettled.add(item.path);
        return "done";
      };
    } else {
      destPlace = place;
      const server = route.via === "download-server" ? servers.find((s) => s.id === (place as { id: string }).id) : undefined;
      // A move to a saved server is the engine's own (#953): it checks every file's size and
      // every folder on the server, and only then deletes the source. A job that ends done has
      // done all of that; one that fails keeps the source.
      const into = { dir: placeTransferPath(place, dir), from: fromDir, move: !!route.engineMoves };
      copy = (item) => ps.download(item, into, server);
    }
    if (toward === "other") ps.clearSelection();
    else pane.clearSelection();

    const srcAddr = consoleAddr(toward === "other" ? here : (place as { host: string }).host);
    const result = await runPaneTransfer(items, route.engineMoves ? "copy" : op, {
      copy,
      verify: async (item) => {
        if (unsettled.has(item.path)) return false;
        const found = await findInPlace(destPlace, toDir, item.name);
        return copyLooksWhole(item, found);
      },
      removeSource: (item) => fsDelete(srcAddr, item.path),
    });

    // What landed shows on both sides: the copy where it went, and a move's gap where it was.
    if (toward === "other" || op === "move") void pane.reload();
    if (toward === "ps5" || op === "move") ps.refresh();
    if (op !== "move") return;
    const names = (list: PaneItem[]) => list.map((i) => i.name).join(", ");
    const srcHostForNote = toward === "other" ? here : (place as { host: string }).host;
    if (result.unverified.length > 0)
      pushNotification(
        "warning",
        withConsolePrefix(
          srcHostForNote,
          tr("two_panes_unverified", undefined, "Copied, but the original was kept: the copy could not be checked"),
        ),
        { body: names(result.unverified) },
      );
    if (result.notRemoved.length > 0)
      pushNotification(
        "warning",
        withConsolePrefix(
          srcHostForNote,
          tr("two_panes_not_removed", undefined, "Copied, but the original could not be deleted"),
        ),
        { body: names(result.notRemoved) },
      );
    if (result.finished.length > 0)
      pushNotification(
        "success",
        tr("two_panes_moved", { count: result.finished.length, place: toward === "other" ? placeName : psName }, "Moved {count} items to {place}"),
      );
  };

  // Tab from a row on one side goes to the list on the other (and Shift+Tab back).
  const rootRef = useRef<HTMLDivElement | null>(null);
  const onKeyDown = (ev: React.KeyboardEvent) => {
    if (ev.key !== "Tab" || ev.altKey || ev.ctrlKey || ev.metaKey || hidden) return;
    const t = ev.target as HTMLElement;
    const inLeft = !!t.closest("[data-fs-name]") && !!t.closest("[data-pane='ps5']");
    const inRight = !!t.closest("[data-other-row]");
    if (!inLeft && !inRight) return;
    const root = rootRef.current;
    if (!root) return;
    const target = inLeft
      ? (root.querySelector<HTMLElement>("[data-other-row][data-ticked]") ??
        root.querySelector<HTMLElement>("[data-other-row]") ??
        root.querySelector<HTMLElement>("[data-other-list]"))
      : (root.querySelector<HTMLElement>("[data-pane='ps5'] [data-fs-name] input:checked")?.closest<HTMLElement>("[data-fs-name]") ??
        root.querySelector<HTMLElement>("[data-pane='ps5'] [data-fs-name]"));
    if (!target) return;
    ev.preventDefault();
    target.focus();
  };

  const actions: { toward: Toward; op: PaneOp }[] = [
    { toward: "other", op: "copy" },
    { toward: "other", op: "move" },
    { toward: "ps5", op: "copy" },
    { toward: "ps5", op: "move" },
  ];

  return (
    <div ref={rootRef} className="flex items-start gap-3" data-two-panes="" onKeyDown={onKeyDown}>
      {confirmNode}
      {/* The PS5 side keeps the larger share: its rows carry their own action buttons. */}
      {/* Narrower than the window, the selection bar's actions wrap onto more lines, each with
          its whole label, instead of squeezing over each other. */}
      <section
        data-pane="ps5"
        aria-label={psName}
        className="min-w-0 flex-[3_1_0%] [&_[data-testid='fs-selection-actions']]:flex-wrap [&_[data-testid='fs-selection-actions']>button]:shrink-0 [&_[data-testid='fs-selection-actions']>button]:whitespace-nowrap"
      >
        {children}
      </section>
      {!hidden && (
        <>
          <div
            role="toolbar"
            aria-orientation="vertical"
            aria-label={tr("two_panes_actions", undefined, "Copy or move between the two sides")}
            // As wide as its longest button label, in any language and font.
            className="sticky top-2 flex w-max min-w-24 shrink-0 flex-col gap-1.5 pt-10"
            data-testid="two-panes-actions"
          >
            {actions.map(({ toward, op }) => {
              // A ticked folder can make a Move "files only": the reason shows while it is ticked.
              const route = place ? paneRoute(place, toward, op, sourceOf(toward).some((i) => i.isDir)) : null;
              const count = toward === "other" ? ps.selected.length : pane.selected.size;
              const usesDownload = route?.ok && (route.via === "download" || route.via === "download-server");
              const disabled =
                !route?.ok || count === 0 || dir === null || starting || (usesDownload && ps.downloadBusy);
              const dest = toward === "other" ? placeName : psName;
              const label = op === "copy" ? tr("two_panes_copy", undefined, "Copy") : tr("two_panes_move", undefined, "Move");
              const reasonId = `two-panes-why-${toward}-${op}`;
              return (
                <div key={`${toward}-${op}`} className={toward === "ps5" && op === "copy" ? "mt-3" : ""}>
                  <Button
                    variant={op === "copy" ? "secondary" : "ghost"}
                    size="sm"
                    className="w-full justify-start whitespace-nowrap"
                    disabled={disabled}
                    onClick={() => void run(toward, op)}
                    aria-label={
                      op === "copy"
                        ? tr("two_panes_copy_to", { place: dest }, "Copy the ticked items to {place}")
                        : tr("two_panes_move_to", { place: dest }, "Move the ticked items to {place}")
                    }
                    aria-describedby={route && !route.ok ? reasonId : undefined}
                    title={route && !route.ok ? blockText(tr, route.block) : undefined}
                    data-testid={`two-panes-${op}-${toward}`}
                    leftIcon={
                      <ArrowRight
                        size={14}
                        className={toward === "other" ? "rtl:rotate-180" : "rotate-180 rtl:rotate-0"}
                      />
                    }
                  >
                    {label}
                  </Button>
                  {route && !route.ok && (
                    // w-0 min-w-full: the note wraps to the buttons' width instead of widening them.
                    <p id={reasonId} className="mt-0.5 w-0 min-w-full text-[11px] leading-snug text-[var(--color-muted)]">
                      {blockText(tr, route.block)}
                    </p>
                  )}
                </div>
              );
            })}
          </div>
          <OtherPane pane={pane} servers={servers} consoles={consoles} />
        </>
      )}
    </div>
  );
}

/** The two panes when `on`; otherwise the PS5 listing alone, exactly as it is without them. */
export function TwoPanesFrame({ on, ...rest }: { on: boolean } & Parameters<typeof TwoPanes>[0]) {
  return on ? <TwoPanes {...rest} /> : <>{rest.children}</>;
}

function computerLabel(tr: ReturnType<typeof useTr>): string {
  return isTauriEnv()
    ? tr("two_panes_this_computer", undefined, "This computer")
    : tr("two_panes_app_computer", undefined, "The app's computer");
}

type OtherPaneState = ReturnType<typeof useOtherPane>;

/** The other side's place, folder, listing and ticks; remembered per device. */
function useOtherPane(here: string, servers: Connection[]) {
  const [place, setPlace] = useState<Place | null>(() => {
    const m = readOtherPane();
    return m?.place ?? { kind: "computer" };
  });
  const [dir, setDir] = useState<string | null>(() => readOtherPane()?.path ?? null);
  const [roots, setRoots] = useState<string[]>([]);
  const [entries, setEntries] = useState<PlaceEntry[]>([]);
  const [more, setMore] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const seq = useRef(0);

  // The console Files is on is never the other side too.
  const usable = place && !(place.kind === "console" && hostOf(place.host) === here) ? place : null;

  const load = useCallback(
    async (p: Place, path: string, after?: string | null) => {
      const mine = ++seq.current;
      setLoading(true);
      setError(null);
      try {
        const page = await listPlace(p, path, after);
        if (mine !== seq.current) return;
        setEntries((prev) => (after ? [...prev, ...page.entries] : page.entries));
        setMore(page.more);
      } catch (e) {
        if (mine !== seq.current) return;
        setEntries([]);
        setMore(null);
        setError(humanizeCaught(e));
      } finally {
        if (mine === seq.current) setLoading(false);
      }
    },
    [],
  );

  // A new place starts at its usual first folder unless one is remembered for it.
  const startOf = useCallback(
    async (p: Place): Promise<{ start: string; roots: string[] }> => {
      if (p.kind === "console") return { start: CONSOLE_ROOTS[0], roots: CONSOLE_ROOTS };
      if (p.kind === "server") {
        const s = servers.find((c) => c.id === p.id)?.start_path || "/";
        return { start: s.startsWith("/") ? s : `/${s}`, roots: [] };
      }
      const rs = await localFs.storageRoots().catch(() => [] as string[]);
      return { start: rs[0] ?? "/", roots: rs };
    },
    [servers],
  );

  const placeKey = usable ? JSON.stringify(usable) : "";
  useEffect(() => {
    if (!usable) return;
    let live = true;
    void (async () => {
      const { start, roots: rs } = await startOf(usable);
      if (!live) return;
      setRoots(rs);
      const remembered = readOtherPane();
      const path =
        remembered && JSON.stringify(remembered.place) === placeKey && remembered.path ? remembered.path : start;
      setDir(path);
      setSelected(new Set());
      void load(usable, path);
    })();
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [placeKey]);

  const open = (path: string) => {
    if (!usable) return;
    setDir(path);
    setSelected(new Set());
    writeOtherPane({ place: usable, path });
    void load(usable, path);
  };

  return {
    place: usable,
    dir: usable ? dir : null,
    roots,
    entries,
    more,
    loading,
    error,
    selected,
    choose: (p: Place) => {
      writeOtherPane({ place: p, path: "" });
      setEntries([]);
      setError(null);
      setPlace(p);
    },
    open,
    up: () => {
      if (!usable || dir === null) return;
      const parent = placeParent(usable, dir);
      if (parent) open(parent);
    },
    reload: async () => {
      if (usable && dir !== null) await load(usable, dir);
    },
    loadMore: () => {
      if (usable && dir !== null && more) void load(usable, dir, more);
    },
    toggle: (name: string) =>
      setSelected((cur) => {
        const next = new Set(cur);
        if (next.has(name)) next.delete(name);
        else next.add(name);
        return next;
      }),
    setAll: (on: boolean) => setSelected(on ? new Set(entries.map((e) => e.name)) : new Set()),
    clearSelection: () => setSelected(new Set()),
  };
}

const placeValue = (p: Place | null) =>
  !p ? "" : p.kind === "computer" ? "computer" : p.kind === "server" ? `server:${p.id}` : `console:${p.host}`;

function OtherPane({
  pane,
  servers,
  consoles,
}: {
  pane: OtherPaneState;
  servers: Connection[];
  consoles: { name: string; host: string }[];
}) {
  const tr = useTr();
  const { place, dir, entries, selected } = pane;
  const parent = place && dir !== null ? placeParent(place, dir) : null;
  const listRef = useRef<HTMLUListElement | null>(null);
  const focusRow = (name: string) =>
    listRef.current?.querySelector<HTMLElement>(`[data-other-row="${CSS.escape(name)}"]`)?.focus();
  const allTicked = entries.length > 0 && entries.every((e) => selected.has(e.name));

  return (
    <section
      aria-label={tr("two_panes_other_side", undefined, "Other side")}
      // Its keys are its own: the PS5 list's shortcuts (Enter, Backspace, Ctrl+A) stay over there.
      data-keys-own=""
      data-testid="two-panes-other"
      className="sticky top-2 flex max-h-[calc(100dvh-6rem)] min-w-0 flex-[2_1_0%] flex-col rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)]"
    >
      <div className="flex flex-wrap items-center gap-2 border-b border-[var(--color-border)] p-2">
        <select
          className="input input-sm min-w-0 max-w-full flex-1 text-xs"
          aria-label={tr("two_panes_place", undefined, "Place on the other side")}
          data-testid="two-panes-place"
          value={placeValue(place)}
          onChange={(e) => {
            const v = e.target.value;
            if (v === "computer") pane.choose({ kind: "computer" });
            else if (v.startsWith("server:")) pane.choose({ kind: "server", id: v.slice(7) });
            else if (v.startsWith("console:")) pane.choose({ kind: "console", host: v.slice(8) });
          }}
        >
          {!place && (
            <option value="" disabled>
              {tr("two_panes_choose_place", undefined, "Choose a place")}
            </option>
          )}
          <option value="computer">{computerLabel(tr)}</option>
          {servers.length > 0 && (
            <optgroup label={tr("two_panes_servers", undefined, "Saved servers")}>
              {servers.map((s) => (
                <option key={s.id} value={`server:${s.id}`}>
                  {s.name}
                </option>
              ))}
            </optgroup>
          )}
          {consoles.length > 0 && (
            <optgroup label={tr("two_panes_consoles", undefined, "Other consoles")}>
              {consoles.map((c) => (
                <option key={c.host} value={`console:${hostOf(c.host)}`}>
                  {c.name || hostOf(c.host)}
                </option>
              ))}
            </optgroup>
          )}
        </select>
        {pane.roots.length > 1 && (
          <select
            className="input input-sm max-w-[40%] text-xs"
            aria-label={tr("picker_roots", undefined, "Storage")}
            value={pane.roots.includes(dir ?? "") ? (dir ?? "") : ""}
            onChange={(e) => e.target.value && pane.open(e.target.value)}
          >
            <option value="" disabled>
              {tr("picker_roots", undefined, "Storage")}
            </option>
            {pane.roots.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        )}
      </div>
      <div className="flex items-center gap-1.5 border-b border-[var(--color-border)] px-2 py-1.5">
        <input
          type="checkbox"
          className="ms-1 h-3.5 w-3.5 shrink-0"
          checked={allTicked}
          disabled={entries.length === 0}
          onChange={(e) => pane.setAll(e.target.checked)}
          aria-label={tr("fs_select_all", undefined, "Select all")}
          title={tr("fs_select_all", undefined, "Select all")}
        />
        <button
          type="button"
          disabled={!parent}
          onClick={pane.up}
          aria-label={tr("picker_up", undefined, "Up one level")}
          title={tr("picker_up", undefined, "Up one level")}
          className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-float)] disabled:opacity-30"
        >
          <ChevronUp size={16} />
        </button>
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-[var(--color-muted)]" dir="ltr" title={dir ?? ""}>
          <bdi data-testid="two-panes-path">{dir ?? ""}</bdi>
        </span>
        {selected.size > 0 && (
          <span className="shrink-0 text-xs text-[var(--color-muted)]" data-testid="two-panes-count">
            {tr("fs_selected_count", { count: selected.size }, "{count} selected")}
          </span>
        )}
        <button
          type="button"
          disabled={!place || pane.loading}
          onClick={() => void pane.reload()}
          aria-label={tr("fs_refresh", undefined, "Refresh")}
          title={tr("fs_refresh", undefined, "Refresh")}
          className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-float)] disabled:opacity-30"
        >
          <RefreshCw size={14} className={pane.loading ? "animate-spin" : ""} />
        </button>
      </div>
      <div className="min-h-[12rem] flex-1 overflow-y-auto">
        {pane.loading && entries.length === 0 ? (
          <p className="px-4 py-8 text-center text-xs text-[var(--color-muted)]">
            {tr("picker_loading", undefined, "Loading…")}
          </p>
        ) : pane.error ? (
          <div className="flex flex-col items-center gap-2 px-4 py-8 text-center text-xs" role="alert">
            <span className="text-[var(--color-bad)]">{pane.error}</span>
            <Button variant="ghost" size="sm" onClick={() => void pane.reload()} leftIcon={<RefreshCw size={14} />}>
              {tr("picker_retry", undefined, "Retry")}
            </Button>
          </div>
        ) : entries.length === 0 ? (
          <p className="flex flex-col items-center gap-2 px-4 py-8 text-center text-xs text-[var(--color-muted)]">
            <HardDrive size={20} />
            {place
              ? tr("picker_empty", undefined, "This folder is empty.")
              : tr("two_panes_choose_place", undefined, "Choose a place")}
          </p>
        ) : (
          <ul
            ref={listRef}
            data-other-list=""
            tabIndex={-1}
            className="grid gap-1 p-1.5"
            onKeyDown={(ev) => {
              const row = (ev.target as HTMLElement).closest<HTMLElement>("[data-other-row]");
              if (!row || ev.altKey || ev.ctrlKey || ev.metaKey) return;
              const at = entries.findIndex((x) => x.name === row.dataset.otherRow);
              if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
                const next = entries[at + (ev.key === "ArrowDown" ? 1 : -1)];
                if (!next) return;
                ev.preventDefault();
                focusRow(next.name);
              } else if (ev.key === "Backspace" && parent && ev.target === row) {
                ev.preventDefault();
                pane.up();
              }
            }}
          >
            {entries.map((e) => {
              const ticked = selected.has(e.name);
              return (
                <li
                  key={e.path}
                  data-other-row={e.name}
                  data-ticked={ticked ? "" : undefined}
                  tabIndex={-1}
                  onKeyDown={(ev) => {
                    if (ev.target !== ev.currentTarget) return;
                    if (ev.key === "Enter" && e.is_dir) {
                      ev.preventDefault();
                      pane.open(e.path);
                    } else if (ev.key === " ") {
                      ev.preventDefault();
                      pane.toggle(e.name);
                    }
                  }}
                  onDoubleClick={(ev) => {
                    if ((ev.target as HTMLElement).closest("input,button")) return;
                    if (e.is_dir) pane.open(e.path);
                  }}
                  className={
                    "flex items-center gap-2 rounded-[var(--radius-card)] border px-2.5 py-1 text-sm focus-visible:outline-2 focus-visible:outline-[var(--color-accent)] " +
                    (ticked
                      ? "border-[color-mix(in_srgb,var(--color-accent)_40%,transparent)] bg-[var(--color-accent-soft)]"
                      : "border-[var(--glass-edge)] bg-[var(--color-surface-raised)] hover:bg-[var(--color-float)]")
                  }
                >
                  <input
                    type="checkbox"
                    checked={ticked}
                    onChange={() => pane.toggle(e.name)}
                    aria-label={e.name}
                    className="h-3.5 w-3.5 shrink-0"
                  />
                  {e.is_dir ? (
                    <Folder size={16} className="shrink-0 text-[var(--color-accent)]" />
                  ) : (
                    <FileIcon size={16} className="shrink-0 text-[var(--color-muted)]" />
                  )}
                  <button
                    type="button"
                    disabled={!e.is_dir}
                    onClick={() => e.is_dir && pane.open(e.path)}
                    title={e.name}
                    className={
                      "min-w-0 flex-1 truncate text-start text-sm " +
                      (e.is_dir ? "font-medium hover:text-[var(--color-accent)]" : "disabled:cursor-default")
                    }
                  >
                    <bdi>{e.name}</bdi>
                  </button>
                  {!e.is_dir && (
                    <span className="shrink-0 text-xs text-[var(--color-muted)]" dir="ltr">
                      {formatBytes(e.size)}
                    </span>
                  )}
                </li>
              );
            })}
            {pane.more && (
              <li>
                <button
                  type="button"
                  disabled={pane.loading}
                  className="w-full px-4 py-2 text-center text-xs text-[var(--color-accent)] hover:bg-[var(--color-float)]"
                  onClick={pane.loadMore}
                >
                  {pane.loading ? tr("picker_loading", undefined, "Loading…") : tr("picker_load_more", undefined, "Load more")}
                </button>
              </li>
            )}
          </ul>
        )}
      </div>
    </section>
  );
}

