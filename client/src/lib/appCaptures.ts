import { create } from "zustand";

import { invoke } from "./invokeLogged";
import type { SavedShot } from "./captureScreenshot";

/**
 * The app's own captures: the screenshots the status bar's Capture button
 * saves on this computer (desktop app only). One viewer, mounted once in the
 * shell, shows them large and lets them be deleted; the Bug report gallery and
 * the status bar both open it.
 */

/** Fired on `window` with the deleted names as `detail`, so every list of
 *  captures drops them without a re-read. */
export const CAPTURES_DELETED_EVENT = "ps5upload:captures-deleted";

const NAME_RE = /^screenshot-(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})(?:-\d{3})?\.png$/;

/** When a capture was taken, from its file name (local time); null if the
 *  name is not one the Capture button wrote. */
export function captureDate(name: string): Date | null {
  const m = NAME_RE.exec(name);
  if (!m) return null;
  const [y, mo, d, h, mi, s] = m.slice(1).map(Number);
  return new Date(y, mo - 1, d, h, mi, s);
}

export function withoutNames<T extends { name: string }>(list: T[], names: readonly string[]): T[] {
  const gone = new Set(names);
  return list.filter((s) => !gone.has(s.name));
}

/** The next/previous position, wrapping at both ends. */
export function stepIndex(index: number, delta: number, length: number): number {
  if (length <= 0) return 0;
  return (((index + delta) % length) + length) % length;
}

/** Where the viewer goes once the capture it showed is deleted: the one that
 *  moved into its place, or the new last one. */
export function indexAfterDelete(index: number, remaining: number): number {
  if (remaining <= 0) return 0;
  return Math.min(index, remaining - 1);
}

export async function listCaptures(): Promise<SavedShot[]> {
  return (await invoke<SavedShot[]>("screenshot_list", {})) ?? [];
}

/** Delete captures; returns the names that are gone, and tells every list. */
export async function deleteAppCaptures(names: string[]): Promise<string[]> {
  const gone = (await invoke<string[]>("screenshot_delete", { names })) ?? [];
  if (gone.length && typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent<string[]>(CAPTURES_DELETED_EVENT, { detail: gone }));
  }
  return gone;
}

export async function openCapturesFolder(): Promise<void> {
  await invoke("screenshot_open_dir", {});
}

interface ViewerState {
  open: boolean;
  /** The capture to show first; null = the newest. */
  start: string | null;
  show: (start?: string | null) => void;
  close: () => void;
}

export const useCaptureViewer = create<ViewerState>((set) => ({
  open: false,
  start: null,
  show: (start = null) => set({ open: true, start }),
  close: () => set({ open: false }),
}));
