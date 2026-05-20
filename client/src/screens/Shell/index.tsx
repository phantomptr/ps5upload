import { useCallback, useRef, useState } from "react";
import {
  TerminalSquare,
  Send,
  Loader2,
  AlertTriangle,
  Trash2,
} from "lucide-react";
import { shellRun, type ShellRunResult } from "../../api/ps5";
import { useConnectionStore } from "../../state/connection";
import { mgmtAddr } from "../../lib/addr";
import { PageHeader, Button, EmptyState } from "../../components";
import { useTr } from "../../state/lang";
import { splitShellSequence } from "./shellSequence";

interface HistoryEntry {
  id: string;
  cmd: string;
  result?: ShellRunResult;
  error?: string;
  whenMs: number;
  durationMs?: number;
}

export default function ShellScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);

  const [cmd, setCmd] = useState("");
  const [busy, setBusy] = useState(false);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [historyCursor, setHistoryCursor] = useState<number | null>(null);
  const [cwd, setCwd] = useState("/");
  const inputRef = useRef<HTMLInputElement>(null);
  const outputRef = useRef<HTMLDivElement>(null);
  const sessionIdRef = useRef("");

  const send = useCallback(async () => {
    const trimmed = cmd.trim();
    if (!trimmed || busy) return;
    if (!host?.trim() || payloadStatus !== "up") return;
    if (!sessionIdRef.current) {
      sessionIdRef.current = `shell-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    }
    const id = `${Date.now()}-${Math.random()}`;
    const entry: HistoryEntry = {
      id,
      cmd: trimmed,
      whenMs: Date.now(),
    };
    setHistory((prev) => [...prev, entry].slice(-50));
    setBusy(true);
    setCmd("");
    setHistoryCursor(null);
    const t0 = performance.now();
    try {
      const parts = splitShellSequence(trimmed);
      let currentCwd = cwd;
      let previousExit = 0;
      let stdout = "";
      let lastResult: ShellRunResult | null = null;

      for (const part of parts) {
        if (part.op === "and" && previousExit !== 0) continue;
        if (part.op === "or" && previousExit === 0) continue;

        const r = await shellRun(
          mgmtAddr(host),
          part.cmd,
          sessionIdRef.current,
          currentCwd,
          30,
        );
        lastResult = r;
        previousExit = r.exit_code ?? 1;
        stdout += r.stdout ?? "";

        if (r.cwd?.startsWith("/")) {
          currentCwd = r.cwd;
        } else if (isCdCommand(part.cmd) && r.exit_code === 0) {
          const lines = r.stdout.trim().split(/\r?\n/).filter(Boolean);
          const nextCwd = lines[lines.length - 1];
          if (nextCwd?.startsWith("/")) currentCwd = nextCwd;
        }

        if (r.timed_out) break;
      }

      const durationMs = performance.now() - t0;
      if (!lastResult) throw new Error("No shell command to run");
      const result: ShellRunResult = { ...lastResult, stdout, cwd: currentCwd };
      setCwd(currentCwd);
      setHistory((prev) =>
        prev.map((h) => (h.id === id ? { ...h, result, durationMs } : h)),
      );
    } catch (e) {
      const durationMs = performance.now() - t0;
      const error = e instanceof Error ? e.message : String(e);
      setHistory((prev) =>
        prev.map((h) => (h.id === id ? { ...h, error, durationMs } : h)),
      );
    } finally {
      setBusy(false);
      // Re-focus input + scroll to bottom on next paint.
      requestAnimationFrame(() => {
        inputRef.current?.focus();
        if (outputRef.current) {
          outputRef.current.scrollTop = outputRef.current.scrollHeight;
        }
      });
    }
  }, [cmd, busy, cwd, host, payloadStatus]);

  function handleKey(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") {
      e.preventDefault();
      void send();
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      const cmds = history.map((h) => h.cmd);
      if (cmds.length === 0) return;
      const next = historyCursor === null ? cmds.length - 1 : Math.max(0, historyCursor - 1);
      setHistoryCursor(next);
      setCmd(cmds[next] ?? "");
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      const cmds = history.map((h) => h.cmd);
      if (cmds.length === 0 || historyCursor === null) return;
      const next = historyCursor + 1;
      if (next >= cmds.length) {
        setHistoryCursor(null);
        setCmd("");
      } else {
        setHistoryCursor(next);
        setCmd(cmds[next] ?? "");
      }
    }
  }

  return (
    <div className="flex h-full flex-col p-6">
      <PageHeader
        icon={TerminalSquare}
        title={tr("shell_title", undefined, "Shell")}
        description={tr(
          "shell_description",
          undefined,
          "Stateful remote shell session. Commands run in the payload's process context; stdout and stderr are merged and capped at 256 KB. Use up/down arrows for command history.",
        )}
        right={
          <Button
            variant="ghost"
            size="sm"
            leftIcon={<Trash2 size={12} />}
            onClick={() => setHistory([])}
            disabled={history.length === 0}
          >
            {tr("shell_clear", undefined, "Clear history")}
          </Button>
        }
      />

      {payloadStatus !== "up" ? (
        <EmptyState
          icon={TerminalSquare}
          message={tr(
            "shell_no_payload",
            undefined,
            "Connect to your PS5 first — shell needs the payload to be running.",
          )}
        />
      ) : (
        <div className="flex min-h-0 flex-1 flex-col gap-3">
          <div
            ref={outputRef}
            className="flex-1 overflow-auto rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 font-mono text-[10px]"
          >
            {history.length === 0 ? (
              <div className="text-[var(--color-muted)]">
                {tr(
                  "shell_empty",
                  undefined,
                  "No history yet. Type a command below and press Enter. Try `ps`, `df -h`, or `ls /data`.",
                )}
              </div>
            ) : (
              <ul className="space-y-3">
                {history.map((h) => (
                  <ShellHistoryRow key={h.id} entry={h} />
                ))}
              </ul>
            )}
          </div>
          <div className="flex items-center gap-2">
            <span
              className="max-w-[35%] truncate font-mono text-xs text-[var(--color-muted)]"
              title={cwd}
            >
              {cwd} $
            </span>
            <input
              ref={inputRef}
              type="text"
              value={cmd}
              onChange={(e) => setCmd(e.target.value)}
              onKeyDown={handleKey}
              disabled={busy}
              placeholder={tr(
                "shell_placeholder",
                undefined,
                "Enter shell command…",
              )}
              className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 font-mono text-xs outline-none focus:border-[var(--color-accent)] disabled:opacity-50"
              autoFocus
            />
            <Button
              variant="primary"
              size="md"
              leftIcon={
                busy ? (
                  <Loader2 size={11} className="animate-spin" />
                ) : (
                  <Send size={11} />
                )
              }
              onClick={() => void send()}
              disabled={busy || !cmd.trim()}
            >
              {tr("shell_send", undefined, "Run")}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

function isCdCommand(cmd: string): boolean {
  return cmd === "cd" || cmd.startsWith("cd ");
}

function ShellHistoryRow({ entry }: { entry: HistoryEntry }) {
  const tr = useTr();
  const ts = new Date(entry.whenMs);
  const exit = entry.result?.exit_code;
  const ok = entry.result && exit === 0;
  return (
    <li>
      <div className="flex items-center gap-2 text-[10px] text-[var(--color-muted)]">
        <span>{ts.toLocaleTimeString()}</span>
        {entry.durationMs !== undefined && (
          <span>· {Math.round(entry.durationMs)}ms</span>
        )}
        {entry.result && (
          <span
            className={
              ok
                ? "text-[var(--color-good)]"
                : "text-[var(--color-warn)]"
            }
          >
            {tr("shell_exit", "· exit")} {exit ?? "?"}
          </span>
        )}
        {entry.result?.timed_out && (
          <span className="text-[var(--color-warn)]">
            {tr("shell_timed_out", "· timed out")}
          </span>
        )}
      </div>
      <div className="mt-0.5 font-medium text-[var(--color-accent)]">
        $ {entry.cmd}
      </div>
      {entry.error ? (
        <div className="mt-1 flex items-start gap-1 text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          <pre className="whitespace-pre-wrap break-all">{entry.error}</pre>
        </div>
      ) : entry.result ? (
        entry.result.stdout ? (
          <pre className="mt-1 whitespace-pre-wrap break-all text-[var(--color-text)]">
            {entry.result.stdout}
          </pre>
        ) : (
          <div className="mt-1 italic text-[var(--color-muted)]">
            {tr("shell_no_output", undefined, "(no output)")}
          </div>
        )
      ) : (
        <div className="mt-1 italic text-[var(--color-muted)]">
          <Loader2 size={10} className="mr-1 inline animate-spin" />
          {tr("shell_running", undefined, "running…")}
        </div>
      )}
    </li>
  );
}
