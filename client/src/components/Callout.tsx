import { AlertCircle, AlertTriangle, CheckCircle, Info, X } from "lucide-react";
import { useTr } from "../state/lang";

export type CalloutTone = "error" | "warn" | "success" | "info";

export interface CalloutProps {
  tone: CalloutTone;
  title: string;
  children?: React.ReactNode;
  action?: React.ReactNode;
  onDismiss?: () => void;
  className?: string;
}

/**
 * Inline alert banner. Consolidates ErrorCard/SuccessCard/WarningCard
 * into a single component with four tones:
 *
 *   error   — role="alert", aria-live="assertive" (interrupts SR)
 *   warn    — role="status", aria-live="polite"
 *   success — role="status", aria-live="polite"
 *   info    — role="status", aria-live="polite"
 *
 * ErrorCard/SuccessCard/WarningCard are thin aliases over Callout so
 * existing call sites keep working.
 */
export function Callout({
  tone,
  title,
  children,
  action,
  onDismiss,
  className = "",
}: CalloutProps) {
  const tr = useTr();
  const isAlert = tone === "error";

  const config = {
    error: {
      color: "var(--color-bad)",
      softColor: "var(--color-bad-soft)",
      Icon: AlertCircle,
    },
    warn: {
      color: "var(--color-warn)",
      softColor: "var(--color-warn-soft)",
      Icon: AlertTriangle,
    },
    success: {
      color: "var(--color-good)",
      softColor: "var(--color-good-soft)",
      Icon: CheckCircle,
    },
    info: {
      color: "var(--color-accent)",
      softColor: "var(--color-surface-3)",
      Icon: Info,
    },
  }[tone];

  const { Icon } = config;

  return (
    <div
      role={isAlert ? "alert" : "status"}
      aria-live={isAlert ? "assertive" : "polite"}
      className={[
        "flex items-start gap-2.5 rounded-lg border p-3 text-sm",
        className,
      ].join(" ")}
      style={{
        borderColor: config.color,
        background: config.softColor,
      }}
    >
      <Icon
        size={14}
        className="mt-0.5 shrink-0"
        aria-hidden="true"
        style={{ color: config.color }}
      />
      <div className="min-w-0 flex-1">
        <div className="font-medium" style={{ color: config.color }}>
          {title}
        </div>
        {children && (
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {children}
          </div>
        )}
        {action && <div className="mt-2">{action}</div>}
      </div>
      {onDismiss && (
        <button
          type="button"
          onClick={onDismiss}
          aria-label={tr("dismiss", "Dismiss")}
          className="shrink-0 rounded p-0.5 text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)]"
        >
          <X size={14} aria-hidden="true" />
        </button>
      )}
    </div>
  );
}
