// The Files bars of the phone layout (#936): the top row of the pinned header, and the bar of
// selection actions that takes the bottom navigation's place while selecting. Tablets and
// computers never show either (`roomy:hidden`); they keep the page header and the selection bar.

import type { ComponentType, ReactNode } from "react";
import { CheckCheck, FolderOpen, MoreVertical, Search, X } from "lucide-react";

import { Spinner } from "../../components";

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
