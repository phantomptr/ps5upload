// Which file a "Send <payload>" button sends: the copy saved in the app's payload library, or a
// file the user chose. Shown next to the button (its version, else the file's name) with a
// "Choose file…" to send another; a cancelled picker changes nothing.

import { useCallback, useEffect, useState } from "react";

import { payloadsLocalInventory, payloadsLocalPath, type PayloadLocalEntry } from "../api/ps5";
import { pickPath, type PickPathOptions } from "./pickPath";
import { basename } from "./uploadDest";

export interface PayloadCopy {
  path: string;
  /** The saved copy's version, else the file's name. */
  label: string;
  /** Chosen by the user (not the saved copy). */
  picked: boolean;
}

/** The saved copy at `path`, named by its version from the library inventory. Pure. */
export function savedCopy(
  path: string | null,
  inventory: readonly Pick<PayloadLocalEntry, "path" | "version">[],
): PayloadCopy | null {
  if (!path) return null;
  const version = inventory.find((e) => e.path === path)?.version?.trim();
  return { path, label: version || basename(path), picked: false };
}

/** A chosen file as the copy to send; null for a cancelled picker. Pure. */
export function pickedCopy(path: string | null | undefined): PayloadCopy | null {
  return typeof path === "string" && path ? { path, label: basename(path), picked: true } : null;
}

/** The copy of payload `id` a send uses, and `choose` to pick another file (resolves null and
 *  changes nothing when the picker is cancelled). Uses `pickPath`, so Android browses in-app. */
export function usePayloadCopy(
  id: string,
  pick: Omit<PickPathOptions, "mode">,
): { copy: PayloadCopy | null; choose: () => Promise<PayloadCopy | null> } {
  const [saved, setSaved] = useState<PayloadCopy | null>(null);
  const [chosen, setChosen] = useState<PayloadCopy | null>(null);
  useEffect(() => {
    let live = true;
    void Promise.all([
      payloadsLocalPath(id).catch(() => null),
      payloadsLocalInventory().catch(() => [] as PayloadLocalEntry[]),
    ]).then(([path, inventory]) => {
      if (live) setSaved(savedCopy(path, inventory));
    });
    return () => {
      live = false;
    };
  }, [id]);
  const { title, filters } = pick;
  const choose = useCallback(async () => {
    const c = pickedCopy(await pickPath({ mode: "file", title, filters }));
    if (c) setChosen(c);
    return c;
    // The filters are a constant at each call site.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [title]);
  return { copy: chosen ?? saved, choose };
}
