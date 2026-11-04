import { useEffect, useRef } from "react";
import { useLocation, useNavigate } from "react-router";

import { takeHandoffForPc, type HandoffTake } from "../api/consoleFiles";
import { consoleBusy } from "../lib/usePoll";
import { useDocumentVisible } from "../lib/visibility";
import { useConnectionStore } from "../state/connection";
import { openInFileSystem } from "../state/fsNavigation";
import { useTr } from "../state/lang";
import { useToast } from "../state/toasts";

/** How often the console is asked for a "Show on PC" handoff: often on File System, less elsewhere. */
export const HANDOFF_POLL_FILES_MS = 3_000;
export const HANDOFF_POLL_OTHER_MS = 10_000;
/** How long the "Open" offer stays up when a handoff arrives away from File System. */
export const HANDOFF_OFFER_MS = 15_000;

export interface HandoffPoller {
  stop(): void;
}

/**
 * Asks `take` now and then every `delay()` ms; a waiting folder goes to `onPath`. Stops for good
 * when the helper has no handoffs (an older one: `onUnsupported`); a failed ask is retried on the
 * next tick (the console may be busy or restarting).
 */
export function createHandoffPoller(opts: {
  take: () => Promise<HandoffTake>;
  delay: () => number;
  onPath: (path: string) => void;
  onUnsupported?: () => void;
  /** True to skip this tick (a transfer runs: background management calls wait). */
  skip?: () => boolean;
}): HandoffPoller {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const tick = async () => {
    timer = undefined;
    if (stopped) return;
    if (opts.skip?.()) {
      timer = setTimeout(() => void tick(), opts.delay());
      return;
    }
    try {
      const r = await opts.take();
      if (stopped) return;
      if (!r.supported) {
        stopped = true;
        opts.onUnsupported?.();
        return;
      }
      if (r.path) opts.onPath(r.path);
    } catch {
      if (stopped) return;
    }
    timer = setTimeout(() => void tick(), opts.delay());
  };
  timer = setTimeout(() => void tick(), 0);
  return {
    stop() {
      stopped = true;
      if (timer !== undefined) clearTimeout(timer);
    },
  };
}

/**
 * A handoff arrived: on File System it opens at once with a toast; elsewhere the user is never
 * pulled off the screen they are on, a toast offers "Open" instead.
 */
export function deliverHandoff(
  path: string,
  ctx: {
    onFiles: boolean;
    navigate: (to: string) => void;
    tr: ReturnType<typeof useTr>;
    toast: ReturnType<typeof useToast>["toast"];
  },
): void {
  const { navigate: go, tr: t, toast: show } = ctx;
  if (ctx.onFiles) {
    openInFileSystem(go, path);
    show({ tone: "info", message: t("fs_opened_from_ps5", { path }, `Opened from your PS5: ${path}`) });
    return;
  }
  show({
    tone: "info",
    duration: HANDOFF_OFFER_MS,
    message: t("fs_handoff_from_ps5", { path }, `Your PS5 sent ${path}`),
    action: { label: t("fs_handoff_open", undefined, "Open"), onClick: () => openInFileSystem(go, path) },
  });
}

/**
 * "Show on PC": the console's Files page left a folder for this app. While the helper is up and
 * the window is visible and no transfer runs on it, the handoff is taken (every 3 s on File System,
 * every 10 s elsewhere). On File System it opens at once with a toast; elsewhere a toast offers
 * "Open". An older helper is asked once per connection.
 */
export function useConsoleHandoff(): void {
  const host = useConnectionStore((s) => s.host);
  const up = useConnectionStore((s) => s.payloadStatus) === "up";
  const visible = useDocumentVisible();
  const navigate = useNavigate();
  const onFiles = useLocation().pathname.startsWith("/files");
  const tr = useTr();
  const { toast } = useToast();
  // Read by the running poller without restarting it.
  const live = useRef({ onFiles, navigate, tr, toast });
  useEffect(() => {
    live.current = { onFiles, navigate, tr, toast };
  }, [onFiles, navigate, tr, toast]);

  useEffect(() => {
    if (!up || !visible || !host?.trim()) return;
    const p = createHandoffPoller({
      take: () => takeHandoffForPc(host),
      delay: () => (live.current.onFiles ? HANDOFF_POLL_FILES_MS : HANDOFF_POLL_OTHER_MS),
      skip: () => consoleBusy(host),
      onPath: (path) => deliverHandoff(path, live.current),
    });
    return () => p.stop();
  }, [host, up, visible]);
}
