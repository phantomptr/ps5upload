// Electron adapter for Tauri event API
// This provides compatibility with Tauri's listen() function

import './types';

export type UnlistenFn = () => void;

export interface Event<T> {
  payload: T;
}

export async function listen<T>(
  event: string,
  callback: (event: Event<T>) => void
): Promise<UnlistenFn> {
  // Check if running with Tauri-compatible bridge
  if (window.__TAURI_EVENT__) {
    return window.__TAURI_EVENT__.listen<T>(event, callback);
  }

  // Check if running in Electron with electronAPI
  if (window.electronAPI?.on) {
    const unlisten = window.electronAPI.on(event, (...args: unknown[]) => {
      // Electron sends args directly, wrap in payload object
      callback({ payload: args[0] as T });
    });
    return Promise.resolve(unlisten);
  }

  // No listener available
  console.warn(`Event listener not available for: ${event}`);
  return Promise.resolve(() => {});
}

export async function once<T>(
  event: string,
  callback: (event: Event<T>) => void
): Promise<void> {
  if (window.__TAURI_EVENT__) {
    return window.__TAURI_EVENT__.once<T>(event, callback);
  }

  if (window.electronAPI?.once) {
    window.electronAPI.once(event, (...args: unknown[]) => {
      callback({ payload: args[0] as T });
    });
    return Promise.resolve();
  }

  console.warn(`Event once not available for: ${event}`);
  return Promise.resolve();
}

export async function emit(event: string, payload?: unknown): Promise<void> {
  if (window.__TAURI_EVENT__) {
    window.__TAURI_EVENT__.emit(event, payload);
    return;
  }

  console.warn(`Event emit not available for: ${event}`);
}
