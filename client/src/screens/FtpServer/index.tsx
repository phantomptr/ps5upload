import { useCallback, useEffect, useState } from "react";
import { Server, RefreshCw, Play, Square, CheckCircle2, AlertTriangle, Lock } from "lucide-react";
import { PageHeader, Button, ErrorCard, ConnectionGate, Card, Spinner, Input, Toggle } from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { transferAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import { ftpStart, ftpStatus, type FtpStatusResponse } from "../../api/ps5";

export default function FtpServerScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";

  const [status, setStatus] = useState<FtpStatusResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [actionLoading, setActionLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Not 2121: that is ftpsrv.elf's default, and ftpsrv is in our own
  // payload catalogue — so anyone running both got bind_failed out of the
  // box. 2122 is adjacent enough to stay memorable.
  const [port, setPort] = useState(2122);
  const [root, setRoot] = useState("/");
  const [readonly, setReadonly] = useState(true);
  const [user, setUser] = useState("");
  const [pass, setPass] = useState("");

  const refresh = useCallback(async () => {
    if (!addr || payloadStatus !== "up") return;
    setLoading(true);
    setError(null);
    try {
      const resp = await ftpStatus(addr);
      setStatus(resp);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setLoading(false);
    }
  }, [addr, payloadStatus]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const handleStart = async () => {
    if (!addr) return;
    setActionLoading(true);
    setError(null);
    try {
      await ftpStart({ port, root, readonly, user: user || undefined, pass: pass || undefined }, addr);
      await refresh();
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setActionLoading(false);
    }
  };

  const handleStop = async () => {
    if (!addr) return;
    setActionLoading(true);
    setError(null);
    try {
      await ftpStart({ port: 0 }, addr);
      await refresh();
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setActionLoading(false);
    }
  };

  const running = status?.running ?? false;

  return (
    <div className="p-6">
      <ConnectionGate>
        <PageHeader
          icon={Server}
          title={tr("ftp_title", undefined, "FTP Server")}
          description={tr(
            "ftp_subtitle",
            undefined,
            "Run a lightweight FTP server on the PS5 for file access",
          )}
          right={
            <Button variant="ghost" onClick={() => void refresh()} disabled={loading}>
              {loading ? <Spinner size={16} tone="inherit" /> : <RefreshCw size={16} />}
              {tr("refresh", undefined, "Refresh")}
            </Button>
          }
        />

        {error && <div className="mb-4"><ErrorCard title={error} /></div>}

        {running && (
          <Card className="mb-4 flex items-center gap-4" accent>
            <CheckCircle2 size={32} className="shrink-0 text-[var(--color-good)]" />
            <div className="min-w-0 flex-1">
              <div className="text-lg font-semibold">
                {tr("ftp_running", undefined, "FTP Server Running")}
              </div>
              <div className="text-sm text-[var(--color-muted)]">
                {tr("ftp_port", undefined, "Port")}: <code className="font-mono">{status?.port}</code>
                {" · "}
                {tr("ftp_connections", undefined, "Connections")}: {status?.connections ?? 0}
                {" · "}
                {tr("ftp_root", undefined, "Root")}: <code className="font-mono">{status?.root ?? "/"}</code>
              </div>
            </div>
            <Button variant="danger" onClick={() => void handleStop()} disabled={actionLoading}>
              <Square size={16} />
              {tr("ftp_stop", undefined, "Stop")}
            </Button>
          </Card>
        )}

        {!running && (
          <Card className="mb-4 space-y-4">
            <div className="flex items-start gap-2">
              <AlertTriangle size={18} className="mt-0.5 shrink-0 text-[var(--color-warn)]" />
              <span className="text-sm font-medium">
                {tr("ftp_warning", undefined, "FTP provides file access. Read-only mode is recommended.")}
              </span>
            </div>

            <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
              <Input
                label={tr("ftp_port_label", undefined, "Port")}
                type="number"
                value={port}
                onChange={(e) => setPort(parseInt(e.target.value) || 2122)}
                min={1}
                max={65535}
                inputMode="numeric"
              />

              <Input
                label={tr("ftp_root_label", undefined, "Root Directory")}
                type="text"
                value={root}
                onChange={(e) => setRoot(e.target.value)}
                className="font-mono"
                placeholder="/"
              />

              <Input
                label={tr("ftp_user_label", undefined, "Username (optional)")}
                type="text"
                value={user}
                onChange={(e) => setUser(e.target.value)}
                placeholder={tr("ftp_user_placeholder", "anonymous")}
              />

              <Input
                label={tr("ftp_pass_label", undefined, "Password (optional)")}
                type="password"
                value={pass}
                onChange={(e) => setPass(e.target.value)}
                placeholder=""
              />
            </div>

            <Toggle
              checked={readonly}
              onChange={setReadonly}
              label={
                <span className="inline-flex items-center gap-1.5">
                  <Lock size={14} />
                  {tr("ftp_readonly", undefined, "Read-only (recommended — prevents writes/deletes)")}
                </span>
              }
            />

            <Button variant="primary" size="md" onClick={() => void handleStart()} disabled={actionLoading}>
              {actionLoading ? <Spinner size={16} tone="inherit" /> : <Play size={16} />}
              {tr("ftp_start", undefined, "Start FTP Server")}
            </Button>
          </Card>
        )}
      </ConnectionGate>
    </div>
  );
}
