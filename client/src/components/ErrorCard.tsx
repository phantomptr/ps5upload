import { AlertTriangle } from "lucide-react";

/**
 * Red-tinted error card. Used when a screen-level operation failed
 * (fetch, action, etc). Not for per-field validation errors — those
 * should sit next to the field that caused them.
 *
 * Visual weight is intentionally high: red soft-tint background, red
 * border, red icon. Users who just took an action and got an error
 * should see it immediately, not have to hunt for a muted-tone alert.
 */
export function ErrorCard({
  title,
  detail,
  action,
}: {
  title: string;
  detail?: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex items-start gap-2.5 rounded-lg border border-[var(--color-bad)] bg-[var(--color-bad-soft)] p-3 text-sm">
      <AlertTriangle
        size={14}
        className="mt-0.5 shrink-0 text-[var(--color-bad)]"
      />
      <div className="min-w-0 flex-1">
        <div className="font-medium text-[var(--color-bad)]">{title}</div>
        {detail && (
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {detail}
          </div>
        )}
        {action && <div className="mt-2">{action}</div>}
      </div>
    </div>
  );
}

/**
 * Good / success variant — green-tinted. Same shape as ErrorCard,
 * different color vocabulary. Used for "payload sent", "fan threshold
 * applied", etc.
 */
export function SuccessCard({
  title,
  detail,
  action,
}: {
  title: string;
  detail?: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex items-start gap-2.5 rounded-lg border border-[var(--color-good)] bg-[var(--color-good-soft)] p-3 text-sm">
      <svg
        width={14}
        height={14}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={2.5}
        strokeLinecap="round"
        strokeLinejoin="round"
        className="mt-0.5 shrink-0 text-[var(--color-good)]"
        aria-hidden
      >
        <polyline points="20 6 9 17 4 12" />
      </svg>
      <div className="min-w-0 flex-1">
        <div className="font-medium text-[var(--color-good)]">{title}</div>
        {detail && (
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {detail}
          </div>
        )}
        {action && <div className="mt-2">{action}</div>}
      </div>
    </div>
  );
}

/** Yellow/amber warning variant. Softer than Error — "heads up"
 *  rather than "this failed." */
export function WarningCard({
  title,
  detail,
  action,
}: {
  title: string;
  detail?: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex items-start gap-2.5 rounded-lg border border-[var(--color-warn)] bg-[var(--color-warn-soft)] p-3 text-sm">
      <AlertTriangle
        size={14}
        className="mt-0.5 shrink-0 text-[var(--color-warn)]"
      />
      <div className="min-w-0 flex-1">
        <div className="font-medium text-[var(--color-warn)]">{title}</div>
        {detail && (
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {detail}
          </div>
        )}
        {action && <div className="mt-2">{action}</div>}
      </div>
    </div>
  );
}
