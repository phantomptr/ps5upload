// The install links this engine is serving, shared by the Collection's buttons and its
// "Serving n" indicator.

import { create } from "zustand";

import { pkgLinks, type SharedLink } from "../api/pkgLinks";
import { mgmtAddr } from "../lib/addr";

interface PkgLinksState {
  links: SharedLink[];
  set: (p: Partial<PkgLinksState>) => void;
}

export const usePkgLinksStore = create<PkgLinksState>((set) => ({
  links: [],
  set: (p) => set(p),
}));

export async function loadPkgLinks(): Promise<void> {
  try {
    // Only a list counts: anything else (an older engine, an error body) means none, never a
    // crash of the screens that show them.
    const list: unknown = await pkgLinks.list();
    usePkgLinksStore.getState().set({ links: Array.isArray(list) ? (list as SharedLink[]) : [] });
  } catch {
    /* the indicator keeps what it last knew */
  }
}

/** Hosts the package for `host` and returns the link the console fetches. */
export async function createPkgLink(host: string, path: string) {
  const link = await pkgLinks.create(mgmtAddr(host), path);
  await loadPkgLinks();
  return link;
}

export async function stopPkgLinks(ids: string[]): Promise<void> {
  await Promise.allSettled(ids.map((id) => pkgLinks.stop(id)));
  await loadPkgLinks();
}

/** Lets any device on the network fetch the link, or only its console again. */
export async function setPkgLinkOpen(id: string, any: boolean): Promise<void> {
  await pkgLinks.open(id, any);
  await loadPkgLinks();
}
