// Packages picked on Upload & Install stream & install from this computer through the console's
// queue (nothing is copied to the PS5 first). Shared by the Stream & install buttons, which start
// the queue as any add does, and Add to queue, which only adds the rows.

import { inStreamOrder } from "../../lib/streamSelection";

export interface StreamPkg {
  path: string;
  category?: string;
  contentId?: string;
  size?: number | null;
}

type InstallStream = (
  source: string,
  host: string,
  opts: { category?: string; contentId?: string; sizeBytes?: number | null; start?: boolean },
) => Promise<unknown>;

/** Queue each package as a stream install, games first, then updates, then DLC. Each joins the
 *  console's queue as it is called, so they queue in that order; the promises settle only when
 *  each install ends (its outcome stays on its queue row). `start: false` adds without starting. */
export function streamPackages(
  installStream: InstallStream,
  host: string,
  pkgs: StreamPkg[],
  start: boolean,
): Promise<unknown>[] {
  return inStreamOrder(pkgs).map((p) =>
    installStream(p.path, host, {
      category: p.category,
      contentId: p.contentId,
      sizeBytes: p.size ?? null,
      ...(start ? {} : { start: false }),
    }),
  );
}
