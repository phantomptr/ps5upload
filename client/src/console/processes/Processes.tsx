import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Cpu, Gamepad2, RefreshCw, Settings2 } from "lucide-react";
import { call } from "../api";
import { parseProcesses, type Proc } from "../games/model";
import { errorText, keepsDialogOpen, type Message } from "../home/Home";
import { onNavAction } from "../nav";
import type { SectionContext } from "../sections";
import { t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { pollWhileVisible } from "../ui/visiblePoll";

/** The list is read this often while Processes is open and the page shows (the PC app's is 3 s). */
export const PROCS_POLL_MS = 5_000;
const MESSAGE_MS = 8_000;

/**
 * Whether the page may stop `p`. Only games and payloads, never the helper: the PC app also offers
 * system processes behind a second warning, but stopping SceShellUI or the browser this page runs in
 * freezes the console or closes the page, so the console page never offers it (the helper refuses it
 * too: /api/proc/kill, webui_proc_kill_refusal).
 */
export function canStop(p: Proc): boolean {
  return !p.isSelf && (p.kind === "app" || p.kind === "payload");
}

/** Whether `p` is the game on screen (PROC_FOCUS through /api/status). */
export function isForeground(p: Proc, fg: { title_id: string; app_id: number } | null): boolean {
  if (!fg || p.kind !== "app") return false;
  return (fg.app_id > 0 && p.appId === fg.app_id) || (!!fg.title_id && p.titleId === fg.title_id);
}

/** Games first, then payloads, then the rest; each by memory, largest first. System ones only when asked. */
export function visibleProcs(procs: Proc[], showSystem: boolean): Proc[] {
  const rank = (p: Proc) => (p.kind === "app" ? 0 : p.kind === "payload" ? 1 : 2);
  return procs
    .filter((p) => showSystem || p.kind !== "system" || p.isSelf)
    .sort((a, b) => rank(a) - rank(b) || (b.memoryMib ?? 0) - (a.memoryMib ?? 0) || a.pid - b.pid);
}

/** A failed stop as one line. */
export function stopErrorText(err: unknown): string | null {
  const code = (err as { code?: unknown })?.code;
  if (code === "not_running") return t("cw_procs_err_gone");
  if (code === "system" || code === "helper") return t("cw_procs_err_protected");
  return errorText(err);
}

export interface ProcessesViewProps {
  /** The System section's view switch, shown under the title. */
  switcher?: ReactNode;
  procs: Proc[] | null;
  truncated: boolean;
  loadError: string | null;
  showSystem: boolean;
  foreground: { title_id: string; app_id: number } | null;
  message: Message | null;
  ask: Proc | null;
  busy: boolean;
  askError?: string | null;
  onToggleSystem: () => void;
  onRefresh: () => void;
  onAsk: (p: Proc) => void;
  onConfirm: () => void;
  onDismiss: () => void;
}

function kindLabel(p: Proc): string {
  if (p.isSelf) return t("cw_procs_this_helper");
  if (p.kind === "app") return t("cw_procs_kind_game");
  if (p.kind === "payload") return t("cw_procs_kind_payload");
  return t("cw_procs_kind_system");
}

function ProcRow({ p, fg, onAsk }: { p: Proc; fg: boolean; onAsk: (p: Proc) => void }) {
  const stop = canStop(p);
  const Icon = p.kind === "app" ? Gamepad2 : p.kind === "payload" ? Settings2 : Cpu;
  const facts = [
    t("cw_procs_pid", { pid: p.pid }),
    p.memoryMib != null ? t("cw_procs_mb", { mb: p.memoryMib.toFixed(1) }) : null,
    p.threads != null ? t("cw_procs_threads", { n: p.threads }) : null,
    p.titleId || null,
  ].filter((x): x is string => !!x);
  return (
    <li className="cw-task-item">
      <button
        type="button"
        className={`cw-focusable cw-proc${fg ? " is-fg" : ""}`}
        data-pid={p.pid}
        aria-disabled={stop ? undefined : true}
        onClick={() => {
          if (stop) onAsk(p);
        }}
      >
        <Icon className="cw-task-icon" aria-hidden />
        <span className="cw-task-text">
          <span className="cw-task-title">
            {p.comm || p.name || String(p.pid)}
            <span className={`cw-tag${p.kind === "app" ? " is-on" : ""}`}>{kindLabel(p)}</span>
            {fg ? <span className="cw-tag is-pc">{t("cw_games_on_screen")}</span> : null}
          </span>
          <span className="cw-dim cw-mono cw-task-sub">{facts.join(" · ")}</span>
        </span>
        {stop ? <span className="cw-tag cw-proc-stop">{t("cw_procs_stop")}</span> : null}
      </button>
    </li>
  );
}

/** Processes without state: everything comes in through props (tests render it directly). */
export function ProcessesView(p: ProcessesViewProps) {
  const shown = useMemo(() => (p.procs ? visibleProcs(p.procs, p.showSystem) : []), [p.procs, p.showSystem]);
  let body;
  if (!p.procs) {
    body = p.loadError ? (
      <div className="cw-msg is-error" role="alert">
        {p.loadError} {t("cw_home_retrying")}
      </div>
    ) : (
      <p className="cw-dim">{t("cw_home_loading")}</p>
    );
  } else {
    body = (
      <ul className="cw-tasks cw-procs" aria-label={t("cw_procs_title")}>
        {shown.map((x) => (
          <ProcRow key={x.pid} p={x} fg={isForeground(x, p.foreground)} onAsk={p.onAsk} />
        ))}
      </ul>
    );
  }
  const a = p.ask;
  return (
    <div className="cw-procs-section">
      <h1 className="cw-h1">
        {t("cw_procs_title")}
        {p.procs ? <span className="cw-tag">{p.procs.length}</span> : null}
      </h1>
      {p.switcher}
      <p className="cw-dim">{t("cw_procs_intro")}</p>
      <div className="cw-toolbar">
        <button
          type="button"
          className="cw-focusable cw-button"
          aria-pressed={p.showSystem}
          data-action="procs-system"
          onClick={p.onToggleSystem}
        >
          <Cpu aria-hidden />
          <span>{t("cw_procs_show_system")}</span>
        </button>
        <button type="button" className="cw-focusable cw-button" data-action="procs-refresh" onClick={p.onRefresh}>
          <RefreshCw aria-hidden />
          <span>{t("cw_cheats_refresh")}</span>
        </button>
      </div>
      {p.truncated ? <p className="cw-dim">{t("cw_procs_truncated")}</p> : null}
      {p.loadError && p.procs ? (
        <div className="cw-msg is-error" role="alert">
          {p.loadError} {t("cw_home_retrying")}
        </div>
      ) : null}
      {p.message ? (
        <div className={`cw-msg ${p.message.tone === "error" ? "is-error" : "is-info"}`} role={p.message.tone === "error" ? "alert" : "status"}>
          {p.message.text}
        </div>
      ) : null}
      {body}
      {a ? (
        <ConfirmDialog
          title={t(a.kind === "app" ? "cw_procs_confirm_game_title" : "cw_procs_confirm_payload_title", {
            name: a.comm || a.name,
          })}
          body={t(a.kind === "app" ? "cw_procs_confirm_game_body" : "cw_procs_confirm_payload_body")}
          confirmLabel={t("cw_procs_stop")}
          danger
          busy={p.busy}
          error={p.askError}
          onConfirm={p.onConfirm}
          onCancel={p.onDismiss}
        />
      ) : null}
    </div>
  );
}

/** The Processes section: what runs on the console, and Stop for a game or a payload. */
export function Processes({ ctx, switcher }: { ctx: SectionContext; switcher?: ReactNode }) {
  const [procs, setProcs] = useState<Proc[] | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [showSystem, setShowSystem] = useState(false);
  const [message, setMessage] = useState<Message | null>(null);
  const [ask, setAsk] = useState<Proc | null>(null);
  const [busy, setBusy] = useState(false);
  const [askError, setAskError] = useState<string | null>(null);
  const live = useRef(true);
  const root = useRef<HTMLDivElement>(null);
  const { refreshStatus } = ctx;

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  useEffect(() => {
    if (!message) return;
    const id = setTimeout(() => setMessage(null), MESSAGE_MS);
    return () => clearTimeout(id);
  }, [message]);

  const load = useCallback(async () => {
    try {
      const r = parseProcesses(await call<unknown>("/api/proc/list"));
      if (!live.current) return;
      setProcs(r.procs);
      setTruncated(r.truncated);
      setLoadError(null);
    } catch (e) {
      if (live.current) setLoadError(errorText(e));
    }
  }, []);

  useEffect(() => pollWhileVisible(load, PROCS_POLL_MS), [load]);
  useEffect(
    () =>
      onNavAction(
        (a) => {
          if (a !== "refresh") return false;
          void load();
          return false;
        },
        () => root.current,
      ),
    [load],
  );

  const onConfirm = useCallback(async () => {
    if (!ask || busy) return;
    setBusy(true);
    setAskError(null);
    try {
      await call("/api/proc/kill", { pid: ask.pid });
      setMessage({ tone: "info", text: t("cw_procs_stopped", { name: ask.comm || ask.name }) });
      setAsk(null);
      if (ask.kind === "app") refreshStatus();
      void load();
    } catch (e) {
      const text = stopErrorText(e);
      if (keepsDialogOpen(e)) setAskError(text);
      else {
        setAsk(null);
        if (text) setMessage({ tone: "error", text });
        void load();
      }
    } finally {
      setBusy(false);
    }
  }, [ask, busy, load, refreshStatus]);

  const run = ctx.status?.running ?? null;
  return (
    <div ref={root}>
      <ProcessesView
        switcher={switcher}
        procs={procs}
        truncated={truncated}
        loadError={loadError}
        showSystem={showSystem}
        foreground={run ? { title_id: run.title_id, app_id: run.app_id } : null}
        message={message}
        ask={ask}
        busy={busy}
        askError={askError}
        onToggleSystem={() => setShowSystem((s) => !s)}
        onRefresh={() => void load()}
        onAsk={(p) => {
          if (!canStop(p)) return;
          setAskError(null);
          setAsk(p);
        }}
        onConfirm={() => void onConfirm()}
        onDismiss={() => {
          if (!busy) {
            setAsk(null);
            setAskError(null);
          }
        }}
      />
    </div>
  );
}
