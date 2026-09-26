import {
  profileAccentForHost,
  profileNameForAddr,
  useRosterStore,
} from "../state/roster";

/**
 * Small "which console" chip: a colored dot + the console's roster name.
 *
 * The one canonical way activity rows, the footer ActivityBar, and the
 * Dashboard attribute an operation to a console. Renders nothing when:
 *   - the addr is empty / unknown (nothing useful to show), or
 *   - the roster has fewer than two consoles (a single-PS5 user already
 *     knows which console everything targets — the chip would be noise).
 *
 * The dot color comes from the roster-position accent palette
 * (`profileAccentForHost`), the same color the console's tab uses, so
 * the user can match a running row to a tab without reading either name.
 */
export function ConsoleChip({
  addr,
  className = "",
}: {
  addr: string | undefined | null;
  className?: string;
}) {
  const profiles = useRosterStore((s) => s.profiles);
  if (!addr || profiles.length < 2) return null;
  const name = profileNameForAddr(addr, profiles);
  if (!name) return null;
  const accent = profileAccentForHost(addr, profiles);
  return (
    <span
      className={`inline-flex max-w-[12rem] items-center gap-1 rounded-full bg-[var(--color-surface-3)] px-1.5 py-0.5 text-xs font-medium text-[var(--color-muted)] ${className}`}
      title={name}
    >
      <span
        aria-hidden
        className="h-1.5 w-1.5 shrink-0 rounded-full"
        style={{ background: accent ?? "var(--color-muted)" }}
      />
      <span className="truncate">{name}</span>
    </span>
  );
}
