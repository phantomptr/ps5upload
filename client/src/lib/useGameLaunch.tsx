import { useCallback, useState, type ReactNode } from "react";

import { appLaunch, fsChmod, processList } from "../api/ps5";
import { useConfirm } from "../components/ConfirmDialog";
import { hostOf, mgmtAddr, transferAddr } from "./addr";
import { humanizePs5Error } from "./humanizeError";
import { askToSignIn, isSignInNeeded } from "./signInNeeded";
import { DOC_ANCHORS, faqLink, installErrorLink } from "./installErrorDoc";
import { killGame } from "./killGame";
import {
  handleHomebrewRefusal,
  isHomebrewRefusal,
  type RefusalOutcome,
} from "./launchRefusal";
import {
  LAST_PS5_FAKE_GAME_FIRMWARE,
  ps5FakeGameUnplayableFirmware,
} from "./ps5Firmware";
import { fetchRunningGames, type RunningGame } from "./runningGames";
import { useStaleHostGuard } from "./staleHostGuard";
import { useMakeWay } from "./useMakeWay";
import { useConnectionStore } from "../state/connection";
import { useTr } from "../state/lang";
import { pushNotification } from "../state/notifications";
import { withConsolePrefix } from "../state/roster";
import { runningAfterClose, useRunningAppsStore } from "../state/runningApps";

/** How long a just-launched title stays "Starting…" while its process is awaited, and how often
 *  the process list is read. A first launch (just installed, cold cache, disc image) is slow, so
 *  this is generous; running out never fails the launch or touches the game. */
const LAUNCH_CONFIRM_TIMEOUT_MS = 90_000;
const LAUNCH_CONFIRM_POLL_MS = 2_000;

/** A 0x80940033 launch refusal the repair in launchRefusal.ts could not clear. */
class HomebrewRefused extends Error {
  constructor(readonly outcome: Exclude<RefusalOutcome, { kind: "fixed" }>) {
    super("launch_sony_error_0x80940033");
  }
}

/** A game to start or close: its title ID, the name to show, and (for a folder or image the
 *  console runs from) its source path, which the homebrew repair needs. */
export interface LaunchTarget {
  titleId: string;
  titleName: string;
  source?: string | null;
}

/**
 * Play and Close game for the connected console, one implementation for every place that
 * offers them (Games, the game page, Installed games):
 *
 * - Play closes a running game first with the user's say-so (useMakeWay), launches, then waits
 *   (read-only) for the title's process before saying "Now playing"; a slow start is reported
 *   as such, never as a failure. A 0x80940033 refusal of a homebrew or fake-package title is
 *   diagnosed and, for a folder sent without execute permission, repaired and retried.
 * - Close game asks first, then closes the title by its app id with a pid fallback (killGame).
 *
 * Both keep the shared running-apps store current, so every badge flips at once. `dialogs`
 * must be rendered by the caller.
 */
export function useGameLaunch(
  opts: {
    /** The name of another title, for "close X first?" (by title ID). */
    nameOf?: (titleId: string) => string;
    /** Called with every fresh read of the running games (Installed keeps its own copy). */
    onRunning?: (running: Map<string, RunningGame>) => void;
  } = {},
): {
  launchingId: string | null;
  stoppingId: string | null;
  launch: (t: LaunchTarget) => Promise<void>;
  stop: (t: LaunchTarget) => Promise<void>;
  dialogs: ReactNode;
} {
  const tr = useTr();
  const guard = useStaleHostGuard();
  const { makeWay, dialog: makeWayDialog } = useMakeWay();
  const { confirm, dialog: confirmDialog } = useConfirm();
  const [launchingId, setLaunchingId] = useState<string | null>(null);
  const [stoppingId, setStoppingId] = useState<string | null>(null);
  const { nameOf, onRunning } = opts;

  // A launch that fails on a console above the last firmware that plays PS5 fake packages says
  // so: a fake-package install is then the likely reason.
  const helpBody = useCallback(
    (body: string, launchHost: string, titleId: string) => {
      const kernel =
        useConnectionStore.getState().runtimeByHost[hostOf(launchHost)]
          ?.ps5Kernel;
      const fw = ps5FakeGameUnplayableFirmware(kernel, titleId);
      if (!fw) return body;
      return `${body} ${tr(
        "fakegame.fw.launch",
        { fw, last: LAST_PS5_FAKE_GAME_FIRMWARE },
        `PS5 fake game packages can't be played on firmware above ${LAST_PS5_FAKE_GAME_FIRMWARE} (this console is on ${fw}). If this game was installed from a fake package, that is why it won't start. PS4 packages are fine.`,
      )}`;
    },
    [tr],
  );

  const launch = useCallback(
    async (t: LaunchTarget) => {
      const probe = guard.capture();
      if (!probe.host?.trim() || launchingId) return;
      setLaunchingId(t.titleId);
      try {
        const clear = await makeWay(probe.host, t.titleId, t.titleName, nameOf);
        if (probe.isStale() || !clear) return;
        try {
          await appLaunch(transferAddr(probe.host), t.titleId);
        } catch (e) {
          const raw = e instanceof Error ? e.message : String(e);
          if (!isHomebrewRefusal(raw) || probe.isStale()) throw e;
          // The console refused a non-Sony title: find out whether kstuff is missing, and
          // repair a folder uploaded without world-execute (see launchRefusal.ts).
          const outcome = await handleHomebrewRefusal(t.source ?? undefined, {
            processes: async () =>
              (await processList(mgmtAddr(probe.host))).processes,
            chmod777: (path) =>
              fsChmod(transferAddr(probe.host), path, "0777", true),
            relaunch: () => appLaunch(transferAddr(probe.host), t.titleId),
          });
          if (outcome.kind !== "fixed") throw new HomebrewRefused(outcome);
        }
        if (probe.isStale()) return;
        // The launch RPC means Sony accepted it, not that the game runs: watch the process
        // list until the title appears.
        const addr = mgmtAddr(probe.host);
        const deadline = Date.now() + LAUNCH_CONFIRM_TIMEOUT_MS;
        let confirmed = false;
        while (Date.now() < deadline) {
          await new Promise((r) => setTimeout(r, LAUNCH_CONFIRM_POLL_MS));
          if (probe.isStale()) return;
          try {
            const r = await fetchRunningGames(addr);
            if (probe.isStale()) return;
            onRunning?.(r);
            useRunningAppsStore
              .getState()
              .setRunning(Array.from(r.keys()), probe.host);
            if (r.has(t.titleId)) {
              confirmed = true;
              break;
            }
          } catch {
            // The console is busy while the game spins up: keep waiting.
          }
        }
        if (confirmed) {
          pushNotification(
            "success",
            withConsolePrefix(probe.host, t.titleName),
            {
              body: tr("installed_now_playing", undefined, "Now playing"),
            },
          );
        } else {
          pushNotification("info", withConsolePrefix(probe.host, t.titleName), {
            body: helpBody(
              tr(
                "installed_launch_slow",
                undefined,
                "Launch sent — first starts can take a while. Give it a moment and check your PS5.",
              ),
              probe.host,
              t.titleId,
            ),
            link: faqLink(DOC_ANCHORS.wontLaunch),
          });
        }
      } catch (e) {
        if (probe.isStale()) return;
        if (e instanceof HomebrewRefused) {
          // Not this game's fault and not the 11.60 fake-package limit: the console is
          // refusing every non-Sony title, so name the cause instead.
          pushNotification(
            "error",
            withConsolePrefix(probe.host, t.titleName),
            {
              body:
                e.outcome.kind === "refused" && e.outcome.shadowmount
                  ? tr(
                      "installed_launch_smp_refused",
                      undefined,
                      "The PS5 refused to start this game (0x80940033) although kstuff is running. ShadowMount+ mounts a game when it starts, and that did not happen. Close any game that is running, send ShadowMount+ again from Payloads, then press Play.",
                    )
                  : e.outcome.kind === "no_kstuff"
                    ? tr(
                        "installed_launch_no_kstuff",
                        undefined,
                        "The PS5 refused to start this game (0x80940033) because kstuff isn't running. Without it the console starts no homebrew or fake-package game. Load kstuff (your autoloader, or Connection → Set up) and press Play again.",
                      )
                    : tr(
                        "installed_launch_homebrew_refused",
                        undefined,
                        "The PS5 refused to start this game (0x80940033). The console isn't starting homebrew or fake-package games right now: reload kstuff (or reboot and run the jailbreak again), then press Play again.",
                      ),
              link: faqLink(DOC_ANCHORS.wontLaunch),
            },
          );
          return;
        }
        const raw = e instanceof Error ? e.message : String(e);
        // Nobody is signed in on the PS5: one message with a Sign in button, not an error.
        if (isSignInNeeded(raw)) {
          askToSignIn(probe.host);
          return;
        }
        pushNotification("error", withConsolePrefix(probe.host, t.titleName), {
          body: helpBody(humanizePs5Error(raw), probe.host, t.titleId),
          link: installErrorLink(raw),
        });
      } finally {
        setLaunchingId(null);
      }
    },
    [guard, launchingId, makeWay, nameOf, onRunning, tr, helpBody],
  );

  const stop = useCallback(
    async (t: LaunchTarget) => {
      const probe = guard.capture();
      if (!probe.host?.trim() || stoppingId) return;
      const ok = await confirm({
        title: tr(
          "installed_stop_confirm_title",
          { name: t.titleName },
          `Close ${t.titleName}?`,
        ),
        message: tr(
          "installed_stop_confirm_body",
          undefined,
          "This closes the running game on the PS5. Any unsaved progress will be lost — the same as quitting from the console.",
        ),
        confirmLabel: tr("installed_stop", undefined, "Close game"),
        destructive: true,
      });
      if (!ok || probe.isStale()) return;
      setStoppingId(t.titleId);
      try {
        const addr = mgmtAddr(probe.host);
        // The store holds title ids only; the app id and pid come from a fresh read. killGame
        // falls back to the pid when Sony's app kill throws (FW 12.20).
        const running = await fetchRunningGames(addr);
        const target = running.get(t.titleId);
        const closed = target ? await killGame(addr, target) : true;
        if (probe.isStale()) return;
        if (closed) {
          running.delete(t.titleId);
          onRunning?.(running);
          const store = useRunningAppsStore.getState();
          store.setRunning(
            runningAfterClose(store, probe.host, t.titleId),
            probe.host,
          );
          pushNotification("info", withConsolePrefix(probe.host, t.titleName), {
            body: tr("installed_stopped", undefined, "Game closed"),
          });
        } else {
          pushNotification(
            "error",
            withConsolePrefix(probe.host, t.titleName),
            {
              body: tr(
                "installed_stop_failed",
                undefined,
                "Couldn't close the game — it may have already exited.",
              ),
            },
          );
        }
      } catch (e) {
        if (probe.isStale()) return;
        pushNotification("error", withConsolePrefix(probe.host, t.titleName), {
          body: e instanceof Error ? e.message : String(e),
        });
      } finally {
        setStoppingId(null);
      }
    },
    [guard, stoppingId, confirm, onRunning, tr],
  );

  return {
    launchingId,
    stoppingId,
    launch,
    stop,
    dialogs: (
      <>
        {makeWayDialog}
        {confirmDialog}
      </>
    ),
  };
}
