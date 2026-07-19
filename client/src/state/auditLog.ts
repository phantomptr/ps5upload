import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * Persistent audit log for destructive actions.
 *
 * Distinct from `notifications.ts` (info-level event surface) and
 * `activityHistory.ts` (live ops + recent transfers). The audit log
 * answers a different question: "what changed on my PS5(s) lately
 * that I might want to undo or attribute?".
 *
 * Stored as a 256-entry ring buffer in localStorage. Each entry is
 * append-only — we don't expose `clear()` to make it a meaningful
 * safety record.
 */

export type AuditKind =
  | "fs_delete"
  | "fs_chmod"
  | "library_unregister"
  | "library_unmount"
  | "library_register"
  | "library_chmod"
  | "system_reboot"
  | "system_shutdown"
  | "system_standby"
  | "app_kill"
  | "app_suspend"
  | "app_resume"
  | "ufs_fsck_repair"
  | "smp_autotune_write"
  | "fs_write_bytes"
  | "peripheral_eject"
  | "peripheral_bd_off"
  | "pkg_install_start"
  | "pkg_dpi_direct_install"
  | "lwfs_mount"
  | "pkg_direct_mount";

export interface AuditEntry {
  id: string;
  ts: number;
  kind: AuditKind;
  /** Short headline. */
  what: string;
  /** Optional context: PS5 host, file path, title id, etc. */
  context?: string;
  /** True when the underlying action returned an error or non-zero
   *  exit code. UI shades these differently to make accidental
   *  destructive actions stand out. */
  failed?: boolean;
}

const STORAGE_KEY = "ps5upload.audit-log.v1";
const MAX_ENTRIES = 256;

interface AuditState {
  entries: AuditEntry[];
  push: (kind: AuditKind, what: string, extras?: { context?: string; failed?: boolean }) => void;
  /** Convenience: returns entries newest-first for UI rendering. */
}

function loadInitial(): AuditEntry[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = safeGetItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (e): e is AuditEntry =>
        typeof e?.id === "string" &&
        typeof e?.ts === "number" &&
        typeof e?.kind === "string" &&
        typeof e?.what === "string",
    );
  } catch {
    return [];
  }
}

function persist(entries: AuditEntry[]) {
  if (typeof window === "undefined") return;
  try {
    safeSetItem(STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // Quota exceeded — best-effort. Already capped at MAX_ENTRIES.
  }
}

function genId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return "a_" + Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
}

export const useAuditLogStore = create<AuditState>((set, get) => ({
  entries: loadInitial(),
  push: (kind, what, extras) => {
    const entry: AuditEntry = {
      id: genId(),
      ts: Date.now(),
      kind,
      what,
      context: extras?.context,
      failed: extras?.failed,
    };
    const next = [...get().entries, entry].slice(-MAX_ENTRIES);
    set({ entries: next });
    persist(next);
  },
  // No `recent()` method here: it would be a footgun if any future
  // caller invoked it as a zustand selector — every render would
  // produce a new array reference and trigger an infinite re-render
  // loop. Consumers should subscribe to `entries` and derive the
  // reversed view in a `useMemo`.
}));

/** Convenience for fire-and-forget audit pushes from anywhere in the
 *  renderer. Avoids needing each caller to subscribe to the store. */
export function audit(
  kind: AuditKind,
  what: string,
  extras?: { context?: string; failed?: boolean },
): void {
  useAuditLogStore.getState().push(kind, what, extras);
}
