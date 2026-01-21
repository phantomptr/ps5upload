// Global type declarations for Electron API bridge

export interface ElectronDialogOpenOptions {
  title?: string;
  defaultPath?: string;
  filters?: { name: string; extensions: string[] }[];
  properties?: ('openFile' | 'openDirectory' | 'multiSelections' | 'showHiddenFiles')[];
}

export interface ElectronDialogSaveOptions {
  title?: string;
  defaultPath?: string;
  filters?: { name: string; extensions: string[] }[];
}

export interface ElectronAPI {
  // Dialog
  dialogOpen: (options: ElectronDialogOpenOptions) => Promise<string[] | null>;
  dialogSave: (options: ElectronDialogSaveOptions) => Promise<string | null>;

  // Window
  windowMinimize: () => Promise<void>;
  windowMaximize: () => Promise<void>;
  windowClose: () => Promise<void>;

  // Events
  on: (channel: string, callback: (...args: unknown[]) => void) => () => void;
  once: (channel: string, callback: (...args: unknown[]) => void) => void;
  removeAllListeners: (channel: string) => void;
}

export interface TauriInternals {
  invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
}

export interface TauriEvent {
  listen: <T>(event: string, callback: (event: { payload: T }) => void) => Promise<() => void>;
  once: <T>(event: string, callback: (event: { payload: T }) => void) => Promise<void>;
  emit: (event: string, payload?: unknown) => void;
}

declare global {
  interface Window {
    electronAPI?: ElectronAPI;
    __TAURI_INTERNALS__?: TauriInternals;
    __TAURI_EVENT__?: TauriEvent;
  }
}

export {};
