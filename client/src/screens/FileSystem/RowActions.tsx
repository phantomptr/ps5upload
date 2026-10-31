import {
  type ComponentType,
  type KeyboardEvent as ReactKeyboardEvent,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import {
  Archive,
  BadgeCheck,
  Download,
  Eye,
  FileArchive,
  Hash,
  Info,
  MoreVertical,
  PackageOpen,
  PackagePlus,
  Pencil,
  PieChart,
  Rocket,
  ScanSearch,
  Trash2,
  Upload,
} from "lucide-react";

import { Spinner } from "../../components";
import { menuKeyAction } from "../../lib/menuKeys";
import { isInstallPackagePath } from "../../lib/pkgDropDedupe";
import { isExtractableArchive } from "../../lib/archiveName";
import type { Translator } from "../../state/lang";

type IconType = ComponentType<{ size?: number; className?: string }>;

/** One action of a Files row. The desktop layout draws it as an icon button;
 *  below `sm` (a phone) the same list sits behind the row's ⋮ menu, in the
 *  same order, so both run the very same handler. */
export interface RowAction {
  id: string;
  icon: IconType;
  /** Short name: the menu item's text and the button's accessible name. */
  label: string;
  /** Tooltip on the desktop button; the label when unset. */
  title?: string;
  run: () => void;
  disabled?: boolean;
  /** Working on it: the desktop button shows a spinner in place of its icon. */
  busy?: boolean;
  tone?: "accent" | "bad";
  /** Only in the menus (the phone's ⋮; right-click on a desktop), not a row button. */
  menuOnly?: boolean;
}

export interface RowMenuItem {
  icon: IconType;
  label: string;
  run: () => void;
  disabled?: boolean;
  destructive?: boolean;
}

export function rowActionMenuItems(actions: RowAction[]): RowMenuItem[] {
  return actions.map((a) => ({
    icon: a.icon,
    label: a.label,
    run: a.run,
    disabled: a.disabled || a.busy,
    destructive: a.tone === "bad",
  }));
}

const BUTTON =
  "rounded-full border shadow-[var(--edge-highlight),var(--shadow-1)] hover:bg-[var(--color-float)] border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-1 disabled:opacity-30";
const TONE: Record<NonNullable<RowAction["tone"]>, string> = {
  accent:
    "border-[color-mix(in_srgb,var(--color-accent)_40%,transparent)] text-[var(--color-accent)]",
  bad: "border-[color-mix(in_srgb,var(--color-bad)_40%,transparent)] text-[var(--color-bad)]",
};

/** A Files listing entry, as far as its actions care. */
export interface RowEntry {
  name: string;
  kind: string;
  size: number;
}

/** What a row's actions need from the Files screen: its state, and the
 *  handlers the buttons run. */
export interface RowActionsContext {
  tr: Translator;
  /** The desktop app (downloads to this computer exist only there). */
  desktopApp: boolean;
  hasHost: boolean;
  downloadActive: boolean;
  /** The entry a download from this folder is saving now. */
  downloadingName: string | null;
  /** Another entry operation is running (replace waits for it). */
  entryBusy: boolean;
  pkgInstalling: boolean;
  installingPkgName: string | null;
  viewable: (name: string, isDir: boolean) => boolean;
  download: (e: RowEntry, asZip?: boolean) => void;
  rename: (e: RowEntry) => void;
  replace: (e: RowEntry) => void;
  properties?: (e: RowEntry) => void;
  view: (e: RowEntry) => void;
  preview: (e: RowEntry) => void;
  crc32: (e: RowEntry) => void;
  verify: (e: RowEntry) => void;
  install: (e: RowEntry) => void;
  /** Send a .elf/.bin on the console to its payload loader (absent: not offered). */
  loadPayload?: (e: RowEntry) => void;
  /** The entry a payload load is sending now. */
  loadingPayloadName?: string | null;
  remove: (e: RowEntry) => void;
  /** Extract an archive on the console (absent: not offered). */
  extract?: (e: RowEntry) => void;
  /** An extract is running (one at a time). */
  extractBusy?: boolean;
  /** Snapshot the entry on the console (absent: not offered). */
  snapshot?: (e: RowEntry) => void;
  /** Open "What takes space?" on a folder (absent: not offered). */
  space?: (e: RowEntry) => void;
}

/** A file offered to the console's payload loader (:9021): .elf or .bin. */
export function isConsolePayloadName(name: string): boolean {
  return /\.(elf|bin)$/i.test(name);
}

/** A row's actions, in the order the desktop row draws them as buttons and
 *  the phone layout lists them behind ⋮. One list, so both run the same
 *  handlers. */
export function buildRowActions(e: RowEntry, ctx: RowActionsContext): RowAction[] {
  const { tr } = ctx;
  const isDir = e.kind === "dir";
  const actions: RowAction[] = [];
  if (ctx.desktopApp) {
    actions.push(
      {
        id: "download",
        icon: Download,
        label: tr("fs_menu_download", undefined, "Download"),
        title: tr(
          "fs_download_tooltip",
          undefined,
          "Save a copy of this entry to a folder on this computer",
        ),
        run: () => ctx.download(e),
        disabled: ctx.downloadActive,
        busy: ctx.downloadingName === e.name,
      },
      {
        id: "download-zip",
        icon: FileArchive,
        label: tr(
          "fs_download_zip_tooltip",
          undefined,
          "Download to this computer as a .zip (streamed — no temp copy)",
        ),
        run: () => ctx.download(e, true),
        disabled: ctx.downloadActive,
      },
    );
  }
  // An archive's main action, so it leads the row rather than hiding in the menu.
  if (!isDir && ctx.extract && ctx.hasHost && isExtractableArchive(e.name))
    actions.push({
      id: "extract",
      icon: PackageOpen,
      label: tr("fs_extract_action", undefined, "Extract…"),
      title: tr("fs_extract_row_tooltip", undefined, "Extract on the console, into a folder next to it"),
      run: () => ctx.extract?.(e),
      disabled: ctx.extractBusy,
      tone: "accent",
    });
  actions.push({
    id: "rename",
    icon: Pencil,
    label: tr("fs_rename", undefined, "Rename"),
    run: () => ctx.rename(e),
  });
  if (ctx.properties) actions.push({
    id: "properties",
    icon: Info,
    label: tr("fs_properties", undefined, "Properties & permissions"),
    run: () => ctx.properties?.(e),
    disabled: !ctx.hasHost,
  });
  if (!isDir)
    actions.push({
      id: "replace",
      icon: Upload,
      label: tr("fs_replace", undefined, "Replace file"),
      title: tr(
        "fs_replace_tooltip",
        undefined,
        "Overwrite this file with one from your computer — how you patch a file inside a mounted game image",
      ),
      run: () => ctx.replace(e),
      disabled: ctx.entryBusy,
    });
  if (ctx.viewable(e.name, isDir) && ctx.hasHost)
    actions.push({
      id: "view",
      icon: ScanSearch,
      label: tr("viewer_open", undefined, "View details"),
      run: () => ctx.view(e),
    });
  if (!isDir && e.size <= 256 * 1024)
    actions.push({
      id: "preview",
      icon: Eye,
      label: tr("fs_preview", undefined, "Preview"),
      title: tr(
        "fs_preview_tooltip",
        undefined,
        "Preview small file inline (text or image)",
      ),
      run: () => ctx.preview(e),
    });
  // crc32_file_get / fs_blake3_hash are desktop-only commands (no engine
  // route): the browser build has nothing to run them with.
  if (!isDir && ctx.desktopApp)
    actions.push(
      {
        id: "crc32",
        icon: Hash,
        label: tr("fs_crc32", undefined, "CRC32 checksum"),
        title: tr(
          "fs_crc32_tooltip",
          undefined,
          "Compute CRC32 of this file (cheap integrity check)",
        ),
        run: () => ctx.crc32(e),
      },
      {
        id: "verify",
        icon: BadgeCheck,
        label: tr("fs_verify", undefined, "Verify (BLAKE3)"),
        title: tr(
          "fs_verify_tooltip",
          undefined,
          "BLAKE3 + CRC32 verification (slower, crypto-strength)",
        ),
        run: () => ctx.verify(e),
      },
    );
  if (!isDir && isInstallPackagePath(e.name))
    actions.push({
      id: "install",
      icon: PackagePlus,
      label: tr("fs_install_action", undefined, "Install"),
      title: tr(
        "fs_install_pkg_tooltip",
        undefined,
        "Install this package on your PS5",
      ),
      run: () => ctx.install(e),
      disabled: ctx.pkgInstalling,
      busy: ctx.installingPkgName === e.name,
      tone: "accent",
    });
  if (!isDir && ctx.loadPayload && isConsolePayloadName(e.name))
    actions.push({
      id: "load-payload",
      icon: Rocket,
      label: tr("fs_load_payload", undefined, "Load payload"),
      title: tr(
        "fs_load_payload_tooltip",
        undefined,
        "Send this payload to the PS5's payload loader (port 9021)",
      ),
      run: () => ctx.loadPayload?.(e),
      disabled: ctx.pkgInstalling || !ctx.hasHost,
      busy: ctx.loadingPayloadName === e.name,
      tone: "accent",
    });
  if (isDir && ctx.space && ctx.hasHost)
    actions.push({
      id: "space",
      icon: PieChart,
      label: tr("fs_space_action", undefined, "What takes space?"),
      run: () => ctx.space?.(e),
      menuOnly: true,
    });
  if (ctx.snapshot && ctx.hasHost)
    actions.push({
      id: "snapshot",
      icon: Archive,
      label: tr("fs_snapshot_action", undefined, "Snapshot…"),
      title: tr("fs_snapshot_tooltip", undefined, "Copy into a snapshot kept on the PS5, to put back later"),
      run: () => ctx.snapshot?.(e),
      menuOnly: true,
    });
  actions.push({
    id: "delete",
    icon: Trash2,
    label: tr("library_delete", undefined, "Delete"),
    run: () => ctx.remove(e),
    tone: "bad",
  });
  return actions;
}

/** A row's desktop action buttons (hidden below `sm`). */
export function RowActionButtons({
  entry,
  ctx,
}: {
  entry: RowEntry;
  ctx: RowActionsContext;
}) {
  return <RowActionButtonList actions={buildRowActions(entry, ctx)} />;
}

/** A row's ⋮ menu (the phone layout), against the ⋮ button's box. */
export function RowActionsMenu({
  entry,
  ctx,
  anchor,
  onClose,
}: {
  entry: RowEntry;
  ctx: RowActionsContext;
  anchor: { left: number; right: number; top: number; bottom: number };
  onClose: () => void;
}) {
  return (
    <RowMenu
      x={anchor.right}
      y={anchor.bottom}
      anchor={anchor}
      onClose={onClose}
      items={rowActionMenuItems(buildRowActions(entry, ctx))}
    />
  );
}

/** The row's actions as the desktop icon buttons. Hidden below `sm`, where
 *  `RowActionsTrigger` stands in for them. */
export function RowActionButtonList({ actions }: { actions: RowAction[] }) {
  return (
    <div
      data-testid="fs-row-actions"
      className="ml-2 hidden shrink-0 items-center gap-1 sm:flex"
    >
      {actions.filter((a) => !a.menuOnly).map((a) => (
        <button
          key={a.id}
          type="button"
          onClick={a.run}
          disabled={a.disabled}
          aria-label={a.label}
          title={a.title ?? a.label}
          className={`${BUTTON} ${a.tone ? TONE[a.tone] : ""}`}
        >
          {a.busy ? (
            <Spinner size={12} tone={a.tone === "accent" ? "inherit" : "accent"} />
          ) : (
            <a.icon size={12} />
          )}
        </button>
      ))}
    </div>
  );
}

/** The ⋮ that opens a row's actions on a phone (below `sm` only). */
export function RowActionsTrigger({
  label,
  open,
  onToggle,
}: {
  label: string;
  open: boolean;
  /** Called with the trigger's box, for the menu to sit against. */
  onToggle: (anchor: DOMRect) => void;
}) {
  return (
    <button
      type="button"
      data-testid="fs-row-more"
      aria-haspopup="menu"
      aria-expanded={open}
      aria-label={label}
      title={label}
      // The open menu closes on any mousedown outside it. Without this the
      // tap that should close it would close it and the click reopen it.
      onMouseDown={(ev) => ev.stopPropagation()}
      onClick={(ev) => onToggle(ev.currentTarget.getBoundingClientRect())}
      className="-me-1 shrink-0 rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] sm:hidden"
    >
      <MoreVertical size={16} />
    </button>
  );
}

/** Where a menu opened from a button sits: under the button, its end edge on
 *  the button's end edge; above the button when there is no room below. The
 *  viewport's `insetTop` / `insetBottom` are the phone's status and navigation
 *  bars: a menu item under the status bar can't be tapped (Android takes the
 *  touch), so the menu stays between them. */
export function menuPlacement(
  anchor: { left: number; right: number; top: number; bottom: number },
  menu: { width: number; height: number },
  viewport: { width: number; height: number; insetTop?: number; insetBottom?: number },
  margin = 4,
): { left: number; top: number } {
  const left = Math.max(
    margin,
    Math.min(anchor.right - menu.width, viewport.width - menu.width - margin),
  );
  const minTop = (viewport.insetTop ?? 0) + margin;
  const maxBottom = viewport.height - (viewport.insetBottom ?? 0) - margin;
  const fitsBelow = anchor.bottom + menu.height <= maxBottom;
  const fitsAbove = anchor.top - menu.height >= minTop;
  const top = fitsBelow
    ? anchor.bottom
    : fitsAbove
      ? anchor.top - menu.height
      : Math.max(minTop, maxBottom - menu.height);
  return { left, top };
}

/** The status and navigation bar heights (CSS px) the page is drawn under. */
function safeInsets(): { insetTop: number; insetBottom: number } {
  const probe = document.createElement("div");
  probe.style.cssText =
    "position:fixed;visibility:hidden;pointer-events:none;padding-top:env(safe-area-inset-top);padding-bottom:var(--safe-bottom, env(safe-area-inset-bottom))";
  document.body.appendChild(probe);
  const cs = getComputedStyle(probe);
  const insets = {
    insetTop: parseFloat(cs.paddingTop) || 0,
    insetBottom: parseFloat(cs.paddingBottom) || 0,
  };
  probe.remove();
  return insets;
}

/** The Files row menu: the right-click menu (at the pointer) and, on a phone,
 *  the ⋮ menu (against its button, flipped up near the bottom of the screen).
 *  Kept on screen, closed by a click elsewhere, Escape, scroll or a resize. It
 *  takes focus on its first item when it opens; the arrow keys, Home and End
 *  move between items, and Escape or Tab close it and hand focus back to
 *  whatever had it (the row, or the ⋮). */
export function RowMenu({
  x,
  y,
  anchor,
  items,
  onClose,
}: {
  x: number;
  y: number;
  /** Open against this box (the ⋮ button) instead of at x/y. */
  anchor?: { left: number; right: number; top: number; bottom: number };
  items: RowMenuItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const viewport = { width: window.innerWidth, height: window.innerHeight, ...safeInsets() };
    setPos(
      anchor
        ? menuPlacement(anchor, r, viewport)
        : {
            left: Math.max(4, Math.min(x, viewport.width - r.width - 4)),
            top: Math.max(
              viewport.insetTop + 4,
              Math.min(y, viewport.height - viewport.insetBottom - r.height - 4),
            ),
          },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [x, y, anchor?.left, anchor?.top]);
  // Whatever had focus when the menu opened, for a keyboard close to return to.
  const openerRef = useRef<HTMLElement | null>(null);
  const menuItems = () =>
    Array.from(
      ref.current?.querySelectorAll<HTMLElement>(
        '[role="menuitem"]:not([disabled])',
      ) ?? [],
    );
  useEffect(() => {
    const active = document.activeElement;
    openerRef.current = active instanceof HTMLElement ? active : null;
    menuItems()[0]?.focus({ preventScroll: true });
  }, []);
  /** Close, then put focus back where it was (keyboard close or picking an item). */
  const closeAndRestore = () => {
    onClose();
    const opener = openerRef.current;
    if (opener?.isConnected) opener.focus({ preventScroll: true });
  };
  const onMenuKeyDown = (e: ReactKeyboardEvent) => {
    const items = menuItems();
    const action = menuKeyAction(
      e.key,
      items.indexOf(document.activeElement as HTMLElement),
      items.length,
    );
    if (action === null) return;
    e.preventDefault();
    e.stopPropagation();
    if (action === "close") closeAndRestore();
    else items[action]?.focus();
  };
  useEffect(() => {
    const close = (e: Event) => {
      if (e instanceof MouseEvent && ref.current?.contains(e.target as Node))
        return;
      onClose();
    };
    // Scroll events arrive a frame late: a list still settling from a fling
    // when the ⋮ is tapped would close the menu the moment it opened.
    const openedAt = performance.now();
    const onScroll = (e: Event) => {
      if (performance.now() - openedAt > 250) close(e);
    };
    // Escape with focus somewhere else (the menu's own handler stops it otherwise).
    const onKey = (e: KeyboardEvent) =>
      e.key === "Escape" && !e.defaultPrevented && onClose();
    window.addEventListener("mousedown", close);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);
  // On <body>, not in the screen: the shell canvas clips its content and (having
  // a backdrop filter) is what `position: fixed` measures from inside it, so a
  // menu opened low on the page was cut off at the canvas edge and sat offset
  // from the pointer. The rows themselves can't host it either: they use
  // `content-visibility`, which clips anything that pokes out of them.
  return createPortal(
    <div
      ref={ref}
      role="menu"
      data-testid="fs-row-menu"
      onKeyDown={onMenuKeyDown}
      style={{ left: pos.left, top: pos.top }}
      // The same floating glass as the app's other menus (components/Menu).
      // `--color-surface` is the translucent page tint: with it the rows under
      // the menu read straight through the items.
      className="glass-float elev-2 fixed z-50 max-h-[calc(100dvh-0.5rem-env(safe-area-inset-top)-var(--safe-bottom,0px))] min-w-[12rem] max-w-[calc(100vw-0.5rem)] overflow-y-auto rounded-[var(--radius-card)] p-1.5 text-sm"
    >
      <RowMenuList
        items={items}
        // Focus back on the row first, so a dialog the item opens returns
        // focus there when it closes.
        onPick={(it) => {
          closeAndRestore();
          it.run();
        }}
      />
    </div>,
    document.body,
  );
}

/** The items of a RowMenu: one menuitem per entry, in order, the destructive
 *  one in the danger colour. */
export function RowMenuList({
  items,
  onPick,
}: {
  items: RowMenuItem[];
  onPick: (item: RowMenuItem) => void;
}) {
  return items.map((it) => (
    <button
      key={it.label}
      type="button"
      role="menuitem"
      tabIndex={-1}
      disabled={it.disabled}
      onClick={() => onPick(it)}
      className={
        "flex w-full items-center gap-2 rounded-full px-3 py-1.5 text-start hover:bg-[var(--color-surface-3)] focus:bg-[var(--color-surface-3)] disabled:opacity-40 " +
        (it.destructive ? "text-[var(--color-bad)]" : "")
      }
    >
      <it.icon size={14} className="shrink-0" />
      {it.label}
    </button>
  ));
}
