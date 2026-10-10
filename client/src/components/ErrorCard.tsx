import { RefreshCw } from "lucide-react";

import { humanizePs5Error } from "../lib/humanizeError";
import { useTr } from "../state/lang";
import { Button } from "./Button";
import { Callout } from "./Callout";

/**
 * Status cards for a screen-level result. Thin wrappers over Callout so every
 * error, success and warning has the same tinted-glass look and live-region
 * semantics (error: role="alert", assertive; the others: role="status",
 * polite). Not for per-field validation errors — those sit next to the field.
 */
interface StatusCardProps {
  title: string;
  detail?: React.ReactNode;
  action?: React.ReactNode;
  /** When provided, renders a dismiss "×" so the user can clear the card. */
  onDismiss?: () => void;
}

/** A screen-level operation failed (fetch, action, …).
 *
 *  Raw engine/payload strings are passed through `humanizePs5Error` (it
 *  returns anything it doesn't recognise unchanged, so already-friendly
 *  text is safe). `onRetry` adds a Retry button — every error that came
 *  from a read the user can repeat should offer one. */
export function ErrorCard({
  title,
  detail,
  action,
  onDismiss,
  onRetry,
  retrying = false,
}: StatusCardProps & { onRetry?: () => void; retrying?: boolean }) {
  const tr = useTr();
  const retry = onRetry ? (
    <Button
      size="sm"
      leftIcon={<RefreshCw size={12} />}
      loading={retrying}
      onClick={onRetry}
    >
      {tr("retry", undefined, "Retry")}
    </Button>
  ) : null;
  return (
    <Callout
      tone="error"
      title={humanizePs5Error(title)}
      action={
        retry || action ? (
          <div className="flex flex-wrap gap-2">
            {retry}
            {action}
          </div>
        ) : undefined
      }
      onDismiss={onDismiss}
    >
      {typeof detail === "string" ? humanizePs5Error(detail) : detail}
    </Callout>
  );
}

/** "Payload sent", "fan threshold applied", … */
export function SuccessCard({ title, detail, action, onDismiss }: StatusCardProps) {
  return (
    <Callout tone="success" title={title} action={action} onDismiss={onDismiss}>
      {detail}
    </Callout>
  );
}

/** "Heads up" rather than "this failed". */
export function WarningCard({ title, detail, action, onDismiss }: StatusCardProps) {
  return (
    <Callout tone="warn" title={title} action={action} onDismiss={onDismiss}>
      {detail}
    </Callout>
  );
}
