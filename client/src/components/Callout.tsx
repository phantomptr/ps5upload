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
      softColor: "var(--color-accent-soft)",
      Icon: Info,
    },
  }[tone];

  const { Icon } = config;

  return (
    <div
      role={isAlert ? "alert" : "status"}
      aria-live={isAlert ? "assertive" : "polite"}
      className={[
        "flex items-start gap-3 rounded-[var(--radius-card)] border px-4 py-3 text-sm text-[var(--color-text)] shadow-[var(--edge-highlight),var(--shadow-1)]",
        className,
      ].join(" ")}
      // Glass tinted by the tone: the tone's soft wash layered over the
      // raised glass (never a solid fill), with an edge in the tone's colour.
      // Text stays the normal text colour so it reads in both themes; the
      // icon carries the tone.
      style={{
        borderColor: `color-mix(in srgb, ${config.color} 34%, transparent)`,
        backgroundColor: "var(--color-surface-raised)",
        backgroundImage: `linear-gradient(${config.softColor}, ${config.softColor})`,
      }}
    >
      <span
        className="grid h-6 w-6 shrink-0 place-items-center rounded-full"
        style={{ backgroundColor: config.softColor, color: config.color }}
        aria-hidden="true"
      >
        <Icon size={14} />
      </span>
      <div className="min-w-0 flex-1 pt-0.5">
        <div className="break-words font-semibold">{title}</div>
        {children && (
          <div className="mt-1 break-words text-xs leading-relaxed text-[var(--color-muted)]">
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
          className="grid h-7 w-7 shrink-0 place-items-center rounded-full text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
        >
          <X size={14} aria-hidden="true" />
        </button>
      )}
    </div>
  );
}
