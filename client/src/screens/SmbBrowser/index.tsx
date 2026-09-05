import { useCallback, useState } from "react";
import {
  Network,
  Folder,
  File,
  ChevronLeft,
  RefreshCw,
  HardDrive,
  Download,
  Home,
  Upload,
} from "lucide-react";
import {
  PageHeader,
  Button,
  ErrorCard,
  ConnectionGate,
  Card,
  EmptyState,
  Input,
  Spinner,
} from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { transferAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import { isTauriEnv } from "../../lib/tauriEnv";
import {
  smbListShares,
  smbListDir,
  smbDownloadFile,
  smbTransferToPs5,
  waitForJob,
  type SmbShare,
  type SmbDirEntry,
} from "../../api/ps5";

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

export default function SmbBrowserScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";

  // Empty rather than a sample address: the old default pointed at a
  // subnet almost nobody is on, so "Connect" looked broken until you
  // noticed it needed editing. The placeholder shows the shape instead.
  const [server, setServer] = useState("");
  const [user, setUser] = useState("guest");
  const [password, setPassword] = useState("");
  const [connected, setConnected] = useState(false);
  const [shares, setShares] = useState<SmbShare[]>([]);
  const [currentShare, setCurrentShare] = useState<string | null>(null);
  const [entries, setEntries] = useState<SmbDirEntry[]>([]);
  const [pathStack, setPathStack] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [uploading, setUploading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  // Same default parent as the Upload screen's mental model.
  const [destRoot, setDestRoot] = useState("/data/homebrew");

  const handleConnect = useCallback(async () => {
    setLoading(true);
    setError(null);
    setStatus(null);
    setShares([]);
    setEntries([]);
    setConnected(false);
    setCurrentShare(null);
    try {
      const resp = await smbListShares(server, user, password);
      setShares(resp.shares ?? []);
      setConnected(true);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setLoading(false);
    }
  }, [server, user, password]);

  const browseShare = useCallback(
    async (shareName: string) => {
      setLoading(true);
      setError(null);
      setPathStack([]);
      try {
        const resp = await smbListDir(server, user, shareName, "", password);
        setEntries(resp.entries ?? []);
        setCurrentShare(shareName);
      } catch (e) {
        setError(humanizePs5Error(String(e)));
      } finally {
        setLoading(false);
      }
    },
    [server, user, password],
  );

  const browseDir = useCallback(
    async (dirName: string) => {
      if (!currentShare) return;
      const newPathStack = [...pathStack, dirName];
      setLoading(true);
      setError(null);
      try {
        const path = newPathStack.join("/");
        const resp = await smbListDir(server, user, currentShare, path, password);
        setEntries(resp.entries ?? []);
        setPathStack(newPathStack);
      } catch (e) {
        setError(humanizePs5Error(String(e)));
      } finally {
        setLoading(false);
      }
    },
    [currentShare, pathStack, server, user, password],
  );

  const navigateTo = useCallback(
    async (depth: number) => {
      if (!currentShare) return;
      const newPathStack = pathStack.slice(0, depth);
      setLoading(true);
      setError(null);
      try {
        const path = newPathStack.join("/");
        const resp = await smbListDir(server, user, currentShare, path, password);
        setEntries(resp.entries ?? []);
        setPathStack(newPathStack);
      } catch (e) {
        setError(humanizePs5Error(String(e)));
      } finally {
        setLoading(false);
      }
    },
    [currentShare, pathStack, server, user, password],
  );

  const goUp = useCallback(async () => {
    if (!currentShare || pathStack.length === 0) return;
    const newPathStack = pathStack.slice(0, -1);
    setLoading(true);
    setError(null);
    try {
      const path = newPathStack.join("/");
      const resp = await smbListDir(server, user, currentShare, path, password);
      setEntries(resp.entries ?? []);
      setPathStack(newPathStack);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setLoading(false);
    }
  }, [currentShare, pathStack, server, user, password]);

  const handleDownload = useCallback(
    async (fileName: string) => {
      if (!currentShare) return;
      const fullPath = [...pathStack, fileName].join("/");
      const { save } = await import("@tauri-apps/plugin-dialog");
      const destPath = await save({
        defaultPath: fileName,
      });
      if (!destPath || typeof destPath !== "string") return;
      setDownloading(fileName);
      setError(null);
      try {
        await smbDownloadFile(server, user, currentShare, fullPath, destPath, password);
      } catch (e) {
        setError(humanizePs5Error(String(e)));
      } finally {
        setDownloading(null);
      }
    },
    [currentShare, pathStack, server, user, password],
  );

  const handleUploadToPs5 = useCallback(
    async (name: string, isDir: boolean) => {
      if (!currentShare) return;
      if (payloadStatus !== "up" || !addr) {
        setError(
          tr("smb_need_payload", undefined, "Connect a PS5 with the payload loaded first"),
        );
        return;
      }
      const root = destRoot.trim();
      if (!root) {
        setError(tr("smb_need_dest", undefined, "Enter a PS5 destination folder"));
        return;
      }
      const fullPath = [...pathStack, name].join("/");
      const key = isDir ? `dir:${name}` : name;
      setUploading(key);
      setError(null);
      setStatus(tr("smb_uploading", undefined, "Staging from SMB and uploading…"));
      try {
        const jobId = await smbTransferToPs5(
          server,
          user,
          currentShare,
          fullPath,
          root,
          addr,
          password,
        );
        const snap = await waitForJob(jobId);
        const dest =
          snap.status === "done" && "dest" in snap && typeof snap.dest === "string"
            ? snap.dest
            : `${root.replace(/\/$/, "")}/${name}`;
        setStatus(
          tr("smb_upload_done", { dest }, `Uploaded to ${dest}`),
        );
      } catch (e) {
        setError(humanizePs5Error(String(e)));
        setStatus(null);
      } finally {
        setUploading(null);
      }
    },
    [
      currentShare,
      pathStack,
      destRoot,
      payloadStatus,
      addr,
      server,
      user,
      password,
      tr,
    ],
  );

  const sortedEntries = [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    return a.name.localeCompare(b.name);
  });

  const canUploadPs5 = payloadStatus === "up" && !!addr;

  return (
    <div className="p-6">
      <ConnectionGate>
        <PageHeader
          icon={Network}
          title={tr("smb_title", undefined, "SMB Browser")}
          description={tr(
            "smb_subtitle",
            undefined,
            "Browse a NAS or Windows share, download to this computer, or upload straight to the PS5",
          )}
        />

        {error && (
          <div className="mb-4">
            <ErrorCard title={error} />
          </div>
        )}
        {status && !error && (
          <Card className="mb-4 text-sm text-[var(--color-good)]">{status}</Card>
        )}

        {/* Connection form */}
        {!connected && (
          <Card className="mb-4 space-y-4">
            <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
              <Input
                label={tr("smb_server", undefined, "Server")}
                type="text"
                value={server}
                onChange={(e) => setServer(e.target.value)}
                className="font-mono"
                placeholder="192.168.1.100"
                inputMode="url"
              />
              <Input
                label={tr("smb_user", undefined, "Username")}
                type="text"
                value={user}
                onChange={(e) => setUser(e.target.value)}
                placeholder={tr("smb_user_placeholder", "guest")}
              />
              <Input
                label={tr("smb_password", undefined, "Password")}
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </div>
            <Button
              variant="primary"
              size="md"
              onClick={() => void handleConnect()}
              disabled={loading}
            >
              {loading ? <Spinner size={16} tone="inherit" /> : <Network size={16} />}
              {tr("smb_connect", undefined, "Connect")}
            </Button>
          </Card>
        )}

        {/* Connected: show shares or directory listing */}
        {connected && (
          <>
            <Card className="mb-4 space-y-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex min-w-0 flex-wrap items-center gap-2 text-sm">
                  <HardDrive size={16} className="shrink-0 text-[var(--color-muted)]" />
                  <span className="font-mono">{server}</span>
                  <span className="text-[var(--color-muted)]">·</span>
                  <span>{user}</span>
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  {currentShare && (
                    <>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => void goUp()}
                        disabled={loading || pathStack.length === 0}
                      >
                        <ChevronLeft size={14} />
                        {tr("smb_up", undefined, "Up")}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => void browseShare(currentShare)}
                        disabled={loading}
                      >
                        <RefreshCw size={14} />
                      </Button>
                    </>
                  )}
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      setConnected(false);
                      setShares([]);
                      setEntries([]);
                      setCurrentShare(null);
                      setStatus(null);
                    }}
                  >
                    {tr("smb_disconnect", undefined, "Disconnect")}
                  </Button>
                </div>
              </div>
              <Input
                label={tr("smb_dest_root", undefined, "PS5 destination folder")}
                type="text"
                value={destRoot}
                onChange={(e) => setDestRoot(e.target.value)}
                className="font-mono"
                placeholder="/data/homebrew"
              />
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "smb_dest_root_hint",
                  undefined,
                  "Source name is appended (same as Upload)",
                )}
              </p>
            </Card>

            {/* Breadcrumb navigation */}
            {currentShare && (
              <Card className="mb-4 flex items-center gap-1 overflow-x-auto text-sm">
                <button
                  className="flex shrink-0 items-center gap-1 rounded px-2 py-1 font-mono font-semibold hover:bg-[var(--color-surface-3)]"
                  onClick={() => void navigateTo(0)}
                >
                  <Home size={14} />
                  {currentShare}
                </button>
                {pathStack.map((dir, i) => (
                  <div key={i} className="flex shrink-0 items-center gap-1">
                    <span className="text-[var(--color-muted)]">/</span>
                    <button
                      className="rounded px-2 py-1 font-mono hover:bg-[var(--color-surface-3)]"
                      onClick={() => void navigateTo(i + 1)}
                    >
                      {dir}
                    </button>
                  </div>
                ))}
              </Card>
            )}

            {loading ? (
              <div className="flex items-center justify-center py-12">
                <Spinner size={32} />
              </div>
            ) : !currentShare ? (
              shares.length === 0 ? (
                <EmptyState
                  icon={Network}
                  title={tr("smb_no_shares", undefined, "No shares found")}
                  message={tr(
                    "smb_no_shares_desc",
                    undefined,
                    "The server has no accessible shares",
                  )}
                />
              ) : (
                <div className="space-y-2">
                  {shares.map((s) => (
                    <div
                      key={s.name}
                      className="flex cursor-pointer items-center gap-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 transition-colors hover:bg-[var(--color-surface-3)]"
                      onClick={() => void browseShare(s.name)}
                    >
                      <HardDrive size={20} className="shrink-0 text-[var(--color-muted)]" />
                      <div className="min-w-0 flex-1">
                        <div className="font-mono font-semibold">{s.name}</div>
                        {s.comment && (
                          <div className="text-sm text-[var(--color-muted)]">{s.comment}</div>
                        )}
                      </div>
                      <span className="shrink-0 text-xs text-[var(--color-muted)]">
                        {s.share_type}
                      </span>
                    </div>
                  ))}
                </div>
              )
            ) : entries.length === 0 ? (
              <EmptyState
                icon={Folder}
                title={tr("smb_empty", undefined, "Empty directory")}
                message={tr("smb_empty_desc", undefined, "This folder contains no files")}
              />
            ) : (
              <div className="space-y-1">
                {sortedEntries.map((e) => {
                  const upKey = e.is_dir ? `dir:${e.name}` : e.name;
                  const isUp = uploading === upKey;
                  return (
                    <div
                      key={e.name}
                      className={`flex items-center gap-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 transition-colors ${
                        e.is_dir ? "cursor-pointer hover:bg-[var(--color-surface-3)]" : ""
                      }`}
                      onClick={() => {
                        if (e.is_dir) void browseDir(e.name);
                      }}
                    >
                      {e.is_dir ? (
                        <Folder size={18} className="shrink-0 text-[var(--color-accent)]" />
                      ) : (
                        <File size={18} className="shrink-0 text-[var(--color-muted)]" />
                      )}
                      <span className="min-w-0 flex-1 truncate font-mono text-sm">
                        {e.name}
                      </span>
                      {!e.is_dir && (
                        <span className="shrink-0 text-xs text-[var(--color-muted)]">
                          {formatSize(e.size)}
                        </span>
                      )}
                      {/* Download saves to THIS computer via the native file
                          dialog, so it exists only in the desktop app. Uploading
                          to the PS5 (below) is engine-side and works anywhere. */}
                      {!e.is_dir && isTauriEnv() && (
                        <button
                          className="shrink-0 rounded p-1.5 text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
                          onClick={(ev) => {
                            ev.stopPropagation();
                            void handleDownload(e.name);
                          }}
                          disabled={downloading === e.name || !!uploading}
                          title={tr("smb_download", undefined, "Download to this computer")}
                        >
                          {downloading === e.name ? (
                            <Spinner size={14} tone="inherit" />
                          ) : (
                            <Download size={14} />
                          )}
                        </button>
                      )}
                      <button
                        className="shrink-0 rounded p-1.5 text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] disabled:opacity-40"
                        onClick={(ev) => {
                          ev.stopPropagation();
                          void handleUploadToPs5(e.name, e.is_dir);
                        }}
                        disabled={!canUploadPs5 || !!uploading || !!downloading}
                        title={
                          e.is_dir
                            ? tr("smb_upload_folder_ps5", undefined, "Upload folder to PS5")
                            : tr("smb_upload_ps5", undefined, "Upload to PS5")
                        }
                      >
                        {isUp ? (
                          <Spinner size={14} tone="inherit" />
                        ) : (
                          <Upload size={14} />
                        )}
                      </button>
                    </div>
                  );
                })}
              </div>
            )}
          </>
        )}
      </ConnectionGate>
    </div>
  );
}
