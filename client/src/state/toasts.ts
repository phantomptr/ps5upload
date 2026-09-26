import { create } from "zustand";

export type ToastTone = "info" | "success" | "warn" | "critical";

export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: string;
  message: string;
  tone: ToastTone;
  duration: number | null; // null = sticky (critical)
  action?: ToastAction;
}

export interface ToastOptions {
  message: string;
  tone?: ToastTone;
  duration?: number;
  action?: ToastAction;
}

const DEFAULT_DURATION: Record<ToastTone, number | null> = {
  info: 4000,
  success: 3000,
  warn: 6000,
  critical: null, // sticky
};

let counter = 0;
function nextId(): string {
  counter += 1;
  return `toast-${Date.now()}-${counter}`;
}

interface ToastStore {
  toasts: Toast[];
  push: (opts: ToastOptions) => string;
  dismiss: (id: string) => void;
  dismissAll: () => void;
}

export const useToastStore = create<ToastStore>((set) => ({
  toasts: [],
  push: (opts) => {
    const id = nextId();
    const tone = opts.tone ?? "info";
    const toast: Toast = {
      id,
      message: opts.message,
      tone,
      duration: opts.duration ?? DEFAULT_DURATION[tone],
      action: opts.action,
    };
    set((s) => ({ toasts: [...s.toasts, toast] }));
    return id;
  },
  dismiss: (id) =>
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
  dismissAll: () => set({ toasts: [] }),
}));

/**
 * Imperative toast API:
 *
 *   const { toast, dismiss, dismissAll } = useToast();
 *   toast({ message: "Saved", tone: "success" });
 *
 * Tone → duration defaults:
 *   info     4s
 *   success  3s
 *   warn     6s
 *   critical sticky (no auto-dismiss)
 *
 * NOT for errors (those are inline Callout). `critical` is the only
 * error-class tone allowed — reserved for Task System §7.4 alerts
 * (thermal trip, power-off imminent).
 */
export function useToast() {
  const push = useToastStore((s) => s.push);
  const dismiss = useToastStore((s) => s.dismiss);
  const dismissAll = useToastStore((s) => s.dismissAll);
  return { toast: push, dismiss, dismissAll };
}
