// Hush's card on "On your PS5": whether it runs, whether it is on, its mode and the newest
// lines of its log, with an On/Off switch that rewrites only `enabled=` in its config.ini.
// When Play paused Hush for a stream and could not turn it back on (the app closed during the
// stream), the card offers to.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { RefreshCw, RotateCcw } from "lucide-react";

import { fsWriteText, type ProcessInfo } from "../../api/ps5";
import { Button, Callout, Toggle } from "../../components";
import { mgmtAddr } from "../../lib/addr";
import { HUSH_BLOCKER, blockerCtx, pausedOnConsole, readConsoleText } from "../../lib/rpBlockers";
import { usePoll } from "../../lib/usePoll";
import { useDocumentVisible } from "../../lib/visibility";
import { useTr } from "../../state/lang";
import { log } from "../../state/logs";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import {
  HUSH_CONFIG_PATH,
  HUSH_LOCK_PATH,
  HUSH_LOG_PATH,
  hushLockPid,
  hushLogTail,
  hushProcess,
  parseHushConfig,
  setHushEnabled,
  type HushConfig,
} from "./hushConfig";

/** Re-read Hush's files this often while its card is on screen. */
const POLL_MS = 15_000;
/** Lines of log.txt the panel shows. */
const LOG_LINES = 12;

export interface HushView {
  /** config.ini as read, or null when there is none. */
  configText: string | null;
  config: HushConfig | null;
  /** The pid pid.lock names. */
  lockPid: number | null;
  /** The newest lines of log.txt. */
  log: string[];
  /** Play paused it for a stream and has not turned it back on. */
  pausedByUs: boolean;
}

/** Hush's files on `host`, re-read while `active`. `null` until first read. */
export function useHush(host: string, active: boolean) {
  const visible = useDocumentVisible();
  const [view, setView] = useState<HushView | null>(null);
  const gen = useRef(0);
  useEffect(() => {
    gen.current++;
    setView(null);
  }, [host]);
  const refresh = useCallback(async () => {
    if (!host.trim()) return;
    const g = gen.current;
    const addr = mgmtAddr(host.trim());
    const [configText, lock, logText, paused] = await Promise.all([
      readConsoleText(addr, HUSH_CONFIG_PATH),
      readConsoleText(addr, HUSH_LOCK_PATH, 64),
      readConsoleText(addr, HUSH_LOG_PATH, 72 * 1024),
      pausedOnConsole(addr),
    ]);
    if (g !== gen.current) return;
    setView({
      configText,
      config: configText === null ? null : parseHushConfig(configText),
      lockPid: hushLockPid(lock),
      log: logText ? hushLogTail(logText, LOG_LINES) : [],
      pausedByUs: paused.includes("hush"),
    });
  }, [host]);
  usePoll(refresh, POLL_MS, { host, enabled: active && visible && !!host.trim() });
  return { view, refresh };
}

/** The card's line: on or off, and its mode; "Turn Hush back on" when Play left it paused. */
export function HushSummary({
  view,
  running,
  onResume,
  busy,
}: {
  view: HushView | null;
  running: boolean;
  onResume: () => void;
  busy: boolean;
}) {
  const tr = useTr();
  if (!view?.config) return <>{tr("hush_checking", undefined, "Reading Hush's settings…")}</>;
  if (view.pausedByUs && !view.config.enabled) {
    return (
      <span className="inline-flex flex-wrap items-center gap-2">
        {tr("hush_paused_by_play", undefined, "Paused for Remote Play")}
        <Button size="sm" variant="secondary" leftIcon={<RotateCcw size={13} />} loading={busy} onClick={onResume}>
          {tr("hush_turn_back_on", undefined, "Turn Hush back on")}
        </Button>
      </span>
    );
  }
  const state = view.config.enabled
    ? running
      ? tr("hush_on", undefined, "On: gameplay is not being recorded")
      : tr("hush_on_not_running", undefined, "On, but not running")
    : tr("hush_off", undefined, "Off: the console records as usual");
  return (
    <>
      {state}
      {view.config.mode === "check" ? ` · ${tr("hush_mode_check", undefined, "check mode")}` : ""}
    </>
  );
}

/** Switch Hush on (loading it again when it exited) or off. */
export function useHushActions(host: string, refresh: () => Promise<void>) {
  const tr = useTr();
  const [busy, setBusy] = useState(false);
  const run = async (on: boolean, configText: string | null) => {
    setBusy(true);
    try {
      if (on) {
        const outcome = await HUSH_BLOCKER.resume(blockerCtx(host));
        pushNotification(
          outcome === "running" ? "success" : "info",
          withConsolePrefix(
            host,
            outcome === "running"
              ? tr("hush_turned_on", undefined, "Hush is on")
              : tr("hush_turned_on_send", undefined, "Hush is set to on. Send it to the PS5 to start it."),
          ),
        );
      } else {
        const r = await fsWriteText(mgmtAddr(host.trim()), HUSH_CONFIG_PATH, setHushEnabled(configText ?? "", false));
        if (!r.ok) throw new Error(r.err || "write failed");
        pushNotification(
          "success",
          withConsolePrefix(host, tr("hush_turned_off", undefined, "Hush turns off within about 20 s")),
        );
      }
      log.info("payloads", `Hush ${on ? "on" : "off"}`);
    } catch (e) {
      pushNotification("error", withConsolePrefix(host, tr("hush_switch_failed", undefined, "Couldn't change Hush")), {
        body: String(e),
      });
    } finally {
      setBusy(false);
      await refresh();
    }
  };
  return { busy, run };
}

/** Hush's panel (Manage): the switch, its state and its log. */
export function HushPanel({
  host,
  view,
  procs,
  refresh,
}: {
  host: string;
  view: HushView | null;
  procs: readonly ProcessInfo[];
  refresh: () => Promise<void>;
}) {
  const tr = useTr();
  const { busy, run } = useHushActions(host, refresh);
  const proc = useMemo(() => hushProcess(procs, view?.lockPid ?? null), [procs, view?.lockPid]);
  if (!view) return <p className="text-sm text-[var(--color-muted)]">{tr("hush_checking", undefined, "Reading Hush's settings…")}</p>;
  if (!view.config) {
    return (
      <p className="text-sm text-[var(--color-muted)]">
        {tr("hush_no_config", undefined, "No /data/hush/config.ini yet: Hush writes it the first time it runs.")}
      </p>
    );
  }
  const c = view.config;
  return (
    <div className="space-y-3 text-sm">
      {view.pausedByUs && !c.enabled && (
        <Callout
          tone="warn"
          title={tr("hush_left_paused", undefined, "Play paused Hush for a Remote Play stream and didn't turn it back on")}
          action={
            <Button size="sm" variant="primary" leftIcon={<RotateCcw size={14} />} loading={busy} onClick={() => void run(true, view.configText)}>
              {tr("hush_turn_back_on", undefined, "Turn Hush back on")}
            </Button>
          }
        >
          {tr("hush_left_paused_body", undefined, "The app closed during the stream. Until it is on again, the console records gameplay as usual.")}
        </Callout>
      )}
      <Toggle
        checked={c.enabled}
        disabled={busy}
        onChange={(on) => void run(on, view.configText)}
        label={tr("hush_switch", undefined, "Hush on")}
        hint={tr(
          "hush_switch_hint",
          undefined,
          "Off gives the console its recording back within about 20 s, and Hush exits. On loads it again. Only the enabled line of config.ini changes.",
        )}
      />
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
        <dt className="text-[var(--color-muted)]">{tr("hush_running", undefined, "Running")}</dt>
        <dd>{proc ? `${proc.comm || proc.name} · pid ${proc.pid}` : tr("hush_not_running", undefined, "No")}</dd>
        <dt className="text-[var(--color-muted)]">{tr("hush_mode", undefined, "Mode")}</dt>
        <dd>{c.mode === "check" ? tr("hush_mode_check", undefined, "check mode") : tr("hush_mode_apply", undefined, "apply")}</dd>
        <dt className="text-[var(--color-muted)]">{tr("hush_interval", undefined, "Checks every")}</dt>
        <dd>{`${c.intervalS} s`}</dd>
      </dl>
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "hush_rp_note",
          undefined,
          "Remote Play uses the same video encoder, so while Hush is on a stream has no picture. Play pauses Hush for a stream when you ask it to, and turns it back on afterwards.",
        )}
      </p>
      <div className="space-y-1">
        <div className="flex items-center justify-between">
          <span className="text-xs font-medium">{tr("hush_log", undefined, "log.txt, newest last")}</span>
          <Button size="sm" variant="ghost" leftIcon={<RefreshCw size={13} />} onClick={() => void refresh()}>
            {tr("refresh", undefined, "Refresh")}
          </Button>
        </div>
        <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-[var(--color-surface-3)] p-2 font-mono text-[11px]">
          {view.log.length ? view.log.join("\n") : tr("hush_log_empty", undefined, "Nothing logged yet.")}
        </pre>
      </div>
    </div>
  );
}
