// A computer's phone controller (or watch) link, and its ps5upload:// form. No imports: the
// engine's small /pad page (vite.pad.config.ts) uses it, and lib/appLinks reads it in the app.

/** A computer's phone controller link (`http://<computer>:<port>/pad#t=<key>[&w=1]`). */
export interface PadLink {
  /** `<host>:<port>` as written (an IPv6 host in brackets). */
  authority: string;
  /** The host alone, to name the computer. */
  host: string;
  key: string;
  watch: boolean;
}

/** The controller link, or null when `text` is not one. */
export function parsePadLink(text: string): PadLink | null {
  let u: URL;
  try {
    u = new URL(text.trim());
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  if (!/^\/pad\/?$/.test(u.pathname) || u.username || u.password) return null;
  const hash = new URLSearchParams(u.hash.replace(/^#/, ""));
  const key = hash.get("t") ?? "";
  if (!/^[0-9a-fA-F]{8,128}$/.test(key)) return null;
  return { authority: u.host, host: u.hostname.replace(/^\[|\]$/g, ""), key, watch: hash.get("w") === "1" };
}

/** "Open in the PS5Upload app" for a controller (or watch) link. */
export function padAppLink(link: string): string {
  return `ps5upload://pad?link=${encodeURIComponent(link)}`;
}

