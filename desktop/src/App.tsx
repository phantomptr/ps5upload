import { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

const tabs = [
  { id: "transfer", label: "Transfer" },
  { id: "manage", label: "Manage" },
  { id: "chat", label: "Chat" }
] as const;

const appWindow = getCurrentWindow();

type TabId = (typeof tabs)[number]["id"];

type TransferProgressEvent = {
  run_id: number;
  sent: number;
  total: number;
  files_sent: number;
  elapsed_secs: number;
  current_file: string | null;
};

type TransferScanEvent = {
  run_id: number;
  files_found: number;
  total_size: number;
};

type TransferCompleteEvent = {
  run_id: number;
  files: number;
  bytes: number;
};

type TransferErrorEvent = {
  run_id: number;
  message: string;
};

type TransferLogEvent = {
  run_id: number;
  message: string;
};

type StorageLocation = {
  path: string;
  storage_type: string;
  free_gb: number;
};

type AppConfig = {
  address: string;
  storage: string;
  connections: number;
  use_temp: boolean;
  auto_connect: boolean;
  theme: string;
  compression: string;
  bandwidth_limit_mbps: number;
  update_channel: string;
  download_compression: string;
  chmod_after_upload: boolean;
  resume_mode: string;
  language: string;
  auto_tune_connections: boolean;
  auto_check_payload: boolean;
  optimize_upload: boolean;
  chat_display_name: string;
  rar_extract_mode: string;
};

const presetOptions = ["etaHEN/games", "homebrew", "custom"] as const;

type CompressionOption = "auto" | "none" | "lz4" | "zstd" | "lzma";

type ResumeOption = "none" | "size" | "size_mtime" | "sha256";

const isPresetOption = (
  value: string
): value is (typeof presetOptions)[number] =>
  (presetOptions as readonly string[]).includes(value);

type TransferState = {
  status: string;
  sent: number;
  total: number;
  files: number;
  elapsed: number;
  currentFile: string;
};

const formatBytes = (bytes: number) => {
  const kb = 1024;
  const mb = kb * 1024;
  const gb = mb * 1024;
  if (bytes >= gb) return `${(bytes / gb).toFixed(2)} GB`;
  if (bytes >= mb) return `${(bytes / mb).toFixed(2)} MB`;
  if (bytes >= kb) return `${(bytes / kb).toFixed(2)} KB`;
  return `${bytes} B`;
};

const joinRemote = (...parts: string[]) =>
  parts
    .filter(Boolean)
    .map((part, index) => {
      if (index === 0) return part.replace(/\/$/, "");
      return part.replace(/^\//, "").replace(/\/$/, "");
    })
    .join("/");

export default function App() {
  const [activeTab, setActiveTab] = useState<TabId>("transfer");

  const handleMinimize = async () => {
    await appWindow.minimize();
  };

  const handleToggleMaximize = async () => {
    await appWindow.toggleMaximize();
  };

  const handleCloseWindow = async () => {
    await appWindow.close();
  };

  const handleDragStart = async (event: React.MouseEvent<HTMLElement>) => {
    if (event.button !== 0) {
      return;
    }
    if ((event.target as HTMLElement).closest(".window-controls")) {
      return;
    }
    await appWindow.startDragging();
  };
  const [configDefaults, setConfigDefaults] = useState<AppConfig | null>(null);
  const [configLoaded, setConfigLoaded] = useState(false);
  const [ip, setIp] = useState("");
  const [connectionStatus, setConnectionStatus] = useState("Disconnected");
  const [storageLocations, setStorageLocations] = useState<StorageLocation[]>([]);
  const [sourcePath, setSourcePath] = useState("");
  const [storageRoot, setStorageRoot] = useState("/data");
  const [preset, setPreset] = useState<(typeof presetOptions)[number]>(
    presetOptions[0]
  );
  const [customPreset, setCustomPreset] = useState("");
  const [subfolder, setSubfolder] = useState("");
  const [compression, setCompression] = useState<CompressionOption>("auto");
  const [resumeMode, setResumeMode] = useState<ResumeOption>("none");
  const [connections, setConnections] = useState(4);
  const [bandwidthLimit, setBandwidthLimit] = useState(0);
  const [optimizeUpload, setOptimizeUpload] = useState(false);
  const [autoTune, setAutoTune] = useState(true);
  const [useTemp, setUseTemp] = useState(false);
  const [transferState, setTransferState] = useState<TransferState>({
    status: "Idle",
    sent: 0,
    total: 0,
    files: 0,
    elapsed: 0,
    currentFile: ""
  });
  const [logs, setLogs] = useState<string[]>([]);

  const destBase = preset === "custom" ? customPreset : preset;
  const destPath = useMemo(
    () => joinRemote(storageRoot, destBase, subfolder),
    [storageRoot, destBase, subfolder]
  );

  useEffect(() => {
    let active = true;
    const load = async () => {
      try {
        const cfg = await invoke<AppConfig>("config_load");
        if (!active) return;
        setConfigDefaults(cfg);
        setIp(cfg.address ?? "");
        setStorageRoot(cfg.storage || "/data");
        setConnections(cfg.connections || 4);
        setCompression((cfg.compression as CompressionOption) || "auto");
        setBandwidthLimit(cfg.bandwidth_limit_mbps || 0);
        setResumeMode((cfg.resume_mode as ResumeOption) || "none");
        setAutoTune(cfg.auto_tune_connections ?? true);
        setOptimizeUpload(cfg.optimize_upload ?? false);
        setUseTemp(cfg.use_temp ?? false);
      } catch (err) {
        setLogs((prev) => [
          `Failed to load config: ${String(err)}`,
          ...prev
        ]);
      } finally {
        if (active) {
          setConfigLoaded(true);
        }
      }
    };

    const sourceSaved = localStorage.getItem("ps5upload.source_path");
    const presetSaved = localStorage.getItem("ps5upload.preset");
    const customSaved = localStorage.getItem("ps5upload.custom_preset");
    const subfolderSaved = localStorage.getItem("ps5upload.subfolder");

    if (sourceSaved) setSourcePath(sourceSaved);
    if (presetSaved && isPresetOption(presetSaved)) {
      setPreset(presetSaved);
    }
    if (customSaved) setCustomPreset(customSaved);
    if (subfolderSaved) setSubfolder(subfolderSaved);

    load();
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!subfolder && sourcePath) {
      const name = sourcePath.split(/[/\\]/).filter(Boolean).pop();
      if (name) {
        setSubfolder(name);
      }
    }
  }, [sourcePath, subfolder]);

  useEffect(() => {
    if (!configLoaded || !configDefaults) {
      return;
    }
    const nextConfig: AppConfig = {
      ...configDefaults,
      address: ip,
      storage: storageRoot,
      connections,
      use_temp: useTemp,
      compression,
      bandwidth_limit_mbps: bandwidthLimit,
      resume_mode: resumeMode,
      auto_tune_connections: autoTune,
      optimize_upload: optimizeUpload
    };

    const handle = setTimeout(() => {
      invoke("config_save", { config: nextConfig }).catch((err) => {
        setLogs((prev) => [
          `Failed to save config: ${String(err)}`,
          ...prev
        ]);
      });
    }, 350);

    return () => clearTimeout(handle);
  }, [
    configLoaded,
    configDefaults,
    ip,
    storageRoot,
    connections,
    useTemp,
    compression,
    bandwidthLimit,
    resumeMode,
    autoTune,
    optimizeUpload
  ]);

  useEffect(() => {
    if (sourcePath) {
      localStorage.setItem("ps5upload.source_path", sourcePath);
    }
    if (preset) {
      localStorage.setItem("ps5upload.preset", preset);
    }
    if (customPreset) {
      localStorage.setItem("ps5upload.custom_preset", customPreset);
    }
    if (subfolder) {
      localStorage.setItem("ps5upload.subfolder", subfolder);
    }
  }, [sourcePath, preset, customPreset, subfolder]);

  useEffect(() => {
    let mounted = true;
    const unlisten = async () => {
      const unlistenProgress = await listen<TransferProgressEvent>(
        "transfer_progress",
        (event) => {
          if (!mounted) return;
          const payload = event.payload;
          setTransferState((prev) => ({
            ...prev,
            status: "Uploading",
            sent: payload.sent,
            total: payload.total,
            files: payload.files_sent,
            elapsed: payload.elapsed_secs,
            currentFile: payload.current_file ?? ""
          }));
        }
      );
      const unlistenScan = await listen<TransferScanEvent>(
        "transfer_scan",
        (event) => {
          if (!mounted) return;
          const payload = event.payload;
          setTransferState((prev) => ({
            ...prev,
            status: "Scanning",
            total: payload.total_size,
            files: payload.files_found
          }));
        }
      );
      const unlistenComplete = await listen<TransferCompleteEvent>(
        "transfer_complete",
        (event) => {
          if (!mounted) return;
          const payload = event.payload;
          setTransferState((prev) => ({
            ...prev,
            status: "Complete",
            sent: payload.bytes,
            total: payload.bytes
          }));
        }
      );
      const unlistenError = await listen<TransferErrorEvent>(
        "transfer_error",
        (event) => {
          if (!mounted) return;
          setTransferState((prev) => ({
            ...prev,
            status: `Error: ${event.payload.message}`
          }));
        }
      );
      const unlistenLog = await listen<TransferLogEvent>(
        "transfer_log",
        (event) => {
          if (!mounted) return;
          setLogs((prev) => [`${event.payload.message}`, ...prev].slice(0, 200));
        }
      );

      return () => {
        unlistenProgress();
        unlistenScan();
        unlistenComplete();
        unlistenError();
        unlistenLog();
      };
    };

    const cleanupPromise = unlisten();
    return () => {
      mounted = false;
      cleanupPromise.then((cleanup) => cleanup && cleanup());
    };
  }, []);

  const transferPercent =
    transferState.total > 0
      ? Math.min(100, (transferState.sent / transferState.total) * 100)
      : 0;

  const handleConnect = async () => {
    if (!ip.trim()) {
      setConnectionStatus("Missing IP");
      return;
    }
    setConnectionStatus("Connecting...");
    try {
      const locations = await invoke<StorageLocation[]>("storage_list", { ip });
      setStorageLocations(locations);
      if (locations.length > 0) {
        setStorageRoot(locations[0].path);
        setConnectionStatus("Connected");
      } else {
        setConnectionStatus("No storage");
      }
    } catch (err) {
      setConnectionStatus(`Error: ${String(err)}`);
    }
  };

  const handleDisconnect = () => {
    setStorageLocations([]);
    setConnectionStatus("Disconnected");
  };

  const handleScan = async () => {
    if (!sourcePath.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing source" }));
      return;
    }
    setTransferState((prev) => ({ ...prev, status: "Scanning" }));
    try {
      await invoke("transfer_scan", { source_path: sourcePath });
    } catch (err) {
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
    }
  };

  const handleBrowse = async () => {
    try {
      const selected = await open({
        directory: true,
        multiple: false
      });
      if (typeof selected === "string") {
        setSourcePath(selected);
      }
    } catch (err) {
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
    }
  };

  const handleUpload = async () => {
    if (!ip.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing IP" }));
      return;
    }
    if (!sourcePath.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing source" }));
      return;
    }
    if (!destPath.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing destination" }));
      return;
    }

    try {
      await invoke("transfer_start", {
        req: {
          ip,
          source_path: sourcePath,
          dest_path: destPath,
          use_temp: useTemp,
          connections,
          resume_mode: resumeMode,
          compression,
          bandwidth_limit_mbps: bandwidthLimit,
          auto_tune_connections: autoTune,
          optimize_upload: optimizeUpload,
          rar_extract_mode: "turbo",
          payload_version: null,
          storage_root: storageRoot,
          required_size: transferState.total || null
        }
      });
    } catch (err) {
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
    }
  };

  const handleCancel = async () => {
    try {
      await invoke("transfer_cancel");
      setTransferState((prev) => ({ ...prev, status: "Cancelling" }));
    } catch (err) {
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
    }
  };

  const status = useMemo(
    () => ({
      connection: connectionStatus,
      payload: "Unknown",
      transfer: transferState.status,
      storage: storageRoot
    }),
    [connectionStatus, storageRoot, transferState.status]
  );

  return (
    <div className="app">
      <div className="ambient" aria-hidden="true" />
      <aside className="sidebar shell">
        <div className="brand">
          <div className="brand-mark">PS5</div>
          <div className="brand-text">
            <span>Upload</span>
            <span className="brand-sub">Console Link</span>
          </div>
        </div>

        <section className="card">
          <header className="card-title">Connection</header>
          <label className="field">
            <span>PS5 Address</span>
            <input
              placeholder="192.168.0.105"
              value={ip}
              onChange={(event) => setIp(event.target.value)}
            />
          </label>
          <div className="split">
            <button className="btn primary" onClick={handleConnect}>
              Connect
            </button>
            <button className="btn ghost" onClick={handleDisconnect}>
              Disconnect
            </button>
          </div>
          <div className="status-grid">
            <div>
              <p>State</p>
              <strong>{status.connection}</strong>
            </div>
            <div>
              <p>Storage</p>
              <strong>{status.storage}</strong>
            </div>
          </div>
        </section>

        <section className="card">
          <header className="card-title">Profiles</header>
          <p className="muted">Profiles are not available in the web client yet.</p>
          <div className="split">
            <button className="btn" disabled>
              Save
            </button>
            <button className="btn" disabled>
              Manage
            </button>
          </div>
        </section>

        <section className="card">
          <header className="card-title">Payload</header>
          <div className="pill">Version: {status.payload}</div>
          <div className="split">
            <button className="btn" disabled>
              Check
            </button>
            <button className="btn" disabled>
              Send
            </button>
          </div>
          <button className="btn ghost" disabled>
            Download Latest
          </button>
        </section>

        <section className="card">
          <header className="card-title">Quick Settings</header>
          <label className="field inline">
            <span>Theme</span>
            <select disabled>
              <option>Dark</option>
              <option>Light</option>
            </select>
          </label>
          <label className="field inline">
            <span>Connections</span>
            <input
              type="number"
              min={1}
              max={10}
              value={connections}
              onChange={(event) => setConnections(Number(event.target.value))}
            />
          </label>
          <label className="field inline">
            <span>Resume</span>
            <select
              value={resumeMode}
              onChange={(event) => setResumeMode(event.target.value as ResumeOption)}
            >
              <option value="none">Off</option>
              <option value="size">Size</option>
              <option value="size_mtime">Size + Time</option>
              <option value="sha256">SHA256</option>
            </select>
          </label>
        </section>
      </aside>

      <main className="main">
        <header className="topbar shell">
          <div
            className="topbar-main"
            data-tauri-drag-region
            onMouseDown={handleDragStart}
            onDoubleClick={handleToggleMaximize}
          >
            <div>
              <h1>PS5 Upload</h1>
              <p>High-speed transfers, precision file control, and live console sync.</p>
            </div>
            <div className="chip-row">
              <div className="chip">Transfer: {status.transfer}</div>
              <div className="chip">Auto Resume</div>
              <div className="chip">LAN Ready</div>
            </div>
          </div>
          <div className="window-controls">
            <button
              className="window-btn"
              onClick={handleMinimize}
              aria-label="Minimize window"
              title="Minimize"
            >
              _
            </button>
            <button
              className="window-btn"
              onClick={handleToggleMaximize}
              aria-label="Toggle maximize"
              title="Maximize"
            >
              []
            </button>
            <button
              className="window-btn close"
              onClick={handleCloseWindow}
              aria-label="Close window"
              title="Close"
            >
              X
            </button>
          </div>
        </header>

        <nav className="tabs shell">
          {tabs.map((tab) => (
            <button
              key={tab.id}
              className={`tab ${activeTab === tab.id ? "active" : ""}`}
              onClick={() => setActiveTab(tab.id)}
            >
              {tab.label}
            </button>
          ))}
        </nav>

        <section className="content shell">
          {activeTab === "transfer" && (
            <div className="grid-two">
              <div className="card">
                <header className="card-title">Source</header>
                <label className="field">
                  <span>Folder</span>
                  <div className="inline-field">
                    <input
                      placeholder="/Users/you/PKG"
                      value={sourcePath}
                      onChange={(event) => setSourcePath(event.target.value)}
                    />
                    <button className="btn" onClick={handleBrowse}>
                      Browse
                    </button>
                  </div>
                </label>
                <label className="field">
                  <span>Detected Title</span>
                  <input placeholder="Not detected yet" disabled />
                </label>
                <div className="split">
                  <button className="btn" onClick={handleScan}>
                    Scan
                  </button>
                  <button
                    className={`btn ${optimizeUpload ? "primary" : ""}`}
                    onClick={() => setOptimizeUpload((prev) => !prev)}
                  >
                    Optimize
                  </button>
                </div>
                <label className="field">
                  <span>Bandwidth Limit (Mbps)</span>
                  <input
                    type="number"
                    min={0}
                    value={bandwidthLimit}
                    onChange={(event) => setBandwidthLimit(Number(event.target.value))}
                  />
                </label>
                <label className="field inline">
                  <span>Auto-tune Connections</span>
                  <input
                    type="checkbox"
                    checked={autoTune}
                    onChange={(event) => setAutoTune(event.target.checked)}
                  />
                </label>
                <label className="field inline">
                  <span>Use Temp Staging</span>
                  <input
                    type="checkbox"
                    checked={useTemp}
                    onChange={(event) => setUseTemp(event.target.checked)}
                  />
                </label>
              </div>

              <div className="card">
                <header className="card-title">Destination</header>
                <label className="field">
                  <span>Storage</span>
                  <select
                    value={storageRoot}
                    onChange={(event) => setStorageRoot(event.target.value)}
                  >
                    {storageLocations.length > 0 ? (
                      storageLocations.map((loc) => (
                        <option key={loc.path} value={loc.path}>
                          {loc.path} ({loc.free_gb.toFixed(1)} GB free)
                        </option>
                      ))
                    ) : (
                      <>
                        <option value="/data">/data</option>
                        <option value="/mnt/usb0">/mnt/usb0</option>
                      </>
                    )}
                  </select>
                </label>
                <label className="field">
                  <span>Preset</span>
                  <select
                    value={preset}
                    onChange={(event) =>
                      setPreset(event.target.value as (typeof presetOptions)[number])
                    }
                  >
                    {presetOptions.map((option) => (
                      <option key={option} value={option}>
                        {option}
                      </option>
                    ))}
                  </select>
                </label>
                {preset === "custom" && (
                  <label className="field">
                    <span>Custom Path</span>
                    <input
                      value={customPreset}
                      onChange={(event) => setCustomPreset(event.target.value)}
                      placeholder="homebrew/custom"
                    />
                  </label>
                )}
                <label className="field">
                  <span>Subfolder</span>
                  <input
                    placeholder="auto"
                    value={subfolder}
                    onChange={(event) => setSubfolder(event.target.value)}
                  />
                </label>
                <label className="field">
                  <span>Compression</span>
                  <select
                    value={compression}
                    onChange={(event) =>
                      setCompression(event.target.value as CompressionOption)
                    }
                  >
                    <option value="auto">Auto</option>
                    <option value="none">None</option>
                    <option value="lz4">LZ4</option>
                    <option value="zstd">Zstd</option>
                    <option value="lzma">LZMA</option>
                  </select>
                </label>
                <label className="field">
                  <span>Resolved Path</span>
                  <input value={destPath} readOnly />
                </label>
              </div>

              <div className="card wide">
                <header className="card-title">Transfer Control</header>
                <div className="progress">
                  <div
                    className="progress-fill"
                    style={{ width: `${transferPercent}%` }}
                  />
                </div>
                <div className="progress-meta">
                  <span>
                    {formatBytes(transferState.sent)} / {formatBytes(transferState.total)}
                  </span>
                  <span>{transferState.files} files</span>
                  <span>{transferState.status}</span>
                </div>
                {transferState.currentFile && (
                  <div className="pill">{transferState.currentFile}</div>
                )}
                <div className="split">
                  <button className="btn primary" onClick={handleUpload}>
                    Upload Current
                  </button>
                  <button className="btn" disabled>
                    Upload Queue
                  </button>
                  <button className="btn ghost" onClick={handleCancel}>
                    Stop
                  </button>
                </div>
              </div>
            </div>
          )}

          {activeTab === "manage" && (
            <div className="grid-two">
              <div className="card wide">
                <header className="card-title">Remote Browser</header>
                <p className="muted">Remote browsing is not implemented yet.</p>
              </div>

              <div className="card">
                <header className="card-title">Actions</header>
                <p className="muted">File actions are coming soon.</p>
              </div>
            </div>
          )}

          {activeTab === "chat" && (
            <div className="grid-two">
              <div className="card wide">
                <header className="card-title">Live Room</header>
                <p className="muted">Chat is not available in the web client yet.</p>
              </div>
              <div className="card">
                <header className="card-title">Room Stats</header>
                <p className="muted">Room stats will appear here when chat is enabled.</p>
              </div>
            </div>
          )}
        </section>
      </main>

      <aside className="rail shell">
        <section className="card">
          <header className="card-title">Queue</header>
          <p className="muted">No queued items yet.</p>
        </section>

        <section className="card">
          <header className="card-title">History</header>
          <p className="muted">No history yet.</p>
          <button className="btn ghost" disabled>
            Clear History
          </button>
        </section>

        <section className="card">
          <header className="card-title">Logs</header>
          <div className="log-window">
            {logs.length === 0 ? (
              <p>No logs yet.</p>
            ) : (
              logs.map((entry, index) => <p key={`${entry}-${index}`}>{entry}</p>)
            )}
          </div>
          <div className="split">
            <button className="btn" disabled>
              Client
            </button>
            <button className="btn" disabled>
              Payload
            </button>
          </div>
        </section>
      </aside>
    </div>
  );
}
