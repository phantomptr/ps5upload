import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/** The name a guest goes by, remembered on this device (the app and the /join page). */
export const NAME_KEY = "ps5upload.join.name";

/** The longest name a guest can give. */
export const NAME_MAX = 32;

/** A device's default name, before the friend types one. */
export function defaultName(ua: string): string {
  return /iPhone/.test(ua) ? "iPhone" : /iPad/.test(ua) ? "iPad" : /Android/.test(ua) ? "Android" : "";
}

/** A typed name as it is sent: trimmed, at most NAME_MAX characters ("" for none). */
export function cleanName(name: string): string {
  return name.trim().slice(0, NAME_MAX);
}

/** The name last used to join on this device, else `fallback`. */
export function lastName(fallback = ""): string {
  const saved = cleanName(safeGetItem(NAME_KEY) ?? "");
  return saved || fallback;
}

/** Remember the name a guest joins with (any way in); the clean name, "" for none (not saved). */
export function rememberName(name: string): string {
  const clean = cleanName(name);
  if (clean) safeSetItem(NAME_KEY, clean);
  return clean;
}
