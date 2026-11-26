// The Files bars of the phone layout (#936): the top row of the pinned header, and the bars that
// take the bottom navigation's place: the selection's actions while selecting, and Paste here
// while something is staged to copy or move. Tablets and computers never show them
// (`roomy:hidden`); they keep the page header, the selection bar and the banner over the list.

import { useLayoutEffect, type ComponentType, type ReactNode } from "react";
import { CheckCheck, ClipboardPaste, FolderOpen, MoreVertical, Search, X } from "lucide-react";

import { Spinner } from "../../components";
import "./filesColumn.css";

type IconType = ComponentType<{ size?: number; className?: string }>;

const ICON_BUTTON =
  "grid h-10 w-10 shrink-0 place-items-center rounded-full text-[var(--color-text)] hover:bg-[var(--color-surface-3)] disabled:opacity-40 aria-expanded:bg-[var(--color-surface-3)] aria-pressed:text-[var(--color-accent-bright)]";

/** A round icon button whose name is its label (read out, and the tooltip). */
function IconButton({
  icon: Icon,
  label,
  onClick,
  menu,
  pressed,
  disabled,
  testId,
}: {
  icon: IconType;
  label: string;
  onClick: (box: DOMRect) => void;
  /** Opens a menu: whether it is open. */
  menu?: boolean;
  pressed?: boolean;
  disabled?: boolean;
  testId?: string;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-haspopup={menu === undefined ? undefined : "menu"}
      aria-expanded={menu}
      aria-pressed={pressed}
      disabled={disabled}
      data-testid={testId}
      // An open menu closes on any mousedown outside it: without this the tap that should close
      // it would close it and the click open it again.
      onMouseDown={menu === undefined ? undefined : (ev) => ev.stopPropagation()}
      onClick={(ev) => onClick(ev.currentTarget.getBoundingClientRect())}
      className={ICON_BUTTON}
    >
      <Icon size={18} />
    </button>
  );
}

/** The top row of the pinned header: the open folder, search, the drive and ⋮. */
export function PhoneFolderRow({
  title,
  loading,
  searchLabel,
  onSearch,
  driveIcon,
  driveLabel,
  drivesOpen,
  onDrives,
  moreLabel,
  moreOpen,
  onMore,
  bell,
  disabled,
}: {
  title: string;
  loading: boolean;
  searchLabel: string;
  onSearch: () => void;
  /** Null: no drive to switch to (none listed yet). */
  driveIcon: IconType | null;
  driveLabel: string;
  drivesOpen: boolean;
  onDrives: (box: DOMRect) => void;
  moreLabel: string;
  moreOpen: boolean;
  onMore: (box: DOMRect) => void;
  /** The notification bell, when this header stands in for the app's top bar. */
  bell?: ReactNode;
  /** No console to act on. */
  disabled: boolean;
}) {
  return (
    <div
      // Held sideways, the path sits beside this row instead of under it (the header's height is
      // what a short screen has least of).
      className="flex min-h-11 items-center gap-0.5 roomy:hidden landscape:max-w-[55%] landscape:shrink-0"
      data-testid="fs-phone-row"
    >
      <FolderOpen size={18} className="ms-1.5 me-1 shrink-0 text-[var(--color-accent-bright)]" aria-hidden />
      <h1 className="min-w-0 flex-1 truncate text-base font-semibold" data-testid="fs-phone-title">
        {title}
      </h1>
      {loading && <Spinner size={14} />}
      <IconButton icon={Search} label={searchLabel} onClick={onSearch} disabled={disabled} />
      {driveIcon && (
        <IconButton
          icon={driveIcon}
          label={driveLabel}
          onClick={onDrives}
          menu={drivesOpen}
          testId="fs-phone-drives"
        />
      )}
      {bell}
      <IconButton
        icon={MoreVertical}
        label={moreLabel}
        onClick={onMore}
        menu={moreOpen}
        testId="fs-phone-more"
      />
    </div>
  );
}

/** The same row in selection mode: cancel, how many, and Select all. */
export function PhoneSelectionRow({
  countLabel,
  cancelLabel,
  onCancel,
  selectAllLabel,
  allSelected,
  onSelectAll,
}: {
  countLabel: string;
  cancelLabel: string;
  onCancel: () => void;
  selectAllLabel: string;
  allSelected: boolean;
  onSelectAll: () => void;
}) {
  return (
    <div
      className="flex min-h-11 items-center gap-0.5 roomy:hidden landscape:max-w-[55%] landscape:shrink-0"
      data-testid="fs-phone-selection"
    >
      <IconButton icon={X} label={cancelLabel} onClick={onCancel} />
      <span
        className="min-w-0 flex-1 truncate ps-1 text-base font-semibold tabular-nums"
        role="status"
        data-testid="fs-phone-count"
      >
        {countLabel}
      </span>
      <IconButton
        icon={CheckCheck}
        label={selectAllLabel}
        onClick={onSelectAll}
        pressed={allSelected}
      />
    </div>
  );
}

export interface PhoneBarAction {
  id: string;
  icon: IconType;
  label: string;
  run: (box: DOMRect) => void;
  disabled?: boolean;
  tone?: "bad";
  /** Opens a menu: whether it is open. */
  menu?: boolean;
}

/** While `on`, Files is a column on a phone (`html[data-files-column]`, filesColumn.css): the page no
 *  longer scrolls, the header stays where it is, and the list under it is the only scroller.
 *  The header sat in the page's scroller as a sticky header before, and Android's overscroll
 *  stretch moved and stretched it with the list. Rendered under the connection gate, so a gate's
 *  message (no console, no helper) keeps the page's own scrolling. A layout effect: in place
 *  before the windowed list looks for its scroller. */
export function FilesColumn({ on }: { on: boolean }) {
  useLayoutEffect(() => {
    if (!on) return;
    const root = document.documentElement;
    root.setAttribute("data-files-column", "");
    return () => root.removeAttribute("data-files-column");
  }, [on]);
  return null;
}

/** What is staged to copy or move, with Paste here and Cancel, where the bottom navigation is
 *  (it hides meanwhile, as for the selection's actions). Replaces the banner over the list that
 *  the wider layouts keep (#936). */
export function PhoneStagedBar({
  label,
  countLabel,
  fromLabel,
  pasteLabel,
  onPaste,
  pasteDisabled,
  cancelLabel,
  onCancel,
}: {
  /** The toolbar's name, read out. */
  label: string;
  /** "Items to move: 3". */
  countLabel: string;
  /** Where they come from, or null. */
  fromLabel: string | null;
  pasteLabel: string;
  onPaste: () => void;
  pasteDisabled: boolean;
  cancelLabel: string;
  onCancel: () => void;
}) {
  const button =
    "flex w-[4.75rem] shrink-0 flex-col items-center justify-center gap-0.5 rounded-full text-[0.625rem] font-medium leading-[1.15] hover:bg-[var(--color-surface-3)] disabled:opacity-40 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)]";
  return (
    <div
      role="toolbar"
      aria-label={label}
      data-testid="fs-staged-bar"
      className="glass-float elev-2 fixed inset-x-3 bottom-[calc(var(--safe-bottom)_+_0.6rem)] z-40 flex min-h-16 items-stretch gap-1 rounded-[2rem] p-1.5 roomy:hidden"
    >
      <button type="button" onClick={onCancel} className={button}>
        <X size={20} aria-hidden />
        <span className="line-clamp-2 max-w-full px-0.5 text-center break-words hyphens-auto">{cancelLabel}</span>
      </button>
      <div className="flex min-w-0 flex-1 flex-col justify-center px-1" role="status" data-testid="fs-staged-count">
        {/* Whole, on two lines if it must (a wide font, a long language): the count is the point. */}
        <span className="line-clamp-2 text-sm leading-tight font-semibold tabular-nums break-words" data-testid="fs-staged-what">
          {countLabel}
        </span>
        {fromLabel && (
          <span className="truncate text-[0.6875rem] text-[var(--color-muted)]" dir="auto">
            {fromLabel}
          </span>
        )}
      </div>
      <button
        type="button"
        onClick={onPaste}
        disabled={pasteDisabled}
        className={`${button} text-[var(--color-accent-bright)]`}
      >
        <ClipboardPaste size={20} aria-hidden />
        <span className="line-clamp-2 max-w-full px-0.5 text-center break-words hyphens-auto">{pasteLabel}</span>
      </button>
    </div>
  );
}

/** The actions for the selected items, where the bottom navigation is (it hides meanwhile:
 *  `html[data-selection-bar]` in index.css). */
export function PhoneSelectionActions({
  label,
  actions,
}: {
  /** The toolbar's name, read out. */
  label: string;
  actions: PhoneBarAction[];
}) {
  return (
    <div
      role="toolbar"
      aria-label={label}
      data-testid="fs-selection-actions-bar"
      // Where the bottom navigation floats (TabNav), the same shape.
      className="glass-float elev-2 fixed inset-x-3 bottom-[calc(var(--safe-bottom)_+_0.6rem)] z-40 flex min-h-16 items-stretch justify-around gap-0.5 rounded-[2rem] p-1.5 roomy:hidden"
    >
      {actions.map((a) => (
        <button
          key={a.id}
          type="button"
          disabled={a.disabled}
          aria-haspopup={a.menu === undefined ? undefined : "menu"}
          aria-expanded={a.menu}
          onMouseDown={a.menu === undefined ? undefined : (ev) => ev.stopPropagation()}
          onClick={(ev) => a.run(ev.currentTarget.getBoundingClientRect())}
          className={
            "flex min-w-0 flex-1 flex-col items-center justify-center gap-0.5 rounded-full text-[0.6875rem] font-medium hover:bg-[var(--color-surface-3)] disabled:opacity-40 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-accent)] " +
            (a.tone === "bad" ? "text-[var(--color-bad)]" : "")
          }
        >
          <a.icon size={20} aria-hidden />
          <span className="line-clamp-3 max-w-full px-0.5 text-center text-[0.625rem] leading-[1.15] tracking-tight break-words hyphens-auto">{a.label}</span>
        </button>
      ))}
    </div>
  );
}
