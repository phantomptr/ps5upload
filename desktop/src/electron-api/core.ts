// Electron adapter for Tauri core API
// This provides compatibility with Tauri's invoke() function

import './types';

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  // Check if running with Tauri-compatible bridge
  if (window.__TAURI_INTERNALS__) {
    return window.__TAURI_INTERNALS__.invoke(cmd, args) as Promise<T>;
  }

  // Fallback - should not happen if preload is properly configured
  throw new Error(`Invoke not available for command: ${cmd}`);
}
