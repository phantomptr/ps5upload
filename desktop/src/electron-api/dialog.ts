// Electron adapter for Tauri dialog API
import { invoke } from './core';

export interface OpenDialogOptions {
  title?: string;
  defaultPath?: string;
  directory?: boolean;
  multiple?: boolean;
  filters?: { name: string; extensions: string[] }[];
}

export interface SaveDialogOptions {
  title?: string;
  defaultPath?: string;
  filters?: { name: string; extensions: string[] }[];
}

export async function open(options?: OpenDialogOptions): Promise<string | string[] | null> {
  const result = await invoke<string[]>('dialog_open', {
    title: options?.title,
    defaultPath: options?.defaultPath,
    filters: options?.filters,
    directory: options?.directory,
    multiple: options?.multiple,
  });

  if (!result || result.length === 0) {
    return null;
  }

  return options?.multiple ? result : result[0];
}

export async function save(options?: SaveDialogOptions): Promise<string | null> {
  return invoke<string>('dialog_save', {
    title: options?.title,
    defaultPath: options?.defaultPath,
    filters: options?.filters,
  });
}
