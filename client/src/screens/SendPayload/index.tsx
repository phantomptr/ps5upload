import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  Rocket,
  Send,
  Loader2,
  CheckCircle2,
  XCircle,
  AlertTriangle,
  FolderOpen,
  History,
  Trash2,
  RotateCcw,
} from "lucide-react";

import {
  useConnectionStore,
  PS5_LOADER_PORT,
} from "../../state/connection";
import { sendPayload } from "../../api/ps5";
import { PageHeader } from "../../components";
import { useTr } from "../../state/lang";
import { PlaylistsPanel } from "./PlaylistsPanel";

/**
 * Send a custom ELF to your PS5's payload loader.
 *
 * Mirrors the Connection screen's send step, but unbound from ps5upload's
 * own ELF. Meant for anything else you'd normally `nc` at :9021 —
 * any PS5 homebrew loader or kernel payload.
 *
 * Probe-then-send pattern: we run the file through the existing
 * `payload_probe` command before sending so the user sees a clear
 * "looks like our payload" / "no PS5Upload signature found — use only
 * if you trust this" banner. It's not a security gate (you can send
 * anyway), just an informed-consent prompt.
 *
 * Recent-sends history is persisted via Tauri (`send_payload_history.json`
 * in the app data dir) — includes successful AND failed sends, each with
 * a status badge so the user can see "I tried foo.elf three times and it
 * failed" at a glance. Clicking a history row fills the form with that
 * record's path + port; the user can then re-send as-is or tweak.
 */

type Status =
  | { kind: "idle" }
  | { kind: "probing" }
  | { kind: "probed"; message: string; isPs5upload: boolean }
  | { kind: "sending"; bytes?: number }
  | { kind: "sent"; bytes: number }
  | { kind: "failed"; error: string };

/** Matches the Tauri `send_payload_history_*` record shape. Fields kept
 *  minimal so the history file stays small. `error` is only set when
 *  status === "failed" so a clean history doesn't carry empty strings. */
interface SendHistoryRecord {
  path: string;
  host: string;
  port: number;
  status: "success" | "failed";
  timestamp: number; // unix ms
  error?: string;
}

interface SendHistoryStore {
  records?: SendHistoryRecord[];
  updated_at?: number;
}

function probeMessage(code: string, isPs5upload: boolean): string {
  switch (code) {
    case "payload_probe_invalid_ext":
      return "Payload must be a .elf, .bin, .js, or .lua file.";
    case "payload_probe_detected":
      return "This is a PS5Upload payload.";
    case "payload_probe_no_signature":
      return isPs5upload
        ? "PS5Upload payload detected."
        : "No PS5Upload signature found — use only if you trust this payload.";
    default:
      return "Payload file looks OK.";
  }
}

/** "2m ago", "1h ago", "Apr 18" — compact timestamps for the history
 *  list. Uses absolute date only once a row is older than a week so
 *  users don't have to interpret "240h ago". */
function formatAgo(ms: number): string {
  const diff = Date.now() - ms;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  if (diff < 7 * 86_400_000) return `${Math.floor(diff / 86_400_000)}d ago`;
  return new Date(ms).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}

function fileNameFrom(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

export default function SendPayloadScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const setHost = useConnectionStore((s) => s.setHost);
  const [elfPath, setElfPath] = useState<string | null>(null);
  // Mirror of `elfPath` used by the editable text input so the user can
  // paste/type a path instead of being forced through the file picker.
  // Separate state because the text input may lag the picker momentarily
  // (picker writes both on success), and we want unsaved typing to not
  // disappear if the user clicks Send immediately.
  const [elfPathText, setElfPathText] = useState<string>("");
  const [portText, setPortText] = useState<string>(String(PS5_LOADER_PORT));
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const [history, setHistory] = useState<SendHistoryRecord[]>([]);

  const parsedPort = (() => {
    const n = Number(portText);
    if (!Number.isInteger(n) || n < 1 || n > 65535) return null;
    return n;
  })();
  const portValid = parsedPort !== null;

  const loadHistory = useCallback(async () => {
    try {
      const store = await invoke<SendHistoryStore>("send_payload_history_load");
      // Sort newest-first — on disk they're append order, but we want
      // the UI to surface recent sends at the top without the caller
      // needing to think about it.
      const records = (store.records ?? [])
        .slice()
        .sort((a, b) => b.timestamp - a.timestamp);
      setHistory(records);
    } catch {
      // Non-fatal: an empty file / parse error just means "no history yet".
      setHistory([]);
    }
  }, []);

  useEffect(() => {
    loadHistory();
  }, [loadHistory]);

  const commitToHistory = useCallback(
    async (rec: SendHistoryRecord) => {
      try {
        await invoke("send_payload_history_add", { record: rec });
        await loadHistory();
      } catch {
        // Non-fatal — history is a nicety, not load-bearing.
      }
    },
    [loadHistory],
  );

  const clearHistory = useCallback(async () => {
    try {
      await invoke("send_payload_history_clear");
      setHistory([]);
    } catch {
      // Non-fatal.
    }
  }, []);

  /** Keep elfPath (authoritative for the send call) in sync with the
   *  editable text field. Empty text clears the picked file so the
   *  "No file chosen" placeholder shows correctly. */
  const updateElfPath = (raw: string) => {
    setElfPathText(raw);
    const trimmed = raw.trim();
    setElfPath(trimmed.length > 0 ? trimmed : null);
  };

  const pickFile = async () => {
    const picked = await openDialog({
      multiple: false,
      directory: false,
      filters: [
        { name: "Payload", extensions: ["elf", "bin", "js", "lua"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (typeof picked !== "string") return;
    setElfPath(picked);
    setElfPathText(picked);
    await probeFile(picked);
  };

  /** Probe an already-selected path. Extracted so both the file picker
   *  and history-replay can trigger it. History-replayed paths may no
   *  longer exist on disk — in that case the probe surfaces the
   *  filesystem error and we skip setting a "good" state. */
  const probeFile = async (path: string) => {
    setStatus({ kind: "probing" });
    try {
      const r = await invoke<{ is_ps5upload: boolean; code: string }>(
        "payload_probe",
        { path },
      );
      setStatus({
        kind: "probed",
        isPs5upload: !!r.is_ps5upload,
        message: probeMessage(r.code, !!r.is_ps5upload),
      });
    } catch (e) {
      setStatus({
        kind: "failed",
        error: e instanceof Error ? e.message : String(e),
      });
    }
  };

  const replayFromHistory = async (rec: SendHistoryRecord) => {
    setElfPath(rec.path);
    setElfPathText(rec.path);
    setPortText(String(rec.port));
    if (rec.host && !host) setHost(rec.host);
    await probeFile(rec.path);
  };

  const send = async () => {
    if (!elfPath || !host?.trim() || parsedPort === null) return;
    setStatus({ kind: "sending" });
    const startedHost = host.trim();
    const startedPort = parsedPort;
    const startedPath = elfPath;
    try {
      await sendPayload(startedHost, startedPath, startedPort);
      setStatus({ kind: "sent", bytes: 0 });
      void commitToHistory({
        path: startedPath,
        host: startedHost,
        port: startedPort,
        status: "success",
        timestamp: Date.now(),
      });
    } catch (e) {
      const err = e instanceof Error ? e.message : String(e);
      setStatus({ kind: "failed", error: err });
      void commitToHistory({
        path: startedPath,
        host: startedHost,
        port: startedPort,
        status: "failed",
        timestamp: Date.now(),
        error: err,
      });
    }
  };

  return (
    <div className="p-6">
      <PageHeader
        icon={Rocket}
        title={tr("send_payload", undefined, "Send payload")}
        description={tr(
          "send_payload_description",
          undefined,
          "Send any PS5 payload file — .elf, .bin, .js, or .lua (kstuff, custom homebrew loaders, browser-stage exploits, plugin scripts) — to your PS5. Same flow as the Connection tab, just pointed at a file you choose.",
        )}
      />

      {/* Two-column layout on wide screens: form on the left, history
          on the right. Stacks to single-column below lg (~1024px).
          History panel grows more generously on xl to hold longer
          paths without wrapping. */}
      <div className="grid gap-6 lg:grid-cols-[1fr_22rem] xl:grid-cols-[1fr_26rem]">
        <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
          {/* IP + port on one row — port is narrow so IP gets the
              room to show long DHCP-style addresses comfortably. */}
          <div className="grid grid-cols-[1fr_7rem] gap-3">
            <div>
              <label className="block text-xs uppercase tracking-wide text-[var(--color-muted)]">
                PS5 IP address
              </label>
              <input
                value={host}
                onChange={(e) => setHost(e.target.value)}
                placeholder="192.168.1.50"
                className="mt-2 w-full rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm outline-none focus:border-[var(--color-accent)]"
              />
            </div>
            <div>
              <label className="block text-xs uppercase tracking-wide text-[var(--color-muted)]">
                Port
              </label>
              <input
                value={portText}
                onChange={(e) => setPortText(e.target.value)}
                placeholder={String(PS5_LOADER_PORT)}
                inputMode="numeric"
                className={
                  "mt-2 w-full rounded-md border bg-[var(--color-surface)] px-3 py-2 text-sm outline-none focus:border-[var(--color-accent)] " +
                  (portValid
                    ? "border-[var(--color-border)]"
                    : "border-[var(--color-bad)]")
                }
              />
            </div>
          </div>
          <div className="mt-1 text-xs text-[var(--color-muted)]">
            Sent to <code>{host || "…"}</code>:
            {parsedPort ?? portText}. Default {PS5_LOADER_PORT} matches
            the elfldr convention — change only if your loader listens
            elsewhere.
          </div>

          {/* File picker + editable path. The text input is the
              canonical source of truth — the Choose button just fills
              it for convenience. Users can paste a path from their
              terminal, which is handy when they know exactly where
              their payload lives. */}
          <label className="mt-5 block text-xs uppercase tracking-wide text-[var(--color-muted)]">
            Payload file
          </label>
          <div className="mt-2 flex items-center gap-2">
            <button
              type="button"
              onClick={pickFile}
              disabled={status.kind === "sending"}
              className="flex shrink-0 items-center gap-2 rounded-md border border-[var(--color-border)] px-3 py-2 text-sm hover:bg-[var(--color-surface-3)] disabled:opacity-50"
            >
              <FolderOpen size={14} />
              Choose
            </button>
            <input
              value={elfPathText}
              onChange={(e) => updateElfPath(e.target.value)}
              onBlur={() => {
                if (elfPath && elfPath.length > 0) probeFile(elfPath);
              }}
              placeholder="/path/to/payload.elf (or .bin / .js / .lua)"
              spellCheck={false}
              className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 font-mono text-xs outline-none focus:border-[var(--color-accent)]"
            />
          </div>

          {status.kind === "probing" && (
            <ProbeRow
              icon={
                <Loader2
                  size={14}
                  className="animate-spin text-[var(--color-accent)]"
                />
              }
              text="Checking file…"
            />
          )}
          {status.kind === "probed" && (
            <ProbeRow
              icon={
                status.isPs5upload ? (
                  <CheckCircle2
                    size={14}
                    className="text-[var(--color-good)]"
                  />
                ) : (
                  <AlertTriangle
                    size={14}
                    className="text-[var(--color-warn)]"
                  />
                )
              }
              text={status.message}
            />
          )}

          <div className="mt-5 flex items-center justify-end gap-2">
            <button
              type="button"
              onClick={send}
              disabled={
                !elfPath ||
                !host?.trim() ||
                !portValid ||
                status.kind === "sending" ||
                status.kind === "probing"
              }
              className="flex items-center gap-2 rounded-md bg-[var(--color-accent)] px-4 py-2 text-sm font-medium text-[var(--color-accent-contrast)] disabled:opacity-50"
            >
              {status.kind === "sending" ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <Send size={14} />
              )}
              Send
            </button>
          </div>

          {status.kind === "sent" && (
            <div className="mt-4 flex items-start gap-2 rounded-md border border-[var(--color-good)] bg-[var(--color-surface-3)] p-3 text-xs">
              <CheckCircle2
                size={14}
                className="mt-0.5 text-[var(--color-good)]"
              />
              <div>
                <div className="font-medium text-[var(--color-good)]">
                  Payload sent to {host}:{parsedPort ?? PS5_LOADER_PORT}
                </div>
                <div className="mt-0.5 text-[var(--color-muted)]">
                  If the file was a working payload, your PS5 is now
                  running it. Most loaders don't flash an obvious UI
                  change — check the payload's own notification or
                  status indicator on the console.
                </div>
              </div>
            </div>
          )}

          {status.kind === "failed" && (
            <div className="mt-4 flex items-start gap-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface-3)] p-3 text-xs">
              <XCircle size={14} className="mt-0.5 text-[var(--color-bad)]" />
              <div>
                <div className="font-medium text-[var(--color-bad)]">
                  Couldn't send
                </div>
                <div className="mt-0.5 text-[var(--color-muted)]">
                  {status.error}
                </div>
              </div>
            </div>
          )}
        </section>

        {/* Recent sends panel — click a row to re-populate the form
            with that record's path+port. The user is responsible for
            making sure the file still exists at that path; we surface
            the probe error clearly if it doesn't. */}
        <HistoryPanel
          records={history}
          onReplay={replayFromHistory}
          onClear={clearHistory}
        />
      </div>

      <p className="mt-4 text-xs text-[var(--color-muted)]">
        Only send payloads from sources you trust — a malicious .elf,
        .bin, .js, or .lua file can do anything on your PS5. ps5upload
        doesn't bundle or verify third-party loaders; you're responsible
        for the files you pick.
      </p>

      <PlaylistsPanel host={host} port={parsedPort ?? PS5_LOADER_PORT} />

    </div>
  );
}

function HistoryPanel({
  records,
  onReplay,
  onClear,
}: {
  records: SendHistoryRecord[];
  onReplay: (rec: SendHistoryRecord) => void;
  onClear: () => void;
}) {
  return (
    <aside className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <History size={14} />
          <h2 className="text-sm font-semibold">Recent sends</h2>
        </div>
        <button
          type="button"
          onClick={onClear}
          disabled={records.length === 0}
          title="Clear history"
          className="flex items-center gap-1 rounded-md border border-[var(--color-border)] px-2 py-1 text-[11px] hover:bg-[var(--color-surface-3)] disabled:opacity-40"
        >
          <Trash2 size={11} />
          Clear
        </button>
      </header>

      {records.length === 0 ? (
        <div className="rounded-md border border-dashed border-[var(--color-border)] p-4 text-center text-xs text-[var(--color-muted)]">
          No sends yet. Successful and failed sends will both appear
          here for quick replay.
        </div>
      ) : (
        <ul className="grid max-h-[28rem] gap-1.5 overflow-y-auto pr-1">
          {records.map((rec) => (
            <li key={`${rec.timestamp}-${rec.path}`}>
              <button
                type="button"
                onClick={() => onReplay(rec)}
                title={rec.path}
                className="group flex w-full items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-left text-xs hover:border-[var(--color-accent)] hover:bg-[var(--color-surface-3)]"
              >
                <StatusDot ok={rec.status === "success"} />
                <div className="min-w-0 flex-1">
                  <div className="truncate font-mono">
                    {fileNameFrom(rec.path)}
                  </div>
                  <div className="mt-0.5 flex items-center gap-2 text-[11px] text-[var(--color-muted)]">
                    <span>
                      {rec.host}:{rec.port}
                    </span>
                    <span>·</span>
                    <span>{formatAgo(rec.timestamp)}</span>
                  </div>
                </div>
                <RotateCcw
                  size={12}
                  className="mt-0.5 shrink-0 text-[var(--color-muted)] opacity-0 group-hover:opacity-100"
                />
              </button>
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}

function StatusDot({ ok }: { ok: boolean }) {
  return (
    <span
      className={`mt-1 inline-block h-1.5 w-1.5 shrink-0 rounded-full ${
        ok ? "bg-[var(--color-good)]" : "bg-[var(--color-bad)]"
      }`}
      aria-label={ok ? "success" : "failed"}
    />
  );
}

function ProbeRow({ icon, text }: { icon: React.ReactNode; text: string }) {
  return (
    <div className="mt-2 flex items-start gap-2 text-xs text-[var(--color-muted)]">
      <div className="mt-0.5 shrink-0">{icon}</div>
      <span>{text}</span>
    </div>
  );
}
