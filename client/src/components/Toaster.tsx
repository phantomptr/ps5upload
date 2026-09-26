import { useEffect } from "react";
import { AlertTriangle, CheckCircle, Info, X, XCircle } from "lucide-react";

import { useToastStore, type Toast, type ToastTone } from "../state/toasts";
import { useTr } from "../state/lang";
import { useAccessibilityStore } from "../state/accessibility";

const TONE_CONFIG: Record<
  ToastTone,
  { color: string; Icon: typeof Info; role: "status" | "alert" }
> = {
  info: { color: "var(--color-accent)", Icon: Info, role: "status" },
  success: { color: "var(--color-good)", Icon: CheckCircle, role: "status" },
  warn: { color: "var(--color-warn)", Icon: AlertTriangle, role: "alert" },
  critical: { color: "var(--color-bad)", Icon: XCircle, role: "alert" },
};

function ToastItem({ toast }: { toast: Toast }) {
  const tr = useTr();
  const dismiss = useToastStore((s) => s.dismiss);
  const motion = useAccessibilityStore((s) => s.resolvedMotion)();
  const { color, Icon, role } = TONE_CONFIG[toast.tone];

  // Auto-dismiss after duration (unless sticky / critical).
  useEffect(() => {
    if (toast.duration === null) return;
    const t = setTimeout(() => dismiss(toast.id), toast.duration);
    return () => clearTimeout(t);
  }, [toast.id, toast.duration, dismiss]);

  const animCls =
    motion === "none"
      ? ""
      : "animate-in fade-in slide-in-from-bottom-3 duration-200";

  return (
    <div
      role={role}
      aria-live={role === "alert" ? "assertive" : "polite"}
      className={[
        "pointer-events-auto flex items-start gap-2.5 rounded-lg border bg-[var(--color-surface-2)] px-3 py-2.5 shadow-lg",
        animCls,
      ].join(" ")}
      style={{ borderColor: color }}
    >
      <Icon
        size={16}
        aria-hidden="true"
        className="mt-0.5 shrink-0"
        style={{ color }}
      />
      <div className="min-w-0 flex-1">
        <p className="text-sm text-[var(--color-text)]">{toast.message}</p>
        {toast.action && (
          <button
            type="button"
            onClick={() => {
              toast.action?.onClick();
              dismiss(toast.id);
            }}
            className="mt-1 text-xs font-medium underline-offset-2 hover:underline"
            style={{ color }}
          >
            {toast.action.label}
          </button>
        )}
      </div>
      <button
        type="button"
        onClick={() => dismiss(toast.id)}
        aria-label={tr("dismiss", "Dismiss")}
        className="shrink-0 rounded p-0.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  );
}

/**
 * Toast container. Renders once at the app root (inside AppShell).
 *
 *   - Desktop: bottom-right stack
 *   - Mobile: top stack (below status bar), full-width
 *
 * Each toast self-dismisses after its duration (critical = sticky).
 * Animations respect the accessibility motion setting.
 */
export function Toaster() {
  const toasts = useToastStore((s) => s.toasts);
  return (
    <div
      aria-live="polite"
      className={[
        "pointer-events-none fixed z-[70] flex flex-col gap-2",
        // Desktop: bottom-right; Mobile (pointer coarse): top, full-width
        "bottom-4 right-4 max-w-sm",
        "max-[767px]:bottom-auto max-[767px]:right-0 max-[767px]:left-0 max-[767px]:top-[calc(44px+env(safe-area-inset-top)+0.5rem)] max-[767px]:max-w-none max-[767px]:px-2",
      ].join(" ")}
    >
      {toasts.map((t) => (
        <ToastItem key={t.id} toast={t} />
      ))}
    </div>
  );
}
