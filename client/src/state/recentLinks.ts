import { create } from "zustand";

import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import type { LinkInstallMode } from "./linkInstallPrefs";

/**
 * Links recently installed from, per console, each with the name the user gave it.
 *
 * A link's own address rarely says which package it is (a token, a hash, a redirect), so
 * with several in play a failed one was a guess to find again. A name is optional; without
 * one the link is labelled by the file its address ends in.
 */
export interface RecentLink {
  url: string;
  /** What the user called it; "" when they did not. */
  name: string;
  mode: LinkInstallMode;
  /** Unix ms of the last time it was started. */
  usedAt: number;
}

export const MAX_RECENT_LINKS = 12;
const KEY = "ps5upload.recent_links.v1";

type ByHost = Record<string, RecentLink[]>;

function load(): ByHost {
  if (typeof window === "undefined") return {};
  try {
    const parsed: unknown = JSON.parse(safeGetItem(KEY) ?? "{}");
    if (!parsed || typeof parsed !== "object") return {};
    const out: ByHost = {};
    for (const [host, list] of Object.entries(
      parsed as Record<string, unknown>,
    )) {
      if (!Array.isArray(list)) continue;
      out[host] = list
        .filter(
          (l): l is RecentLink =>
            !!l &&
            typeof (l as RecentLink).url === "string" &&
            typeof (l as RecentLink).name === "string" &&
            typeof (l as RecentLink).usedAt === "number",
        )
        .map((l): RecentLink => ({
          ...l,
          mode:
            l.mode === "direct" || l.mode === "download" ? l.mode : "stream",
        }))
        .slice(0, MAX_RECENT_LINKS);
    }
    return out;
  } catch {
    return {};
  }
}

function save(byHost: ByHost): void {
  if (typeof window === "undefined") return;
  try {
    safeSetItem(KEY, JSON.stringify(byHost));
  } catch {
    // Storage unavailable: the list still works for this session.
  }
}

interface RecentLinksState {
  byHost: ByHost;
  /** Records a link being started. An empty `name` keeps the name it already had. */
  remember: (
    host: string,
    link: { url: string; name: string; mode: LinkInstallMode },
    now?: number,
  ) => void;
  rename: (host: string, url: string, name: string) => void;
  forget: (host: string, url: string) => void;
}

export const useRecentLinksStore = create<RecentLinksState>((set, get) => {
  const write = (host: string, list: RecentLink[]) => {
    const byHost = { ...get().byHost, [hostOf(host)]: list };
    save(byHost);
    set({ byHost });
  };
  return {
    byHost: load(),
    remember: (host, link, now = Date.now()) => {
      const url = link.url.trim();
      if (!host || !url) return;
      const cur = get().byHost[hostOf(host)] ?? [];
      const prev = cur.find((l) => l.url === url);
      const name = link.name.trim() || prev?.name || "";
      write(
        host,
        [
          { url, name, mode: link.mode, usedAt: now },
          ...cur.filter((l) => l.url !== url),
        ].slice(0, MAX_RECENT_LINKS),
      );
    },
    rename: (host, url, name) =>
      write(
        host,
        (get().byHost[hostOf(host)] ?? []).map((l) =>
          l.url === url ? { ...l, name: name.trim() } : l,
        ),
      ),
    forget: (host, url) =>
      write(
        host,
        (get().byHost[hostOf(host)] ?? []).filter((l) => l.url !== url),
      ),
  };
});

const NONE: RecentLink[] = [];

export function recentLinksFor(
  s: { byHost: ByHost },
  host: string,
): RecentLink[] {
  return s.byHost[hostOf(host)] ?? NONE;
}

/** What to call a link: its name, else the file its address ends in, else its host. */
export function linkLabel(link: Pick<RecentLink, "url" | "name">): string {
  if (link.name.trim()) return link.name.trim();
  try {
    const u = new URL(link.url);
    const last = u.pathname.split("/").filter(Boolean).pop();
    if (last) {
      try {
        return decodeURIComponent(last);
      } catch {
        return last;
      }
    }
    return u.host;
  } catch {
    return link.url;
  }
}
