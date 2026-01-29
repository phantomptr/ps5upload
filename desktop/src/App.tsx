import { startTransition, useEffect, useMemo, useRef, useState } from "react";
import { open, save } from "./electron-api/dialog";
import { listen } from "./electron-api/event";
import { invoke } from "./electron-api/core";
import {
  getCurrentWindow,
  currentMonitor,
  LogicalPosition,
  LogicalSize
} from "./electron-api/window";
import { t } from "./i18n";

const appWindow = getCurrentWindow();
const openExternal = async (url: string) => {
  try {
    await invoke("open_external", { url });
  } catch {
    window.open(url, "_blank");
  }
};

const toSafeNumber = (value: unknown, fallback = 0) => {
  if (typeof value === "bigint") return Number(value);
  if (typeof value === "number") return Number.isFinite(value) ? value : fallback;
  return fallback;
};

const normalizeResumeMode = (value?: string | null): ResumeOption => {
  if (value === "sha256") return "sha256";
  if (value === "hash_large") return "hash_large";
  if (value === "hash_medium") return "hash_medium";
  if (value === "size" || value === "size_mtime") return "size";
  return "none";
};

type TabId = "transfer" | "payload" | "manage" | "chat";

type TransferCompleteEvent = {
  run_id: number;
  files: number;
  bytes: number;
};

type TransferErrorEvent = {
  run_id: number;
  message: string;
};

type TransferStatusSnapshot = {
  run_id: number;
  status: string;
  sent: number;
  total: number;
  files: number;
  elapsed_secs: number;
  current_file: string;
  requested_optimize?: boolean | null;
  auto_tune_connections?: boolean | null;
  effective_optimize?: boolean | null;
  effective_compression?: string | null;
};

type TransferLogEvent = {
  run_id: number;
  message: string;
};

type ManageProgressEvent = {
  op: string;
  processed: number;
  total: number;
  current_file?: string | null;
};

type ManageDoneEvent = {
  op: string;
  bytes?: number | null;
  files?: number | null;
  error?: string | null;
};

type ManageLogEvent = {
  message: string;
};

type ChatMessageEvent = {
  time: string;
  sender: string;
  text: string;
  local: boolean;
};

type ChatStatusEvent = {
  status: string;
};

type ChatAckEvent = {
  ok: boolean;
  reason?: string | null;
};

type ChatInfo = {
  room_id: string;
  enabled: boolean;
};

type StorageLocation = {
  path: string;
  storage_type: string;
  free_gb: number;
};

type ConnectionStatusSnapshot = {
  is_connected: boolean;
  status: string;
  storage_locations: StorageLocation[];
};

type DirEntry = {
  name: string;
  entry_type: string;
  size: number;
  mtime?: number | null;
};

type ManageSortKey = "name" | "size" | "modified";

type Profile = {
  name: string;
  address: string;
  storage: string;
  preset_index: number;
  custom_preset_path: string;
  connections: number;
  use_temp: boolean;
  auto_tune_connections: boolean;
  chat_display_name: string;
};

type ProfilesData = {
  profiles: Profile[];
  default_profile: string | null;
};

type QueueStatus = "Pending" | "InProgress" | "Completed" | { Failed: string };
const USER_STOPPED_SENTINEL = "__USER_STOPPED__";

type QueueItem = {
  id: number;
  source_path: string;
  subfolder_name: string;
  preset_index: number;
  custom_preset_path: string;
  storage_base?: string;
  dest_path?: string;
  ps5_ip?: string;
  status: QueueStatus;
  paused?: boolean;
  size_bytes?: number | null;
  attempts?: number;
  last_run_action?: "new" | "resume" | "requeue";
  last_started_at?: number;
  last_completed_at?: number;
  last_failed_at?: number;
  last_failed_bytes?: number;
  last_failed_total_bytes?: number;
  last_failed_files?: number;
  last_failed_elapsed_sec?: number;
  transfer_settings?: {
    use_temp: boolean;
    connections: number;
    resume_mode: ResumeOption;
    compression: CompressionOption;
    bandwidth_limit_mbps: number;
    auto_tune_connections: boolean;
    optimize_upload: boolean;
    chmod_after_upload?: boolean;
    rar_extract_mode: RarExtractMode;
    rar_temp_root?: string;
    override_on_conflict?: boolean;
  };
};

type QueueData = {
  items: QueueItem[];
  next_id: number;
  rev?: number;
  updated_at?: number;
};

type TransferRecord = {
  timestamp: number;
  source_path: string;
  dest_path: string;
  file_count: number;
  total_bytes: number;
  duration_secs: number;
  speed_bps: number;
  success: boolean;
  error?: string | null;
  via_queue?: boolean;
  game_meta?: GameMetaPayload | null;
  cover_url?: string | null;
};

type HistoryData = {
  records: TransferRecord[];
  rev?: number;
  updated_at?: number;
};

type ReleaseAsset = {
  name: string;
  browser_download_url: string;
};

type ReleaseInfo = {
  tag_name: string;
  html_url: string;
  assets: ReleaseAsset[];
  prerelease: boolean;
};

type PlatformInfo = {
  platform: string;
  arch: string;
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
  override_on_conflict: boolean;
  resume_mode: string;
  language: string;
  auto_tune_connections: boolean;
  auto_check_payload: boolean;
  payload_auto_reload: boolean;
  payload_reload_mode: string;
  payload_local_path: string;
  optimize_upload: boolean;
  chat_display_name: string;
  rar_extract_mode: string;
  rar_temp: string;
  window_width: number;
  window_height: number;
  window_x: number;
  window_y: number;
};

type ThemeMode = "dark" | "light";

const presetOptions = ["etaHEN/games", "homebrew", "custom"] as const;

type CompressionOption = "auto" | "none" | "lz4" | "zstd" | "lzma";

type ResumeOption = "none" | "size" | "hash_large" | "hash_medium" | "sha256";

type DownloadCompressionOption = "auto" | "none" | "lz4" | "zstd" | "lzma";

type OptimizeMode = "none" | "optimize" | "deep";

type RarExtractMode = "normal" | "safe" | "turbo";

type GameMetaPayload = {
  title: string;
  title_id: string;
  content_id: string;
  version: string;
};

type CoverPayload = {
  pixels: number[];
  width: number;
  height: number;
};

type PayloadProbeResult = {
  is_ps5upload: boolean;
  message: string;
};

type GameMetaResponse = {
  meta?: GameMetaPayload | null;
  cover?: CoverPayload | null;
};

type ManageAction = "Move" | "Copy" | "Extract";

type ManageListSnapshot = {
  path: string;
  entries: DirEntry[];
  error?: string | null;
  updated_at_ms?: number | null;
};

type ExtractQueueItem = {
  id: number;
  archive_name: string;
  source_path?: string;
  dest_path?: string;
  status: string;
  percent: number;
  processed_bytes: number;
  total_bytes: number;
  files_extracted: number;
  started_at: number;
  completed_at: number;
  error?: string | null;
};

type QueueHintEvent = {
  queue_id: number;
  source_path: string;
  dest_path: string;
  size_bytes?: number;
};

type QueueMeta = {
  source_path: string;
  dest_path: string;
  size_bytes?: number;
  game_meta?: GameMetaPayload | null;
  cover_url?: string | null;
};

type LogLevel = "debug" | "info" | "warn" | "error";

type LogEntry = {
  level: LogLevel;
  message: string;
  time: number;
};

const LOG_LEVEL_ORDER: Record<LogLevel, number> = {
  debug: 0,
  info: 1,
  warn: 2,
  error: 3
};

const inferLogLevel = (message: string): LogLevel => {
  const text = message.toLowerCase();
  if (
    text.includes("error") ||
    text.includes("failed") ||
    text.includes("failure") ||
    text.includes("exception")
  ) {
    return "error";
  }
  if (
    text.includes("warn") ||
    text.includes("cancel") ||
    text.includes("stalled") ||
    text.includes("disconnected") ||
    text.includes("not connected")
  ) {
    return "warn";
  }
  return "info";
};

type PayloadStatusResponse = {
  version: string;
  uptime: number;
  queue_count: number;
  is_busy: boolean;
  extract_last_progress?: number;
  system?: {
    cpu_percent: number;
    proc_cpu_percent?: number;
    rss_bytes: number;
    thread_count: number;
    mem_total_bytes: number;
    mem_free_bytes: number;
    page_size: number;
    net_rx_bytes?: number;
    net_tx_bytes?: number;
    net_rx_bps?: number;
    net_tx_bps?: number;
    cpu_supported?: boolean;
    proc_cpu_supported?: boolean;
    rss_supported?: boolean;
    thread_supported?: boolean;
    mem_total_supported?: boolean;
    mem_free_supported?: boolean;
    net_supported?: boolean;
  };
  transfer?: {
    pack_in_use: number;
    pool_count: number;
    queue_count: number;
    active_sessions: number;
    backpressure_events: number;
    backpressure_wait_ms: number;
    last_progress: number;
    abort_requested: boolean;
    workers_initialized: boolean;
  };
  items: ExtractQueueItem[];
};

type PayloadStatusSnapshot = {
  status?: PayloadStatusResponse | null;
  error?: string | null;
  updated_at_ms: number;
};

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
  requestedOptimize?: boolean | null;
  autoTuneConnections?: boolean | null;
  effectiveOptimize?: boolean | null;
  effectiveCompression?: string | null;
};

const formatBytes = (bytes: number | bigint) => {
  const value = typeof bytes === "bigint" ? Number(bytes) : bytes;
  const kb = 1024;
  const mb = kb * 1024;
  const gb = mb * 1024;
  if (value >= gb) return `${(value / gb).toFixed(2)} GB`;
  if (value >= mb) return `${(value / mb).toFixed(2)} MB`;
  if (value >= kb) return `${(value / kb).toFixed(2)} KB`;
  return `${value} B`;
};

const renderInlineMarkdown = (text: string, onLink: (url: string) => void, keyPrefix: string) => {
  const parts = text.split(/(`[^`]+`)/g);
  const nodes: Array<JSX.Element | string> = [];
  let nodeIndex = 0;

  const pushNode = (node: JSX.Element | string) => {
    nodes.push(
      <span key={`${keyPrefix}-${nodeIndex}`}>{node}</span>
    );
    nodeIndex += 1;
  };

  const renderLinksAndBold = (segment: string) => {
    const regex = /(\[([^\]]+)\]\(([^)]+)\))|(\*\*([^*]+)\*\*)/;
    let remaining = segment;
    while (remaining.length > 0) {
      const match = regex.exec(remaining);
      if (!match) {
        pushNode(remaining);
        break;
      }
      if (match.index > 0) {
        pushNode(remaining.slice(0, match.index));
      }
      if (match[1]) {
        const label = match[2];
        const url = match[3];
        pushNode(
          <a
            href={url}
            onClick={(event) => {
              event.preventDefault();
              onLink(url);
            }}
          >
            {label}
          </a>
        );
      } else if (match[4]) {
        pushNode(<strong>{match[5]}</strong>);
      }
      remaining = remaining.slice(match.index + match[0].length);
    }
  };

  parts.forEach((part) => {
    if (part.startsWith("`") && part.endsWith("`")) {
      pushNode(<code>{part.slice(1, -1)}</code>);
    } else if (part.length > 0) {
      renderLinksAndBold(part);
    }
  });

  return nodes;
};

const renderMarkdownBlocks = (markdown: string, onLink: (url: string) => void) => {
  const lines = markdown.replace(/\r\n/g, "\n").split("\n");
  const blocks: Array<JSX.Element> = [];
  let paragraph: string[] = [];
  let listItems: string[] = [];
  let listType: "ul" | "ol" | null = null;
  let inCode = false;
  let codeLines: string[] = [];

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    const text = paragraph.join(" ").trim();
    blocks.push(
      <p key={`p-${blocks.length}`}>
        {renderInlineMarkdown(text, onLink, `p-${blocks.length}`)}
      </p>
    );
    paragraph = [];
  };

  const flushList = () => {
    if (!listType || listItems.length === 0) return;
    const items = listItems.map((item, index) => (
      <li key={`li-${blocks.length}-${index}`}>
        {renderInlineMarkdown(item, onLink, `li-${blocks.length}-${index}`)}
      </li>
    ));
    blocks.push(
      listType === "ul" ? (
        <ul key={`ul-${blocks.length}`}>{items}</ul>
      ) : (
        <ol key={`ol-${blocks.length}`}>{items}</ol>
      )
    );
    listItems = [];
    listType = null;
  };

  const flushCode = () => {
    if (!inCode) return;
    blocks.push(
      <pre key={`code-${blocks.length}`}>
        <code>{codeLines.join("\n")}</code>
      </pre>
    );
    codeLines = [];
    inCode = false;
  };

  lines.forEach((line) => {
    if (line.trim().startsWith("```")) {
      flushParagraph();
      flushList();
      if (inCode) {
        flushCode();
      } else {
        inCode = true;
      }
      return;
    }
    if (inCode) {
      codeLines.push(line);
      return;
    }
    if (!line.trim()) {
      flushParagraph();
      flushList();
      return;
    }
    const headingMatch = line.match(/^(#{1,6})\s+(.*)$/);
    if (headingMatch) {
      flushParagraph();
      flushList();
      const level = headingMatch[1].length;
      const text = headingMatch[2].trim();
      const Tag = `h${level}` as keyof JSX.IntrinsicElements;
      blocks.push(
        <Tag key={`h-${blocks.length}`}>
          {renderInlineMarkdown(text, onLink, `h-${blocks.length}`)}
        </Tag>
      );
      return;
    }
    const orderedMatch = line.match(/^\s*\d+\.\s+(.*)$/);
    const unorderedMatch = line.match(/^\s*[-*]\s+(.*)$/);
    if (orderedMatch || unorderedMatch) {
      const itemText = (orderedMatch ? orderedMatch[1] : unorderedMatch?.[1] || "").trim();
      const nextType = orderedMatch ? "ol" : "ul";
      if (listType && listType !== nextType) {
        flushList();
      }
      listType = nextType;
      listItems.push(itemText);
      return;
    }
    paragraph.push(line.trim());
  });

  flushParagraph();
  flushList();
  if (inCode) {
    flushCode();
  }

  return blocks;
};

const joinRemote = (...parts: string[]) =>
  parts
    .filter(Boolean)
    .map((part, index) => {
      if (index === 0) return part.replace(/\/$/, "");
      return part.replace(/^\//, "").replace(/\/$/, "");
    })
    .join("/");

const FolderIcon = () => (
  <svg className="manage-icon" viewBox="0 0 24 24" aria-hidden="true">
    <path
      d="M3 6.5C3 5.67 3.67 5 4.5 5h5l2 2h8c.83 0 1.5.67 1.5 1.5v9c0 .83-.67 1.5-1.5 1.5h-15C3.67 19 3 18.33 3 17.5v-11Z"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinejoin="round"
    />
  </svg>
);

const FileIcon = () => (
  <svg className="manage-icon" viewBox="0 0 24 24" aria-hidden="true">
    <path
      d="M7 3.5h6.5L19 9v11.5c0 .83-.67 1.5-1.5 1.5h-10C6.67 22 6 21.33 6 20.5V5c0-.83.67-1.5 1-1.5Z"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinejoin="round"
    />
    <path
      d="M13.5 3.5V9H19"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinejoin="round"
    />
  </svg>
);

const ArchiveIcon = () => (
  <svg className="manage-icon" viewBox="0 0 24 24" aria-hidden="true">
    <rect
      x="4"
      y="6"
      width="16"
      height="14"
      rx="2"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
    />
    <path
      d="M4 9h16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
    />
    <path
      d="M11.5 12h1v5h-1z"
      fill="currentColor"
    />
    <path
      d="M11.5 10h1"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
    />
  </svg>
);

const isArchivePath = (value: string) => /\.(rar|zip|7z)$/i.test(value);

const normalizeVersion = (version: string) =>
  version.replace(/^v/i, "").trim();

const compareVersions = (latest: string, current: string) => {
  const a = normalizeVersion(latest)
    .split(".")
    .map((part) => Number(part));
  const b = normalizeVersion(current)
    .split(".")
    .map((part) => Number(part));
  const len = Math.max(a.length, b.length);
  for (let i = 0; i < len; i += 1) {
    const av = Number.isFinite(a[i]) ? a[i] : 0;
    const bv = Number.isFinite(b[i]) ? b[i] : 0;
    if (av > bv) return 1;
    if (av < bv) return -1;
  }
  return 0;
};

const isNewerVersion = (latest: string, current: string) =>
  compareVersions(latest, current) > 0;

const selectClientAsset = (
  assets: ReleaseAsset[],
  platformInfo: PlatformInfo | null
) => {
  if (!platformInfo) return null;
  const { platform, arch } = platformInfo;
  const platformKey =
    platform === "win32" ? "win" : platform === "darwin" ? "mac" : "linux";
  const ext = platformKey === "win" ? ".zip" : platformKey === "mac" ? ".dmg" : ".tar.gz";
  const exactSuffix = `-${platformKey}-${arch}${ext}`;
  const exact = assets.find((asset) => asset.name.endsWith(exactSuffix));
  if (exact) return exact;
  return assets.find(
    (asset) =>
      asset.name.includes(`-${platformKey}-`) && asset.name.endsWith(ext)
  ) || null;
};

const formatDuration = (seconds: number) => {
  if (!Number.isFinite(seconds) || seconds <= 0) return "0s";
  const mins = Math.floor(seconds / 60);
  const secs = Math.floor(seconds % 60);
  if (mins === 0) return `${secs}s`;
  return `${mins}m ${secs}s`;
};

const formatTimestamp = (value?: number | null) => {
  if (!value) return "-";
  const date = new Date(value * 1000);
  if (Number.isNaN(date.getTime())) return "-";
  return date.toLocaleString();
};

const getLeafName = (value?: string | null) => {
  if (!value) return "";
  return value.split(/[/\\]/).filter(Boolean).pop() || "";
};

const coverToDataUrl = (cover?: CoverPayload | null) => {
  if (!cover) return null;
  const canvas = document.createElement("canvas");
  canvas.width = cover.width;
  canvas.height = cover.height;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  const imageData = new ImageData(
    new Uint8ClampedArray(cover.pixels),
    cover.width,
    cover.height
  );
  ctx.putImageData(imageData, 0, 0);
  return canvas.toDataURL("image/png");
};

const getEntryType = (entry: DirEntry) => {
  if (typeof entry.entry_type === "string") return entry.entry_type.toLowerCase();
  const anyEntry = entry as unknown as { type?: unknown };
  if (typeof anyEntry.type === "string") return String(anyEntry.type).toLowerCase();
  return "";
};

const sortEntries = (
  entries: DirEntry[],
  sort: { key: ManageSortKey; direction: "asc" | "desc" }
) =>
  entries.slice().sort((a, b) => {
    const aDir = getEntryType(a) === "dir";
    const bDir = getEntryType(b) === "dir";
    if (aDir !== bDir) {
      return aDir ? -1 : 1;
    }
    if (sort.key === "size") {
      const aSize = typeof a.size === "number" ? a.size : 0;
      const bSize = typeof b.size === "number" ? b.size : 0;
      const diff = aSize - bSize;
      return sort.direction === "asc" ? diff : -diff;
    }
    if (sort.key === "modified") {
      const aTime = typeof a.mtime === "number" ? a.mtime : 0;
      const bTime = typeof b.mtime === "number" ? b.mtime : 0;
      const diff = aTime - bTime;
      return sort.direction === "asc" ? diff : -diff;
    }
    return a.name.localeCompare(b.name);
  });

export default function App() {
  const [activeTab, setActiveTab] = useState<TabId>("transfer");
  const [theme, setTheme] = useState<ThemeMode>("dark");
  const [windowSize, setWindowSize] = useState<{ width: number; height: number } | null>(
    null
  );
  const [windowPosition, setWindowPosition] = useState<{
    x: number;
    y: number;
  } | null>(null);

  const handleMinimize = async () => {
    await appWindow.minimize();
  };

  const handleToggleMaximize = async () => {
    await appWindow.toggleMaximize();
  };

  const handleCloseWindow = async () => {
    await appWindow.close();
  };

  const handleToggleKeepAwake = () => {
    const next =
      keepAwakeMode === "off" ? "on" : keepAwakeMode === "on" ? "auto" : "off";
    setKeepAwakeMode(next);
    localStorage.setItem("ps5upload.keep_awake_mode", next);
    if (next !== "auto") {
      localStorage.setItem("ps5upload.keep_awake", next === "on" ? "true" : "false");
    }
  };

  const handleToggleTheme = () => {
    setTheme((prev) => (prev === "dark" ? "light" : "dark"));
  };

  const KEEP_AWAKE_IDLE_MS = 15 * 60 * 1000;

  const [appVersion, setAppVersion] = useState("...");
  const [profilesData, setProfilesData] = useState<ProfilesData>({
    profiles: [],
    default_profile: null
  });
  const [currentProfile, setCurrentProfile] = useState<string | null>(null);
  const [profileSelectValue, setProfileSelectValue] = useState("");
  const [showProfileCreatePrompt, setShowProfileCreatePrompt] = useState(false);
  const [showProfileDeleteConfirm, setShowProfileDeleteConfirm] = useState(false);
  const [newProfileName, setNewProfileName] = useState("");
  const [queueData, setQueueData] = useState<QueueData>({ items: [], next_id: 1 });
  const [currentQueueItemId, setCurrentQueueItemId] = useState<number | null>(null);
  const [uploadQueueTab, setUploadQueueTab] = useState<"current" | "completed" | "failed">("current");
  const [extractQueueTab, setExtractQueueTab] = useState<"current" | "completed" | "failed">("current");
  const [extractionStopping, setExtractionStopping] = useState(false);
  const [extractionActionById, setExtractionActionById] = useState<Record<number, "requeue">>({});
  const uploadQueueRetryTimeoutRef = useRef<number | null>(null);
  const [historyData, setHistoryData] = useState<HistoryData>({ records: [] });
  const [logTab, setLogTab] = useState<"client" | "payload" | "history">("client");
  const [queueMetaById, setQueueMetaById] = useState<Record<number, QueueMeta>>({});
  const [queueChmodDone, setQueueChmodDone] = useState<Record<number, boolean>>({});
  const [clientLogs, setClientLogs] = useState<Array<LogEntry | string>>([]);
  const [payloadLogs, setPayloadLogs] = useState<Array<LogEntry | string>>([]);
  const [logLevel, setLogLevel] = useState<LogLevel>(() => {
    const saved = localStorage.getItem("ps5upload.log_level") as LogLevel | null;
    return saved && LOG_LEVEL_ORDER[saved] !== undefined ? saved : "info";
  });
  const [saveLogs, setSaveLogs] = useState(false);
  const clientLogBuffer = useRef<Array<LogEntry | string>>([]);
  const payloadLogBuffer = useRef<Array<LogEntry | string>>([]);
  const clientLogFlush = useRef<ReturnType<typeof setTimeout> | null>(null);
  const payloadLogFlush = useRef<ReturnType<typeof setTimeout> | null>(null);
  const configSaveRef = useRef<AppConfig | null>(null);
  const resizeSaveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const optimizeSnapshot = useRef<{
    connections: number;
    compression: CompressionOption;
    bandwidth: number;
    autoTune: boolean;
  } | null>(null);
  const lastScanLogAt = useRef(0);
  const [payloadLocalPath, setPayloadLocalPath] = useState("");
  const [payloadStatus, setPayloadStatus] = useState("Unknown");
  const [payloadVersion, setPayloadVersion] = useState<string | null>(null);
  const [payloadBusy, setPayloadBusy] = useState(false);
  const [payloadReloadCooldown, setPayloadReloadCooldown] = useState(false);
  const [updateInfo, setUpdateInfo] = useState<ReleaseInfo | null>(null);
  const [updateStatus, setUpdateStatus] = useState("Checking for updates...");
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const [updateDownloadStatus, setUpdateDownloadStatus] = useState("");
  const [updatePending, setUpdatePending] = useState(false);
  const [includePrerelease, setIncludePrerelease] = useState(false);
  const [platformInfo, setPlatformInfo] = useState<PlatformInfo | null>(null);
  const [language, setLanguage] = useState("en");
  const [langMenuOpen, setLangMenuOpen] = useState(false);
  const langMenuRef = useRef<HTMLDivElement | null>(null);
  const languages = [
    { code: "en", label: "English" },
    { code: "zh-CN", label: "简体中文" },
    { code: "zh-TW", label: "繁體中文" },
    { code: "hi", label: "हिन्दी" },
    { code: "es", label: "Español" },
    { code: "ar", label: "العربية" },
    { code: "bn", label: "বাংলা" },
    { code: "pt-BR", label: "Português (Brasil)" },
    { code: "ru", label: "Русский" },
    { code: "ja", label: "日本語" },
    { code: "de", label: "Deutsch" },
    { code: "fr", label: "Français" },
    { code: "ko", label: "한국어" },
    { code: "tr", label: "Türkçe" },
    { code: "vi", label: "Tiếng Việt" },
    { code: "id", label: "Bahasa Indonesia" },
    { code: "it", label: "Italiano" },
    { code: "th", label: "ไทย" }
  ];
  const currentLanguageLabel =
    languages.find((item) => item.code === language)?.label || "English";
  useEffect(() => {
    if (!platformInfo?.platform) return;
    document.documentElement.dataset.platform = platformInfo.platform;
  }, [platformInfo]);
  const [autoConnect, setAutoConnect] = useState(false);
  const [payloadAutoReload, setPayloadAutoReload] = useState(false);
  const [payloadReloadMode, setPayloadReloadMode] = useState<
    "local" | "current" | "latest"
  >("current");
  const [payloadProbe, setPayloadProbe] = useState<{
    ok: boolean;
    message: string;
  } | null>(null);
  const [payloadFullStatus, setPayloadFullStatus] = useState<PayloadStatusResponse | null>(null);
  const [payloadStatusLoading, setPayloadStatusLoading] = useState(false);
  const [payloadResetting, setPayloadResetting] = useState(false);
  const [payloadQueueLoading, setPayloadQueueLoading] = useState(false);
  const lastPayloadSyncAt = useRef(0);
  const lastPayloadSyncError = useRef<{ message: string; at: number } | null>(null);
  const [payloadStatusError, setPayloadStatusError] = useState<string | null>(null);
  const [payloadLastUpdated, setPayloadLastUpdated] = useState<number | null>(null);
  const [downloadCompression, setDownloadCompression] =
    useState<DownloadCompressionOption>("auto");
  const [chmodAfterUpload, setChmodAfterUpload] = useState(true);
  const [rarExtractMode, setRarExtractMode] = useState<RarExtractMode>("normal");
  const [rarTemp, setRarTemp] = useState("");
  const [chatDisplayName, setChatDisplayName] = useState("");
  const [configDefaults, setConfigDefaults] = useState<AppConfig | null>(null);
  const [configLoaded, setConfigLoaded] = useState(false);
  const [keepAwakeMode, setKeepAwakeMode] = useState<"off" | "on" | "auto">("off");
  const [keepAwakeAutoHold, setKeepAwakeAutoHold] = useState(false);
  const keepAwakeAutoTimeoutRef = useRef<number | null>(null);
  const [ip, setIp] = useState("");
  const [connectionStatus, setConnectionStatus] = useState("Disconnected");
  const [isConnected, setIsConnected] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);
  const [connectCooldown, setConnectCooldown] = useState(false);
  const [storageLocations, setStorageLocations] = useState<StorageLocation[]>([]);
  const [sourcePath, setSourcePath] = useState("");
  const [scanStatus, setScanStatus] = useState('idle');
  const [scanFiles, setScanFiles] = useState(0);
  const [scanTotal, setScanTotal] = useState(0);
  const [scanError, setScanError] = useState<string | null>(null);
  const [scanPartial, setScanPartial] = useState(false);
  const [scanPartialReason, setScanPartialReason] = useState<string | null>(null);
  const [scanEstimated, setScanEstimated] = useState(false);
  const [storageRoot, setStorageRoot] = useState("/data");
  const [preset, setPreset] = useState<(typeof presetOptions)[number]>(
    presetOptions[0]
  );
  const [customPreset, setCustomPreset] = useState("");
  const [subfolder, setSubfolder] = useState("");
  const [finalPathMode, setFinalPathMode] = useState<"auto" | "custom">("auto");
  const [finalPath, setFinalPath] = useState("");
  const [overrideOnConflict, setOverrideOnConflict] = useState(true);
  const [compression, setCompression] = useState<CompressionOption>("auto");
  const [resumeMode, setResumeMode] = useState<ResumeOption>("none");
  const [connections, setConnections] = useState(4);
  const [bandwidthLimit, setBandwidthLimit] = useState(0);
  const [optimizeMode, setOptimizeMode] = useState<OptimizeMode>("none");
  const optimizeActive = optimizeMode !== "none";
  const [scanMode, setScanMode] = useState<OptimizeMode | null>(null);
  const optimizePendingRef = useRef<OptimizeMode | null>(null);
  const [autoTune, setAutoTune] = useState(true);
  const [useTemp, setUseTemp] = useState(false);
  const [managePath, setManagePath] = useState("/data");
  const [manageEntries, setManageEntries] = useState<DirEntry[]>([]);
  const [manageStatus, setManageStatus] = useState("Not connected");
  const [manageSelected, setManageSelected] = useState<number | null>(null);
  const manageSelectionRef = useRef<{ name: string; type: string } | null>(null);
  const manageSelectedIndexRef = useRef<number | null>(null);
  const [manageSort, setManageSort] = useState<{
    key: ManageSortKey;
    direction: "asc" | "desc";
  }>({ key: "name", direction: "asc" });
  const [manageDestPath, setManageDestPath] = useState("/data");
  const [manageDestEntries, setManageDestEntries] = useState<DirEntry[]>([]);
  const [manageDestSelected, setManageDestSelected] = useState<number | null>(null);
  const [manageDestOpen, setManageDestOpen] = useState(false);
  const [manageDestAction, setManageDestAction] = useState<ManageAction | null>(null);
  const [manageDestStatus, setManageDestStatus] = useState("");
  const [manageDestFilename, setManageDestFilename] = useState("");
  const [noticeOpen, setNoticeOpen] = useState(false);
  const [noticeTitle, setNoticeTitle] = useState("");
  const [noticeLines, setNoticeLines] = useState<string[]>([]);
  const [manageBusy, setManageBusy] = useState(false);
  const [manageProgress, setManageProgress] = useState({
    op: "",
    processed: 0,
    total: 0,
    currentFile: "",
    speed_bps: 0
  });
  const [manageModalOpen, setManageModalOpen] = useState(false);
  const [manageModalDone, setManageModalDone] = useState(false);
  const [manageModalError, setManageModalError] = useState<string | null>(null);
  const [manageModalOp, setManageModalOp] = useState("");
  const [manageModalStatus, setManageModalStatus] = useState("");
  const [manageModalStartedAt, setManageModalStartedAt] = useState<number | null>(
    null
  );
  const manageModalStartedAtRef = useRef<number | null>(null);
  const [manageModalSummary, setManageModalSummary] = useState<string | null>(null);
  const [manageModalLastProgressAt, setManageModalLastProgressAt] = useState<number | null>(null);
  const manageModalOpRef = useRef("");
  const manageSpeedRef = useRef({
    op: "",
    processed: 0,
    at: 0,
    speed: 0
  });
  const manageLoadInFlight = useRef<Set<string>>(new Set());
  const [manageMeta, setManageMeta] = useState<GameMetaPayload | null>(null);
  const [manageCoverUrl, setManageCoverUrl] = useState<string | null>(null);
  const [manageLastUpdated, setManageLastUpdated] = useState<number | null>(null);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [showRenamePrompt, setShowRenamePrompt] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [showCreatePrompt, setShowCreatePrompt] = useState(false);
  const [newFolderName, setNewFolderName] = useState("");
  const [showResumePrompt, setShowResumePrompt] = useState(false);
  const [resumeRecord, setResumeRecord] = useState<TransferRecord | null>(null);
  const [resumeQueueItem, setResumeQueueItem] = useState<QueueItem | null>(null);
  const [resumeChoice, setResumeChoice] = useState<ResumeOption>("size");
  const [gameMeta, setGameMeta] = useState<GameMetaPayload | null>(null);
  const [gameCoverUrl, setGameCoverUrl] = useState<string | null>(null);
  const [chatRoomId, setChatRoomId] = useState("");
  const [chatEnabled, setChatEnabled] = useState(false);
  const [chatStatus, setChatStatus] = useState("Disconnected");
  const [chatMessages, setChatMessages] = useState<ChatMessageEvent[]>([]);
  const [chatInput, setChatInput] = useState("");
  const [chatStats, setChatStats] = useState({
    sent: 0,
    received: 0,
    acked: 0,
    rejected: 0
  });
  const [faqContent, setFaqContent] = useState("");
  const [faqLoading, setFaqLoading] = useState(true);
  const [faqError, setFaqError] = useState<string | null>(null);
  const NEW_PROFILE_OPTION = "__new_profile__";
  const trimmedNewProfileName = newProfileName.trim();
  const newProfileExists =
    trimmedNewProfileName.length > 0 &&
    (trimmedNewProfileName === NEW_PROFILE_OPTION ||
      profilesData.profiles.some((profile) => profile.name === trimmedNewProfileName));
  const isRtl = language === "ar";
  const tr = (key: string, vars?: Record<string, string | number>) => t(language, key, vars);
  const generateChatName = () => {
    try {
      if (crypto?.randomUUID) {
        return `user-${crypto.randomUUID()}`;
      }
    } catch {
      // Fallback below
    }
    const bytes = new Uint8Array(10);
    try {
      if (crypto?.getRandomValues) {
        crypto.getRandomValues(bytes);
      } else {
        throw new Error("No crypto");
      }
    } catch {
      for (let i = 0; i < bytes.length; i += 1) {
        bytes[i] = Math.floor(Math.random() * 256);
      }
    }
    const suffix = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    return `user-${suffix}`;
  };
  const chatStatusLower = chatStatus.toLowerCase();
  const isChatConnected = chatStatusLower === "connected";
  const isChatDisconnected = chatStatusLower === "disconnected";
  const payloadStatusLower = payloadStatus.toLowerCase();
  const payloadStatusClass =
    payloadStatusError ||
    payloadStatusLower.includes("error") ||
    payloadStatusLower.includes("not detected") ||
    payloadStatusLower.includes("closed")
      ? "error"
      : payloadStatusLower.includes("running") ||
        payloadStatusLower.includes("ready") ||
        payloadStatusLower.includes("sent") ||
        payloadStatusLower.includes("connected") ||
        payloadStatusLower.includes("idle") ||
        payloadStatusLower.includes("waiting")
      ? "ok"
      : "warn";
  const tabs = useMemo(
    () => [
      { id: "transfer" as TabId, label: tr("transfer"), icon: "↑" },
    { id: "payload" as TabId, label: tr("queues"), icon: "◎" },
      { id: "manage" as TabId, label: tr("manage"), icon: "≡" },
      { id: "chat" as TabId, label: tr("faq"), icon: "?" }
    ],
    [language]
  );

  useEffect(() => {
    document.documentElement.dir = isRtl ? "rtl" : "ltr";
    document.documentElement.lang = language;
  }, [isRtl, language]);

  useEffect(() => {
    if (!langMenuOpen) return;
    const handleClick = (event: MouseEvent) => {
      if (!langMenuRef.current) return;
      if (!langMenuRef.current.contains(event.target as Node)) {
        setLangMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [langMenuOpen]);
  const [transferState, setTransferState] = useState<TransferState>({
    status: "Idle",
    sent: 0,
    total: 0,
    files: 0,
    elapsed: 0,
    currentFile: "",
    requestedOptimize: null,
    autoTuneConnections: null,
    effectiveOptimize: null,
    effectiveCompression: null
  });
  const transferSpeedRef = useRef({ sent: 0, elapsed: 0, ema: 0 });
  const [transferSpeedEma, setTransferSpeedEma] = useState(0);
  const [transferUpdatedAt, setTransferUpdatedAt] = useState<number | null>(null);
  const [activeRunId, setActiveRunId] = useState<number | null>(null);
  const [transferActive, setTransferActive] = useState(false);
  const [uploadQueueRunning, setUploadQueueRunning] = useState(false);
  const lastManageProgressUpdate = useRef(0);
  const lastManageOp = useRef("");
  const [activeTransferSource, setActiveTransferSource] = useState("");
  const [activeTransferDest, setActiveTransferDest] = useState("");
  const [activeTransferViaQueue, setActiveTransferViaQueue] = useState(false);
  const [transferStartedAt, setTransferStartedAt] = useState<number | null>(null);
  const [uploadInfoItem, setUploadInfoItem] = useState<QueueItem | null>(null);
  const maintenanceRequestedRef = useRef(false);
  const lastMaintenanceAtRef = useRef(0);
  const extractionStopAttemptsRef = useRef(0);
  const transferSnapshot = useRef({
    runId: null as number | null,
    source: "",
    dest: "",
    viaQueue: false,
    startedAt: null as number | null,
    gameMeta: null as GameMetaPayload | null,
    coverUrl: null as string | null,
    state: {
      status: "Idle",
      sent: 0,
      total: 0,
      files: 0,
      elapsed: 0,
      currentFile: ""
    }
  });
  const queueSnapshot = useRef({
    data: queueData,
    currentId: currentQueueItemId
  });
  const uploadQueueStatusRef = useRef<Record<number, QueueStatus>>({});
  const extractQueueStatusRef = useRef<Record<number, { status: string; error?: string | null }>>({});
  const manageSnapshot = useRef({
    ip: "",
    path: "/data"
  });
  const managePathRef = useRef(managePath);

  const getQueueDisplayName = (item: ExtractQueueItem) => {
    const meta = queueMetaById[item.id];
    return (
      meta?.game_meta?.title ||
      getLeafName(item.dest_path || meta?.dest_path) ||
      getLeafName(item.source_path || meta?.source_path) ||
      item.archive_name
    );
  };
  managePathRef.current = managePath;
  useEffect(() => {
    manageSelectedIndexRef.current = manageSelected;
  }, [manageSelected]);
  const configSnapshot = useRef({
    ip: "",
    chmodAfterUpload: false
  });
  const languageRef = useRef(language);
  const chatEndRef = useRef<HTMLDivElement | null>(null);

  const destBase = preset === "custom" ? customPreset : preset;
  const defaultDestPath = useMemo(
    () => joinRemote(storageRoot, destBase, subfolder),
    [storageRoot, destBase, subfolder]
  );
  const finalDestPath = finalPathMode === "custom" ? finalPath : defaultDestPath;
  const manageSelectedEntry = useMemo(() => {
    if (manageSelected === null) return null;
    return manageEntries[manageSelected] ?? null;
  }, [manageSelected, manageEntries]);
  const manageDestPreview = useMemo(() => {
    if (manageDestSelected === null) {
      return manageDestPath;
    }
    const entry = manageDestEntries[manageDestSelected];
    if (!entry) return manageDestPath;
    return joinRemote(manageDestPath, entry.name);
  }, [manageDestSelected, manageDestEntries, manageDestPath]);

  useEffect(() => {
    const stored = localStorage.getItem("ps5upload.save_logs");
    if (stored === "true") {
      setSaveLogs(true);
    }
  }, []);

  useEffect(() => {
    if (!activeRunId && !transferActive) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const poll = async () => {
      if (cancelled) return;
      try {
        const status = await invoke<TransferStatusSnapshot>("transfer_status");
          if (!activeRunId && status.run_id) {
            setActiveRunId(status.run_id);
          }
          if (!activeRunId || status.run_id === activeRunId) {
            const isTerminal =
              status.status.startsWith("Complete") ||
              status.status.startsWith("Cancelled") ||
              status.status.startsWith("Error") ||
              status.status.startsWith("Queued for extraction");
            const nextState = {
              status: status.status,
              sent: toSafeNumber(status.sent),
              total: toSafeNumber(status.total),
              files: toSafeNumber(status.files),
              elapsed: toSafeNumber(status.elapsed_secs),
              currentFile: status.current_file ?? "",
              requestedOptimize: status.requested_optimize ?? null,
              autoTuneConnections: status.auto_tune_connections ?? null,
              effectiveOptimize: status.effective_optimize ?? null,
              effectiveCompression: status.effective_compression ?? null
            };
            startTransition(() => {
              setTransferState((prev) => {
                if (
                  prev.status === nextState.status &&
                  prev.sent === nextState.sent &&
                  prev.total === nextState.total &&
                  prev.files === nextState.files &&
                  prev.elapsed === nextState.elapsed &&
                  prev.currentFile === nextState.currentFile &&
                  prev.requestedOptimize === nextState.requestedOptimize &&
                  prev.autoTuneConnections === nextState.autoTuneConnections &&
                  prev.effectiveOptimize === nextState.effectiveOptimize &&
                  prev.effectiveCompression === nextState.effectiveCompression
                ) {
                  return prev;
                }
                return { ...prev, ...nextState };
              });
            });
            if (isTerminal) {
              if (
                status.status.startsWith("Queued for extraction") &&
                activeTransferViaQueue &&
                currentQueueItemId
              ) {
                updateQueueItemStatus(currentQueueItemId, "Completed").then(() => {
                  processNextQueueItem();
                });
              }
              setActiveRunId(null);
              setActiveTransferSource("");
              setActiveTransferDest("");
              setTransferStartedAt(null);
            }
          }
      } catch {
        // ignore polling failures
      }
      if (!cancelled) {
        timer = setTimeout(poll, 500);
      }
    };
    poll();
    return () => {
      cancelled = true;
      if (timer) {
        clearTimeout(timer);
      }
    };
  }, [activeRunId, transferActive]);

  useEffect(() => {
    if (!activeRunId && !transferActive) return;
    const now = Date.now();
    setTransferUpdatedAt(now);
    const sent = toSafeNumber(transferState.sent, 0);
    const nowElapsed =
      transferState.elapsed > 0
        ? transferState.elapsed
        : transferStartedAt
        ? (Date.now() - transferStartedAt) / 1000
        : 0;
    const prev = transferSpeedRef.current;
    if (!prev.elapsed || sent < prev.sent || transferState.status.startsWith("Idle")) {
      transferSpeedRef.current = { sent, elapsed: nowElapsed, ema: 0 };
      setTransferSpeedEma(0);
      return;
    }
    const elapsedDelta = nowElapsed - prev.elapsed;
    if (elapsedDelta <= 0) return;
    if (elapsedDelta < 0.5) return;
    const delta = sent - prev.sent;
    const inst = delta > 0 ? delta / elapsedDelta : 0;
    const tau = 3;
    const alpha = 1 - Math.exp(-elapsedDelta / tau);
    const nextEma = prev.ema > 0 ? prev.ema + (inst - prev.ema) * alpha : inst;
    transferSpeedRef.current = { sent, elapsed: nowElapsed, ema: nextEma };
    setTransferSpeedEma(nextEma);
  }, [transferState.sent, transferState.status, transferActive, activeRunId]);

  useEffect(() => {
    if (!isConnected || !ip.trim()) {
      setTransferActive(false);
      return;
    }
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const poll = async () => {
      if (cancelled) return;
      try {
        const active = await invoke<boolean>("transfer_active");
        if (!cancelled) {
          setTransferActive(active);
        }
      } catch {
        // ignore polling failures
      }
      if (!cancelled) {
        timer = setTimeout(poll, 500);
      }
    };
    poll();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [isConnected, ip]);

  useEffect(() => {
    if (!activeRunId || transferActive) return;
    if (transferStartedAt && Date.now() - transferStartedAt < 2000) return;
    setActiveRunId(null);
    setActiveTransferSource("");
    setActiveTransferDest("");
    setTransferStartedAt(null);
  }, [activeRunId, transferActive, transferStartedAt]);

  const queueAdvanceInFlight = useRef(false);
  useEffect(() => {
    if (!uploadQueueRunning) return;
    if (transferActive || currentQueueItemId) return;
    if (queueAdvanceInFlight.current) return;
    queueAdvanceInFlight.current = true;
    processNextQueueItem().finally(() => {
      queueAdvanceInFlight.current = false;
    });
  }, [uploadQueueRunning, transferActive, currentQueueItemId]);

  useEffect(() => {
    localStorage.setItem("ps5upload.save_logs", saveLogs ? "true" : "false");
    invoke("set_save_logs", { enabled: saveLogs }).catch(() => {
      // ignore log toggle failures
    });
  }, [saveLogs]);

  useEffect(() => {
    const enabled = !transferActive;
    invoke("set_ui_log_enabled", { enabled }).catch(() => {
      // ignore log toggle failures
    });
  }, [transferActive]);

  useEffect(() => {
    transferSnapshot.current = {
      runId: activeRunId,
      source: activeTransferSource,
      dest: activeTransferDest,
      viaQueue: activeTransferViaQueue,
      startedAt: transferStartedAt,
      gameMeta,
      coverUrl: gameCoverUrl,
      state: transferState
    };
  }, [
    activeRunId,
    activeTransferSource,
    activeTransferDest,
    activeTransferViaQueue,
    transferStartedAt,
    transferState,
    gameMeta,
    gameCoverUrl
  ]);

  useEffect(() => {
    if (!isConnected || !ip.trim()) return;
    syncPayloadState();
  }, [isConnected, ip, queueData.updated_at, historyData.updated_at]);

  useEffect(() => {
    queueSnapshot.current = {
      data: queueData,
      currentId: currentQueueItemId
    };
  }, [queueData, currentQueueItemId]);

  const queueNormalized = useRef(false);
  useEffect(() => {
    if (queueNormalized.current) return;
    if (transferActive) return;
    const hasInProgress = queueData.items.some((item) => item.status === "InProgress");
    if (!hasInProgress) {
      queueNormalized.current = true;
      return;
    }
    const nextItems = queueData.items.map((item) => {
      if (item.status !== "InProgress") return item;
      return {
        ...item,
        paused: true,
        status: "Pending",
        transfer_settings: {
          ...(item.transfer_settings || {}),
          resume_mode: "size"
        }
      };
    });
    saveQueueData({ ...queueData, items: nextItems }).then(() => {
      setCurrentQueueItemId(null);
      queueNormalized.current = true;
    });
  }, [queueData, transferActive]);

  useEffect(() => {
    manageSnapshot.current = { ip, path: managePath };
  }, [ip, managePath]);

  useEffect(() => {
    configSnapshot.current = { ip, chmodAfterUpload };
  }, [ip, chmodAfterUpload]);

  useEffect(() => {
    clientLogBuffer.current = clientLogs;
  }, [clientLogs]);

  useEffect(() => {
    languageRef.current = language;
  }, [language]);

  useEffect(() => {
    payloadLogBuffer.current = payloadLogs;
  }, [payloadLogs]);

  useEffect(() => {
    localStorage.setItem("ps5upload.log_level", logLevel);
  }, [logLevel]);

  useEffect(() => {
    let active = true;
    const loadFaq = async () => {
      setFaqLoading(true);
      setFaqError(null);
      try {
        const content = await invoke<string>("faq_load");
        if (!active) return;
        setFaqContent(content);
      } catch (err) {
        if (!active) return;
        setFaqError(String(err));
      } finally {
        if (active) setFaqLoading(false);
      }
    };
    loadFaq();
    return () => {
      active = false;
    };
  }, []);

  const pushClientLog = (message: string, level: LogLevel = "info") => {
    setClientLogs((prev) => [
      { level, message, time: Date.now() },
      ...prev
    ].slice(0, 200));
  };

  const pushPayloadLog = (message: string, level: LogLevel = "info") => {
    setPayloadLogs((prev) => [
      { level, message, time: Date.now() },
      ...prev
    ].slice(0, 200));
  };

  const pushClientLogs = (messages: string[], level: LogLevel = "info") => {
    const now = Date.now();
    setClientLogs((prev) => [
      ...messages.map((message, index) => ({
        level,
        message,
        time: now + index
      })),
      ...prev
    ].slice(0, 200));
  };

  const pushPayloadLogs = (messages: string[], level: LogLevel = "info") => {
    const now = Date.now();
    setPayloadLogs((prev) => [
      ...messages.map((message, index) => ({
        level,
        message,
        time: now + index
      })),
      ...prev
    ].slice(0, 200));
  };

  const normalizeLogEntry = (entry: LogEntry | string): LogEntry => {
    if (typeof entry === "string") {
      return { level: inferLogLevel(entry), message: entry, time: Date.now() };
    }
    return entry;
  };

  useEffect(() => {
    if (!payloadLocalPath.trim()) {
      setPayloadProbe(null);
      return;
    }
    let active = true;
    invoke<PayloadProbeResult>("payload_probe", { path: payloadLocalPath })
      .then((result) => {
        if (!active) return;
        setPayloadProbe({
          ok: result.is_ps5upload,
          message: result.message
        });
      })
      .catch((err) => {
        if (!active) return;
        setPayloadProbe({
          ok: false,
          message: `Error: ${String(err)}`
        });
      });
    return () => {
      active = false;
    };
  }, [payloadLocalPath]);

  useEffect(() => {
    if (!payloadFullStatus?.items?.length) return;
    let cancelled = false;
    const pending = payloadFullStatus.items.filter(
      (item) => item.source_path && !queueMetaById[item.id]
    );
    if (pending.length === 0) return;
    const loadMeta = async () => {
      for (const item of pending) {
        if (cancelled) return;
        const sourcePath = item.source_path || "";
        if (!sourcePath) continue;
        try {
          const response = await invoke<GameMetaResponse>("game_meta_load", { path: sourcePath });
          if (cancelled) return;
          setQueueMetaById((prev) => ({
            ...prev,
            [item.id]: {
              ...(prev[item.id] || {}),
              source_path: sourcePath,
              dest_path: item.dest_path || prev[item.id]?.dest_path || "",
              game_meta: response.meta ?? null,
              cover_url: coverToDataUrl(response.cover)
            }
          }));
        } catch {
          // ignore metadata failures
        }
      }
    };
    loadMeta();
    return () => {
      cancelled = true;
    };
  }, [payloadFullStatus, queueMetaById]);

  useEffect(() => {
    if (chatEndRef.current) {
      chatEndRef.current.scrollIntoView({ behavior: "smooth" });
    }
  }, [chatMessages]);

  const applyProfile = (profile: Profile) => {
    setIp(profile.address);
    setStorageRoot(profile.storage || "/data");
    setConnections(profile.connections || 1);
    setUseTemp(profile.use_temp ?? false);
    setAutoTune(profile.auto_tune_connections ?? true);
    const resolvedChatName = profile.chat_display_name?.trim();
    setChatDisplayName(resolvedChatName ? resolvedChatName : generateChatName());
    const nextPreset = presetOptions[profile.preset_index] ?? presetOptions[0];
    setPreset(nextPreset);
    setCustomPreset(profile.custom_preset_path || "");
  };

  const persistProfiles = async (data: ProfilesData) => {
    setProfilesData(data);
    try {
      localStorage.setItem("ps5upload.profiles_backup", JSON.stringify(data));
    } catch {
      // ignore storage failures
    }
    try {
      await invoke("profiles_update", { data });
    } catch (err) {
      setClientLogs((prev) => [
        `Failed to save profiles: ${String(err)}`,
        ...prev
      ]);
    }
  };

  const buildProfileFromState = (name: string): Profile => {
    const presetIndex = presetOptions.indexOf(preset);
    return {
      name,
      address: ip,
      storage: storageRoot,
      preset_index: presetIndex >= 0 ? presetIndex : 0,
      custom_preset_path: customPreset,
      connections,
      use_temp: useTemp,
      auto_tune_connections: autoTune,
      chat_display_name: chatDisplayName
    };
  };

  const profilesMatch = (a: Profile, b: Profile) =>
    a.address === b.address &&
    a.storage === b.storage &&
    a.preset_index === b.preset_index &&
    a.custom_preset_path === b.custom_preset_path &&
    a.connections === b.connections &&
    a.use_temp === b.use_temp &&
    a.auto_tune_connections === b.auto_tune_connections &&
    a.chat_display_name === b.chat_display_name;

  const handleSelectProfile = async (name: string | null) => {
    if (name === NEW_PROFILE_OPTION) {
      setProfileSelectValue(NEW_PROFILE_OPTION);
      setNewProfileName("");
      setShowProfileCreatePrompt(true);
      return;
    }
    setCurrentProfile(name);
    setProfileSelectValue(name ?? "");
    if (!name) {
      await persistProfiles({ ...profilesData, default_profile: null });
      return;
    }
    const profile = profilesData.profiles.find((item) => item.name === name);
    if (profile) {
      const chatName = profile.chat_display_name?.trim();
      if (!chatName) {
        const generated = generateChatName();
        const resolvedProfile = { ...profile, chat_display_name: generated };
        const nextProfiles = profilesData.profiles.map((item) =>
          item.name === name ? resolvedProfile : item
        );
        applyProfile(resolvedProfile);
        await persistProfiles({ profiles: nextProfiles, default_profile: name });
        return;
      }
      applyProfile(profile);
      await persistProfiles({ ...profilesData, default_profile: name });
    }
  };

  const handleBrowse = async () => {
    if (scanStatus === "scanning") return;
    try {
      const selected = await open({
        directory: true,
        multiple: false
      });
      if (typeof selected === "string") {
        setSourcePath(selected);
        resetScan();
      }
    } catch (err) {
      setClientLogs((prev) => [`Browse failed: ${String(err)}`, ...prev]);
    }
  };

  const handleBrowseArchive = async () => {
    if (scanStatus === "scanning") return;
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "Archives", extensions: ["zip", "rar", "7z"] }]
      });
      if (typeof selected === "string") {
        setSourcePath(selected);
        resetScan();
        if (!subfolder) {
          const name = selected.split(/[/\\]/).filter(Boolean).pop() ?? "";
          const base = name.replace(/\.(zip|7z|rar)$/i, "");
          if (base) {
            setSubfolder(base);
          }
        }
      }
    } catch (err) {
      setClientLogs((prev) => [`Browse failed: ${String(err)}`, ...prev]);
    }
  };

  const optimizeSampleLimitMs = 10000;
  const optimizeSampleLimitFiles = 50000;

  const restoreOptimizeSnapshot = () => {
    if (!optimizeSnapshot.current) return;
    setConnections(optimizeSnapshot.current.connections);
    setCompression(optimizeSnapshot.current.compression);
    setBandwidthLimit(optimizeSnapshot.current.bandwidth);
    setAutoTune(optimizeSnapshot.current.autoTune);
    optimizeSnapshot.current = null;
  };

  const runOptimizeScan = async (mode: OptimizeMode) => {
    if (!sourcePath.trim()) return;
    if (isArchiveSource) {
      setScanStatus("error");
      setScanError(tr("scan_archive_not_supported"));
      optimizePendingRef.current = null;
      setOptimizeMode("none");
      setScanMode(null);
      restoreOptimizeSnapshot();
      return;
    }
    setClientLogs((prev) => [
      `${mode === "deep" ? "Deep optimize" : "Optimize"} scan started: ${sourcePath}`,
      ...prev
    ].slice(0, 100));
    setScanMode(mode);
    setScanStatus('scanning');
    setScanError(null);
    setScanFiles(0);
    setScanTotal(0);
    setScanPartial(false);
    setScanPartialReason(null);
    setScanEstimated(false);
    try {
      await invoke("transfer_scan", {
        source_path: sourcePath,
        max_ms: mode === "deep" ? 0 : optimizeSampleLimitMs,
        max_files: mode === "deep" ? 0 : optimizeSampleLimitFiles,
        quick_count: mode !== "deep",
        sample_limit: mode === "deep" ? 0 : 1200
      });
    } catch (err) {
      setScanStatus('error');
      const message = err instanceof Error ? err.message : String(err);
      setScanError(message);
      setClientLogs((prev) => [`Scan error: ${message}`, ...prev].slice(0, 100));
      optimizePendingRef.current = null;
      setOptimizeMode("none");
      setScanMode(null);
      restoreOptimizeSnapshot();
    }
  };

  const handleOptimizeMode = async (mode: OptimizeMode) => {
    if (mode === "none") return;
    if (scanStatus === 'scanning') {
      if (optimizePendingRef.current === mode) {
        await handleCancelScan();
      }
      return;
    }
    if (optimizeMode === mode) {
      resetScan();
      return;
    }
    if (!optimizeSnapshot.current) {
      optimizeSnapshot.current = {
        connections,
        compression,
        bandwidth: bandwidthLimit,
        autoTune
      };
    }
    setOptimizeMode(mode);
    optimizePendingRef.current = mode;
    await runOptimizeScan(mode);
  };

  const handleCancelScan = async () => {
    if (scanStatus !== 'scanning') return;
    await invoke("transfer_scan_cancel");
    setScanStatus('cancelled');
    setScanError(null);
    setClientLogs((prev) => ["Scan cancelled", ...prev].slice(0, 100));
    optimizePendingRef.current = null;
    setOptimizeMode("none");
    setScanMode(null);
    restoreOptimizeSnapshot();
  };

  const resetScan = () => {
    setScanStatus('idle');
    setScanFiles(0);
    setScanTotal(0);
    setScanError(null);
    setScanPartial(false);
    setScanPartialReason(null);
    setScanEstimated(false);
    setScanMode(null);
    optimizePendingRef.current = null;
    setOptimizeMode("none");
    restoreOptimizeSnapshot();
  };

  const computeOptimizeSettings = () => {
    if (scanFiles <= 0 || scanTotal <= 0) {
      return { connections: 2, compression: "lz4" as CompressionOption, bandwidth: 0 };
    }
    const kb = 1024;
    const mb = 1024 * 1024;
    const avgSize = scanTotal / scanFiles;
    let connections = 4;
    let compression: CompressionOption = "lz4";

    if (avgSize < 256 * kb || scanFiles >= 100000) {
      connections = 1;
      compression = "lz4";
    } else if (avgSize < 1 * mb) {
      connections = 2;
      compression = "lz4";
    } else if (avgSize < 8 * mb) {
      connections = 4;
      compression = "lz4";
    } else if (avgSize < 64 * mb) {
      connections = 6;
      compression = "none";
    } else {
      connections = 8;
      compression = "none";
    }

    return { connections, compression, bandwidth: 0 };
  };

  const applyOptimizeSettings = () => {
    const nextSettings = computeOptimizeSettings();
    setAutoTune(false);
    setConnections(nextSettings.connections);
    setCompression(nextSettings.compression);
    setBandwidthLimit(nextSettings.bandwidth);
    const tag = scanPartial ? " (sampled)" : "";
    const label = optimizeMode === "deep" ? "Deep optimize" : "Optimize";
    setClientLogs((prev) => [
      `${label}${tag}: connections=${nextSettings.connections}, compression=${nextSettings.compression}, bandwidth=${nextSettings.bandwidth === 0 ? "unlimited" : `${nextSettings.bandwidth} Mbps`}`,
      ...prev
    ].slice(0, 100));
  };

  const handleSaveCurrentProfile = async () => {
    if (!currentProfile) return;
    const nextProfile = buildProfileFromState(currentProfile);
    const nextProfiles = profilesData.profiles.map((item) =>
      item.name === currentProfile ? nextProfile : item
    );
    const nextData: ProfilesData = {
      profiles: nextProfiles,
      default_profile: currentProfile
    };
    await persistProfiles(nextData);
  };

  const handleCreateProfile = async () => {
    const name = trimmedNewProfileName;
    if (!name || newProfileExists) return;
    const nextProfile = buildProfileFromState(name);
    const nextData: ProfilesData = {
      profiles: [...profilesData.profiles, nextProfile],
      default_profile: name
    };
    setNewProfileName("");
    setCurrentProfile(name);
    setProfileSelectValue(name);
    await persistProfiles(nextData);
  };

  const handleDeleteProfile = async () => {
    if (!currentProfile) return;
    setShowProfileDeleteConfirm(true);
  };

  const handleConfirmDeleteProfile = async () => {
    if (!currentProfile) return;
    setShowProfileDeleteConfirm(false);
    const nextProfiles = profilesData.profiles.filter(
      (item) => item.name !== currentProfile
    );
    const nextDefault = nextProfiles[0]?.name ?? null;
    const nextData: ProfilesData = {
      profiles: nextProfiles,
      default_profile: nextDefault
    };
    setCurrentProfile(nextDefault);
    setProfileSelectValue(nextDefault ?? "");
    if (nextDefault) {
      const nextProfile = nextProfiles.find((item) => item.name === nextDefault);
      if (nextProfile) {
        applyProfile(nextProfile);
      }
    }
    await persistProfiles(nextData);
  };

  useEffect(() => {
    let active = true;
    const load = async () => {
      // Load app version
      try {
        const version = await invoke<string>("app_version");
        if (!active) return;
        setAppVersion(version);
      } catch (err) {
        setClientLogs((prev) => [`Failed to load app version: ${String(err)}`, ...prev]);
      }

      // Load config (settings) - independent
      try {
        const cfg = await invoke<AppConfig>("config_load");
        if (!active) return;
        const safeWindowX = typeof cfg.window_x === "number" ? cfg.window_x : 0;
        const safeWindowY = typeof cfg.window_y === "number" ? cfg.window_y : 0;
        const normalizedConfig: AppConfig = {
          ...cfg,
          window_x: safeWindowX,
          window_y: safeWindowY
        };
        setConfigDefaults(normalizedConfig);
        setTheme(normalizedConfig.theme === "light" ? "light" : "dark");
        try {
          const innerSize = await appWindow.innerSize();
          const monitor = await currentMonitor();
          const workArea = monitor?.workArea?.size ?? monitor?.size ?? innerSize;
          const width =
            normalizedConfig.window_width > 0
              ? Math.min(normalizedConfig.window_width, workArea.width)
              : workArea.width;
          const height =
            normalizedConfig.window_height > 0
              ? Math.min(normalizedConfig.window_height, workArea.height)
              : workArea.height;
          setWindowSize({ width, height });
          await appWindow.setSize(new LogicalSize(width, height));
          if (safeWindowX !== 0 || safeWindowY !== 0) {
            const x = Math.max(0, Math.min(safeWindowX, workArea.width - width));
            const y = Math.max(0, Math.min(safeWindowY, workArea.height - height));
            setWindowPosition({ x, y });
            await appWindow.setPosition(new LogicalPosition(x, y));
          }
        } catch (windowErr) {
          // Window positioning failed, continue with other config
          setClientLogs((prev) => [`Window setup failed: ${String(windowErr)}`, ...prev]);
        }
        setIp(normalizedConfig.address ?? "");
        setStorageRoot(normalizedConfig.storage || "/data");
        setConnections(normalizedConfig.connections || 4);
        setCompression((normalizedConfig.compression as CompressionOption) || "auto");
        setBandwidthLimit(normalizedConfig.bandwidth_limit_mbps || 0);
        setOverrideOnConflict(normalizedConfig.override_on_conflict ?? false);
        setResumeMode(normalizeResumeMode(normalizedConfig.resume_mode));
        setAutoTune(normalizedConfig.auto_tune_connections ?? true);
        setOptimizeMode(normalizedConfig.optimize_upload ? "optimize" : "none");
        setUseTemp(normalizedConfig.use_temp ?? false);
        setAutoConnect(normalizedConfig.auto_connect ?? false);
        setPayloadAutoReload(
          normalizedConfig.payload_auto_reload ?? normalizedConfig.auto_check_payload ?? false
        );
        const reloadMode =
          normalizedConfig.payload_reload_mode === "local" ||
          normalizedConfig.payload_reload_mode === "current" ||
          normalizedConfig.payload_reload_mode === "latest"
            ? normalizedConfig.payload_reload_mode
            : "current";
        setPayloadReloadMode(reloadMode);
        setPayloadLocalPath(normalizedConfig.payload_local_path ?? "");
        setDownloadCompression(
          (normalizedConfig.download_compression as DownloadCompressionOption) || "auto"
        );
        setChmodAfterUpload(normalizedConfig.chmod_after_upload ?? false);
        setRarExtractMode((normalizedConfig.rar_extract_mode as RarExtractMode) || "normal");
        setRarTemp(normalizedConfig.rar_temp || "");
        setChatDisplayName(normalizedConfig.chat_display_name ?? "");
        setIncludePrerelease(normalizedConfig.update_channel === "all");
        setLanguage(normalizedConfig.language || "en");

        if (!normalizedConfig.chat_display_name?.trim() && active) {
          setChatDisplayName(generateChatName());
        }
      } catch (err) {
        setClientLogs((prev) => [`Failed to load config: ${String(err)}`, ...prev]);
      }

      // Load platform info - independent
      try {
        const platform = await invoke<PlatformInfo>("app_platform");
        if (active) {
          setPlatformInfo(platform);
        }
      } catch {
        if (active) {
          setPlatformInfo(null);
        }
      }

      // Load profiles - independent
      try {
        const profiles = await invoke<ProfilesData>("profiles_load");
        if (!active) return;
        let resolvedProfiles = profiles;
        if (profiles.profiles.length === 0) {
          try {
            const backupRaw = localStorage.getItem("ps5upload.profiles_backup");
            if (backupRaw) {
              const parsed = JSON.parse(backupRaw) as ProfilesData;
              if (parsed.profiles && parsed.profiles.length > 0) {
                resolvedProfiles = parsed;
                await persistProfiles(parsed);
              }
            }
          } catch {
            // ignore invalid backups
          }
        }
        setProfilesData(resolvedProfiles);
        const defaultName =
          resolvedProfiles.default_profile ??
          (resolvedProfiles.profiles.length > 0
            ? resolvedProfiles.profiles[0].name
            : null);
        setCurrentProfile(defaultName);
        setProfileSelectValue(defaultName ?? "");
        if (!resolvedProfiles.default_profile && defaultName) {
          await persistProfiles({
            ...resolvedProfiles,
            default_profile: defaultName
          });
        }
        const profileToApply = resolvedProfiles.profiles.find(
          (item) => item.name === defaultName
        );
        if (profileToApply) {
          applyProfile(profileToApply);
        }
      } catch (err) {
        setClientLogs((prev) => [`Failed to load profiles: ${String(err)}`, ...prev]);
      }

      // Load queue - independent
      try {
        const queue = await invoke<QueueData>("queue_load");
        if (!active) return;
        applyQueueData(
          normalizeQueueData({
            items: queue.items || [],
            next_id: queue.next_id || 1,
            rev: queue.rev || 0,
            updated_at: queue.updated_at || 0
          })
        );
      } catch (err) {
        setClientLogs((prev) => [`Failed to load queue: ${String(err)}`, ...prev]);
      }

      // Load history - independent
      try {
        const history = await invoke<HistoryData>("history_load");
        if (!active) return;
        setHistoryData(
          normalizeHistoryData({
            records: history.records || [],
            rev: history.rev || 0,
            updated_at: history.updated_at || 0
          })
        );
      } catch (err) {
        setClientLogs((prev) => [`Failed to load history: ${String(err)}`, ...prev]);
      }

      // Load cached payload status (for offline queue history)
      try {
        const cached = localStorage.getItem("ps5upload.payload_status_cache");
        if (cached && active) {
          const parsed = JSON.parse(cached);
          setPayloadFullStatus(parsed);
        }
      } catch {
        // ignore invalid cache
      }

      // Chat disabled (FAQ tab)
      if (active) {
        setChatEnabled(false);
        setChatStatus("Disabled");
      }

      // Mark loading complete
      if (active) {
        setConfigLoaded(true);
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
    const keepAwakeModeSaved = localStorage.getItem("ps5upload.keep_awake_mode");
    if (keepAwakeModeSaved === "on" || keepAwakeModeSaved === "off" || keepAwakeModeSaved === "auto") {
      setKeepAwakeMode(keepAwakeModeSaved);
    } else {
      const keepAwakeSaved = localStorage.getItem("ps5upload.keep_awake");
      if (keepAwakeSaved) {
        setKeepAwakeMode(keepAwakeSaved === "true" ? "on" : "off");
      }
    }

    load();
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (keepAwakeMode !== "auto") {
      if (keepAwakeAutoTimeoutRef.current != null) {
        window.clearTimeout(keepAwakeAutoTimeoutRef.current);
        keepAwakeAutoTimeoutRef.current = null;
      }
      setKeepAwakeAutoHold(false);
    }
  }, [keepAwakeMode]);

  useEffect(() => {
    return () => {
      if (keepAwakeAutoTimeoutRef.current != null) {
        window.clearTimeout(keepAwakeAutoTimeoutRef.current);
        keepAwakeAutoTimeoutRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    return () => {
      if (uploadQueueRetryTimeoutRef.current != null) {
        window.clearTimeout(uploadQueueRetryTimeoutRef.current);
        uploadQueueRetryTimeoutRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    if (!configLoaded || !currentProfile) {
      return;
    }
    const existing = profilesData.profiles.find(
      (profile) => profile.name === currentProfile
    );
    if (!existing) {
      return;
    }
    const nextProfile = buildProfileFromState(currentProfile);
    if (profilesMatch(existing, nextProfile)) {
      return;
    }
    const updatedProfiles = profilesData.profiles.map((profile) =>
      profile.name === currentProfile ? nextProfile : profile
    );
    persistProfiles({
      ...profilesData,
      profiles: updatedProfiles
    });
  }, [
    configLoaded,
    currentProfile,
    profilesData,
    ip,
    storageRoot,
    preset,
    customPreset,
    connections,
    useTemp,
    autoTune
  ]);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  useEffect(() => {
    let mounted = true;
    let timeout: ReturnType<typeof setTimeout> | null = null;
    const setup = async () => {
      const unlisten = await appWindow.onResized(async (event) => {
        if (!mounted) return;
        if (timeout) {
          clearTimeout(timeout);
        }
        timeout = setTimeout(async () => {
          const size = event?.payload ?? (await appWindow.innerSize());
          if (!mounted) return;
          setWindowSize({ width: size.width, height: size.height });
          setConfigDefaults((prev) =>
            prev
              ? {
                  ...prev,
                  window_width: size.width,
                  window_height: size.height
                }
              : prev
          );
          if (resizeSaveTimer.current) {
            clearTimeout(resizeSaveTimer.current);
          }
          resizeSaveTimer.current = setTimeout(() => {
            if (!mounted || !configSaveRef.current) {
              return;
            }
            invoke("config_update", {
              config: {
                ...configSaveRef.current,
                window_width: size.width,
                window_height: size.height
              }
            }).catch((err) => {
              setClientLogs((prev) => [
                `Failed to save window size: ${String(err)}`,
                ...prev
              ]);
            });
          }, 80);
        }, 80);
      });
      const unlistenMove = await appWindow.onMoved(async (event) => {
        if (!mounted) return;
        const pos = event?.payload ?? (await appWindow.outerPosition());
        setWindowPosition({ x: pos.x, y: pos.y });
        setConfigDefaults((prev) =>
          prev
            ? {
                ...prev,
                window_x: pos.x,
                window_y: pos.y
              }
            : prev
        );
        if (resizeSaveTimer.current) {
          clearTimeout(resizeSaveTimer.current);
        }
        resizeSaveTimer.current = setTimeout(() => {
          if (!mounted || !configSaveRef.current) {
            return;
          }
          invoke("config_update", {
            config: {
              ...configSaveRef.current,
              window_x: pos.x,
              window_y: pos.y
            }
          }).catch((err) => {
            setClientLogs((prev) => [
              `Failed to save window position: ${String(err)}`,
              ...prev
            ]);
          });
        }, 80);
      });
      return () => {
        unlisten();
        unlistenMove();
      };
    };
    const cleanupPromise = setup();
    return () => {
      mounted = false;
      if (timeout) {
        clearTimeout(timeout);
      }
      if (resizeSaveTimer.current) {
        clearTimeout(resizeSaveTimer.current);
        resizeSaveTimer.current = null;
      }
      cleanupPromise.then((cleanup) => cleanup && cleanup());
    };
  }, []);


  useEffect(() => {
    if (!sourcePath) return;
    const name = sourcePath.split(/[/\\]/).filter(Boolean).pop();
    if (!name) return;
    const clean = name.replace(/\.(zip|7z|rar)$/i, "");
    const nextSubfolder = clean || name;
    if (finalPathMode === "auto") {
      if (subfolder !== nextSubfolder) {
        setSubfolder(nextSubfolder);
      }
      const base = preset === "custom" ? customPreset : preset;
      const nextDest = joinRemote(storageRoot, base, nextSubfolder);
      setFinalPath(nextDest);
    } else if (!subfolder) {
      setSubfolder(nextSubfolder);
    }
  }, [sourcePath, finalPathMode, storageRoot, preset, customPreset, subfolder]);

  useEffect(() => {
    if (finalPathMode === "auto") {
      setFinalPath(defaultDestPath);
    }
  }, [defaultDestPath, finalPathMode]);

  useEffect(() => {
    let active = true;
    if (!sourcePath.trim()) {
      setGameMeta(null);
      setGameCoverUrl(null);
      return undefined;
    }
    invoke<GameMetaResponse>("game_meta_load", { path: sourcePath })
      .then((response) => {
        if (!active) return;
        setGameMeta(response.meta ?? null);
        setGameCoverUrl(coverToDataUrl(response.cover));
      })
      .catch(() => {
        if (!active) return;
        setGameMeta(null);
        setGameCoverUrl(null);
      });
    return () => {
      active = false;
    };
  }, [sourcePath]);

  useEffect(() => {
    setManageEntries((prev) => sortEntries(prev, manageSort));
  }, [manageSort]);

  useEffect(() => {
    let active = true;
    const entry = manageSelectedEntry;
    if (
      !entry ||
      getEntryType(entry) !== "file" ||
      !entry.name.toLowerCase().endsWith(".rar") ||
      !ip.trim()
    ) {
      setManageMeta(null);
      setManageCoverUrl(null);
      return undefined;
    }
    const path = joinRemote(managePath, entry.name);
    invoke<GameMetaResponse>("manage_rar_metadata", { ip, path })
      .then((response) => {
        if (!active) return;
        setManageMeta(response.meta ?? null);
        setManageCoverUrl(coverToDataUrl(response.cover));
      })
      .catch(() => {
        if (!active) return;
        setManageMeta(null);
        setManageCoverUrl(null);
      });
    return () => {
      active = false;
    };
  }, [manageSelectedEntry, managePath, ip]);

  useEffect(() => {
    invoke("connection_set_ip", { ip }).catch(() => {
      // ignore connection ip sync failures
    });
  }, [ip]);

  useEffect(() => {
    invoke("manage_set_ip", { ip }).catch(() => {
      // ignore manage ip sync failures
    });
  }, [ip]);

  useEffect(() => {
    invoke("manage_set_path", { path: managePath }).catch(() => {
      // ignore manage path sync failures
    });
  }, [managePath]);

  useEffect(() => {
    const enabled =
      activeTab === "manage" && isConnected && ip.trim().length > 0 && !transferActive;
    invoke("manage_polling_set", { enabled }).catch(() => {
      // ignore manage poll toggle failures
    });
  }, [activeTab, isConnected, ip, transferActive]);

  useEffect(() => {
    invoke<ManageListSnapshot>("manage_list_snapshot")
      .then((snapshot) => applyManageSnapshot(snapshot))
      .catch(() => {
        // ignore snapshot failures
      });
  }, []);

  useEffect(() => {
    invoke("connection_auto_set", { enabled: autoConnect }).catch(() => {
      // ignore auto-connect failures
    });
  }, [autoConnect]);

  useEffect(() => {
    const enabled = autoConnect && ip.trim().length > 0;
    invoke("connection_polling_set", { enabled }).catch(() => {
      // ignore poll toggle failures
    });
  }, [autoConnect, ip]);

  useEffect(() => {
    const enabled = payloadAutoReload && isConnected && ip.trim().length > 0;
    invoke("payload_auto_reload_set", {
      enabled,
      mode: payloadReloadMode,
      local_path: payloadLocalPath
    }).catch(() => {
      // ignore payload auto-reload failures
    });
  }, [payloadAutoReload, payloadReloadMode, payloadLocalPath, isConnected, ip]);

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
      auto_connect: autoConnect,
      theme,
      window_width: windowSize?.width ?? configDefaults.window_width,
      window_height: windowSize?.height ?? configDefaults.window_height,
      window_x: windowPosition?.x ?? configDefaults.window_x ?? 0,
      window_y: windowPosition?.y ?? configDefaults.window_y ?? 0,
      compression,
      bandwidth_limit_mbps: bandwidthLimit,
      override_on_conflict: overrideOnConflict,
      resume_mode: resumeMode,
      auto_tune_connections: autoTune,
      optimize_upload: optimizeActive,
      update_channel: includePrerelease ? "all" : "stable",
      language,
      auto_check_payload: payloadAutoReload,
      payload_auto_reload: payloadAutoReload,
      payload_reload_mode: payloadReloadMode,
      payload_local_path: payloadLocalPath,
      download_compression: downloadCompression,
      chmod_after_upload: chmodAfterUpload,
      chat_display_name: chatDisplayName,
      rar_extract_mode: rarExtractMode,
      rar_temp: rarTemp
    };

    configSaveRef.current = nextConfig;
    invoke("config_update", { config: nextConfig }).catch((err) => {
      setClientLogs((prev) => [
        `Failed to save config: ${String(err)}`,
        ...prev
      ]);
    });
  }, [
    configLoaded,
    configDefaults,
    ip,
    storageRoot,
    connections,
    useTemp,
    autoConnect,
    compression,
    bandwidthLimit,
    overrideOnConflict,
    resumeMode,
    autoTune,
    optimizeActive,
    includePrerelease,
    theme,
    windowSize,
    windowPosition,
    language,
    payloadAutoReload,
    payloadReloadMode,
    payloadLocalPath,
    downloadCompression,
    chmodAfterUpload,
    chatDisplayName,
    rarExtractMode,
    rarTemp
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
      const unlistenComplete = await listen<TransferCompleteEvent>(
        "transfer_complete",
        (event) => {
          if (!mounted) return;
          const snapshot = transferSnapshot.current;
          if (snapshot.runId && event.payload.run_id !== snapshot.runId) return;
          const payload = event.payload;
          const completeBytes = toSafeNumber(payload.bytes);
          const completeFiles = toSafeNumber(payload.files);
          setTransferState((prev) => ({
            ...prev,
            status: "Complete",
            sent: completeBytes,
            total: completeBytes
          }));
          const duration =
            snapshot.state.elapsed ||
            (snapshot.startedAt ? (Date.now() - snapshot.startedAt) / 1000 : 0);
          const speed = duration > 0 ? completeBytes / duration : 0;
          if (snapshot.source && snapshot.dest) {
            const record: TransferRecord = {
              timestamp: Math.floor(Date.now() / 1000),
              source_path: snapshot.source,
              dest_path: snapshot.dest,
              file_count: completeFiles,
              total_bytes: completeBytes,
              duration_secs: duration,
              speed_bps: speed,
              success: true,
              via_queue: snapshot.viaQueue,
              game_meta: snapshot.gameMeta ?? null,
              cover_url: snapshot.coverUrl ?? null
            };
            setHistoryData((prev) => ({
              records: [...prev.records, record].slice(-100),
              rev: (prev.rev || 0) + 1,
              updated_at: Date.now()
            }));
            invoke("history_add", { record }).catch(() => {
              setClientLogs((prev) => [
                "Failed to save history entry.",
                ...prev
              ]);
            });
          }
          if (
            snapshot.dest &&
            configSnapshot.current.chmodAfterUpload &&
            !(snapshot.source && isArchivePath(snapshot.source))
          ) {
            invoke("manage_chmod", {
              ip: configSnapshot.current.ip,
              path: snapshot.dest
            }).catch(() => {
              // ignore chmod failures in UI
            });
          }
          if (!snapshot.viaQueue && snapshot.source && snapshot.dest) {
            pushClientLog(
              `Transfer completed: ${getBaseName(snapshot.source)} → ${snapshot.dest}`,
              "info"
            );
          }
          if (snapshot.viaQueue && queueSnapshot.current.currentId) {
            const id = queueSnapshot.current.currentId;
            updateQueueItemStatus(id, "Completed").then(() => {
              processNextQueueItem();
            });
          } else {
            // Reset active run ID to re-enable UI (non-queue transfer)
            setActiveRunId(null);
            setActiveTransferSource("");
            setActiveTransferDest("");
            setTransferStartedAt(null);
          }
          maintenanceRequestedRef.current = true;
        }
      );
      const unlistenError = await listen<TransferErrorEvent>(
        "transfer_error",
        (event) => {
          if (!mounted) return;
          const snapshot = transferSnapshot.current;
          if (snapshot.runId && event.payload.run_id !== snapshot.runId) return;
          const rawMessage = event.payload.message || "";
          const successMatch = rawMessage.match(/^(SUCCESS|OK)\s+(\d+)\s+(\d+)/i);
          if (successMatch) {
            const files = Number(successMatch[2]) || snapshot.state.files || 0;
            const bytes = Number(successMatch[3]) || snapshot.state.total || snapshot.state.sent || 0;
            setTransferState((prev) => ({
              ...prev,
              status: "Complete",
              sent: bytes,
              total: bytes
            }));
            const duration =
              snapshot.state.elapsed ||
              (snapshot.startedAt ? (Date.now() - snapshot.startedAt) / 1000 : 0);
            const speed = duration > 0 ? bytes / duration : 0;
            if (snapshot.source && snapshot.dest) {
            const record: TransferRecord = {
              timestamp: Math.floor(Date.now() / 1000),
              source_path: snapshot.source,
              dest_path: snapshot.dest,
              file_count: files,
              total_bytes: bytes,
              duration_secs: duration,
              speed_bps: speed,
              success: true,
              via_queue: snapshot.viaQueue,
              game_meta: snapshot.gameMeta ?? null,
              cover_url: snapshot.coverUrl ?? null
            };
              setHistoryData((prev) => ({
                records: [...prev.records, record].slice(-100),
                rev: (prev.rev || 0) + 1,
                updated_at: Date.now()
              }));
              invoke("history_add", { record }).catch(() => {
                setClientLogs((prev) => [
                  "Failed to save history entry.",
                  ...prev
                ]);
              });
            }
            if (
              snapshot.dest &&
              configSnapshot.current.chmodAfterUpload &&
              !(snapshot.source && isArchivePath(snapshot.source))
            ) {
              invoke("manage_chmod", {
                ip: configSnapshot.current.ip,
                path: snapshot.dest
              }).catch(() => {
                // ignore chmod failures in UI
              });
            }
            if (!snapshot.viaQueue && snapshot.source && snapshot.dest) {
              pushClientLog(
                `Transfer completed: ${getBaseName(snapshot.source)} → ${snapshot.dest}`,
                "info"
              );
            }
            if (snapshot.viaQueue && queueSnapshot.current.currentId) {
              const id = queueSnapshot.current.currentId;
              updateQueueItemStatus(id, "Completed").then(() => {
                processNextQueueItem();
              });
            } else {
              setActiveRunId(null);
              setActiveTransferSource("");
              setActiveTransferDest("");
              setTransferStartedAt(null);
            }
            return;
          }
          setTransferState((prev) => ({
            ...prev,
            status: `Error: ${event.payload.message}`
          }));
          const duration =
            snapshot.state.elapsed ||
            (snapshot.startedAt ? (Date.now() - snapshot.startedAt) / 1000 : 0);
          const speed =
            duration > 0 ? snapshot.state.sent / duration : 0;
          if (snapshot.source && snapshot.dest) {
          const record: TransferRecord = {
            timestamp: Math.floor(Date.now() / 1000),
            source_path: snapshot.source,
            dest_path: snapshot.dest,
            file_count: snapshot.state.files,
            total_bytes: snapshot.state.sent,
            duration_secs: duration,
            speed_bps: speed,
            success: false,
            error: event.payload.message,
            via_queue: snapshot.viaQueue,
            game_meta: snapshot.gameMeta ?? null,
            cover_url: snapshot.coverUrl ?? null
          };
            setHistoryData((prev) => ({
              records: [...prev.records, record].slice(-100),
              rev: (prev.rev || 0) + 1,
              updated_at: Date.now()
            }));
            invoke("history_add", { record }).catch(() => {
              setClientLogs((prev) => [
                "Failed to save history entry.",
                ...prev
              ]);
            });
          }
          if (snapshot.viaQueue && queueSnapshot.current.currentId) {
            const id = queueSnapshot.current.currentId;
            updateQueueItemStatus(id, { Failed: event.payload.message }, {
              last_failed_at: Math.floor(Date.now() / 1000),
              last_failed_bytes: snapshot.state.sent,
              last_failed_total_bytes: snapshot.state.total,
              last_failed_files: snapshot.state.files,
              last_failed_elapsed_sec: duration
            }).then(() => {
              processNextQueueItem();
            });
          }
          maintenanceRequestedRef.current = true;
          const level = event.payload.message.toLowerCase().includes("cancel")
            ? "warn"
            : "error";
          const detailParts = [
            snapshot.state.total > 0
              ? `${formatBytes(snapshot.state.sent)} / ${formatBytes(snapshot.state.total)}`
              : `${formatBytes(snapshot.state.sent)} transferred`,
            `${snapshot.state.files || 0} ${tr("files")}`,
            duration > 0 ? `Elapsed ${formatDuration(duration)}` : "Elapsed —"
          ];
          if (snapshot.state.currentFile) {
            detailParts.push(`Current ${snapshot.state.currentFile}`);
          }
          if (snapshot.viaQueue) {
            detailParts.push("Queue item");
          }
          if (snapshot.source && snapshot.dest) {
            pushClientLog(
              `Transfer failed: ${getBaseName(snapshot.source)} → ${snapshot.dest} (${event.payload.message})`,
              level
            );
          } else {
            pushClientLog(`Transfer failed: ${event.payload.message}`, level);
          }
          pushClientLog(`Details: ${detailParts.join(" · ")}`, level);
          if (!snapshot.viaQueue) {
            // Reset active run ID to re-enable UI (non-queue transfer)
            setActiveRunId(null);
            setActiveTransferSource("");
            setActiveTransferDest("");
            setTransferStartedAt(null);
          }
        }
      );
      const unlistenLog = await listen<TransferLogEvent>(
        "transfer_log",
        (event) => {
          if (!mounted) return;
          if (transferActive) return;
          clientLogBuffer.current = [
            `${event.payload.message}`,
            ...clientLogBuffer.current
          ].slice(0, 100);
          if (!clientLogFlush.current) {
            clientLogFlush.current = setTimeout(() => {
              if (!mounted) return;
              setClientLogs(clientLogBuffer.current);
              clientLogFlush.current = null;
            }, 250);
          }
        }
      );

      const unlistenPayloadLog = await listen<{ message: string }>(
        "payload_log",
        (event) => {
          if (!mounted) return;
          if (transferActive) return;
          payloadLogBuffer.current = [
            `${event.payload.message}`,
            ...payloadLogBuffer.current
          ].slice(0, 100);
          if (!payloadLogFlush.current) {
            payloadLogFlush.current = setTimeout(() => {
              if (!mounted) return;
              setPayloadLogs(payloadLogBuffer.current);
              payloadLogFlush.current = null;
            }, 250);
          }
        }
      );

      const unlistenPayloadDone = await listen<{
        bytes?: number | null;
        error?: string | null;
      }>("payload_done", (event) => {
        if (!mounted) return;
        setPayloadBusy(false);
        if (event.payload?.error) {
          setPayloadStatus(`Error: ${event.payload.error}`);
          payloadLogBuffer.current = [
            `Payload failed: ${event.payload.error}`,
            ...payloadLogBuffer.current
          ].slice(0, 100);
          setPayloadLogs(payloadLogBuffer.current);
        } else if (typeof event.payload?.bytes === "number") {
          setPayloadStatus(`Sent (${formatBytes(event.payload.bytes)})`);
        }
      });

      const unlistenPayloadVersion = await listen<{
        version?: string | null;
        error?: string | null;
      }>("payload_version", (event) => {
        if (!mounted) return;
        if (event.payload?.version) {
          setPayloadVersion(event.payload.version);
          setPayloadStatus(`Running (v${event.payload.version})`);
        } else if (event.payload?.error) {
          setPayloadVersion(null);
          setPayloadStatus(`Not detected (${event.payload.error})`);
        }
      });

      const unlistenPayloadBusy = await listen<{ busy: boolean }>(
        "payload_busy",
        (event) => {
          if (!mounted) return;
          setPayloadBusy(!!event.payload?.busy);
        }
      );

      const unlistenPayloadStatus = await listen<PayloadStatusSnapshot>(
        "payload_status_update",
        (event) => {
          if (!mounted) return;
          applyPayloadSnapshot(event.payload ?? null);
        }
      );

      const unlistenQueueHint = await listen<QueueHintEvent>("queue_hint", (event) => {
        if (!mounted) return;
        const payload = event.payload;
        if (!payload || !payload.queue_id) return;
        setQueueMetaById((prev) => ({
          ...prev,
          [payload.queue_id]: {
            ...(prev[payload.queue_id] || {}),
            source_path: payload.source_path,
            dest_path: payload.dest_path,
            size_bytes: payload.size_bytes
          }
        }));
        handleRefreshQueueStatus();
        invoke<GameMetaResponse>("game_meta_load", { path: payload.source_path })
          .then((response) => {
            if (!mounted) return;
            setQueueMetaById((prev) => ({
              ...prev,
              [payload.queue_id]: {
                ...(prev[payload.queue_id] || {}),
                source_path: payload.source_path,
                dest_path: payload.dest_path,
                size_bytes: payload.size_bytes,
                game_meta: response.meta ?? null,
                cover_url: coverToDataUrl(response.cover)
              }
            }));
          })
          .catch(() => {
            // ignore metadata failures
          });
      });

      const unlistenConnectionStatus = await listen<ConnectionStatusSnapshot>(
        "connection_status_update",
        (event) => {
          if (!mounted) return;
          applyConnectionSnapshot(event.payload ?? null);
        }
      );

      const unlistenUpdateReady = await listen("update_ready", () => {
        if (!mounted) return;
        setUpdatePending(true);
        const lang = languageRef.current;
        setUpdateDownloadStatus(t(lang, "update_ready"));
        setUpdateStatus(t(lang, "update_ready_status"));
      });

      const unlistenUpdateError = await listen<string>("update_error", (event) => {
        if (!mounted) return;
        setUpdateDownloadStatus(`Update failed: ${event.payload}`);
      });

      const unlistenManageProgress = await listen<ManageProgressEvent>(
        "manage_progress",
        (event) => {
          if (!mounted) return;
          setManageModalLastProgressAt(Date.now());
          setManageBusy(true);
          const now = Date.now();
          if (event.payload.op !== lastManageOp.current) {
            lastManageOp.current = event.payload.op;
            lastManageProgressUpdate.current = 0;
          }
          if (
            now - lastManageProgressUpdate.current < 1000 &&
            !(event.payload.total > 0 && event.payload.processed >= event.payload.total)
          ) {
            return;
          }
          lastManageProgressUpdate.current = now;
          const prev = manageSpeedRef.current;
          const nextProcessed = event.payload.processed;
          let speed = prev.speed;
          if (prev.op !== event.payload.op) {
            speed = 0;
          } else if (prev.at > 0 && nextProcessed >= prev.processed) {
            const delta = nextProcessed - prev.processed;
            const elapsed = (now - prev.at) / 1000;
            if (elapsed > 0) {
              speed = delta / elapsed;
            }
          }
          manageSpeedRef.current = {
            op: event.payload.op,
            processed: nextProcessed,
            at: now,
            speed
          };
          setManageProgress({
            op: event.payload.op,
            processed: event.payload.processed,
            total: event.payload.total,
            currentFile: event.payload.current_file ?? "",
            speed_bps: speed
          });
        }
      );

      const unlistenManageDone = await listen<ManageDoneEvent>(
        "manage_done",
        (event) => {
          if (!mounted) return;
          setManageBusy(false);
          lastManageProgressUpdate.current = 0;
          lastManageOp.current = "";
          manageSpeedRef.current = {
            op: "",
            processed: 0,
            at: 0,
            speed: 0
          };
          setManageProgress({
            op: "",
            processed: 0,
            total: 0,
            currentFile: "",
            speed_bps: 0
          });
          setManageModalLastProgressAt(null);
          if (manageModalOpRef.current === event.payload.op) {
            setManageModalDone(true);
            setManageModalError(event.payload.error ?? null);
            setManageModalStatus(
              event.payload.error
                ? event.payload.error.toLowerCase().includes("cancel")
                  ? "Cancelled"
                  : "Failed"
                : "Done"
            );
            if (!event.payload.error && manageModalOpRef.current === "Upload") {
              const elapsedMs = manageModalStartedAtRef.current
                ? Date.now() - manageModalStartedAtRef.current
                : 0;
              const elapsedSec =
                elapsedMs > 0 ? Math.max(1, Math.round(elapsedMs / 1000)) : 0;
              const bytes = event.payload.bytes ?? manageProgress.processed;
              const files =
                typeof event.payload.files === "number"
                  ? event.payload.files
                  : null;
              setManageModalSummary(
                `${formatBytes(bytes || 0)} • ${
                  files !== null ? `${files} files` : "files"
                } • ${elapsedSec}s`
              );
            }
          }
          if (event.payload.error) {
            setManageStatus(`${event.payload.op} failed: ${event.payload.error}`);
            pushClientLog(
              `${event.payload.op} failed: ${event.payload.error}`,
              event.payload.error.toLowerCase().includes("cancel") ? "warn" : "error"
            );
          } else {
            setManageStatus(`${event.payload.op} complete`);
            pushClientLog(`${event.payload.op} completed.`, "info");
            const snapshot = manageSnapshot.current;
            if (snapshot.ip.trim()) {
              invoke<ManageListSnapshot>("manage_list_refresh", {
                ip: snapshot.ip,
                path: snapshot.path
              })
                .then((snapshot) => {
                  if (!mounted) return;
                  applyManageSnapshot(snapshot);
                })
                .catch(() => {
                  // leave status as is
                });
            }
          }
        }
      );

      const unlistenManageLog = await listen<ManageLogEvent>(
        "manage_log",
        (event) => {
          if (!mounted) return;
          setClientLogs((prev) =>
            [`${event.payload.message}`, ...prev].slice(0, 100)
          );
        }
      );

      const unlistenManageList = await listen<ManageListSnapshot>(
        "manage_list_update",
        (event) => {
          if (!mounted) return;
          applyManageSnapshot(event.payload ?? null);
        }
      );

      const unlistenChatMessage = await listen<ChatMessageEvent>(
        "chat_message",
        (event) => {
          if (!mounted) return;
          setChatMessages((prev) => {
            const next = [...prev, event.payload];
            if (next.length > 500) {
              next.splice(0, next.length - 500);
            }
            return next;
          });
          setChatStats((prev) => ({
            ...prev,
            sent: prev.sent + (event.payload.local ? 1 : 0),
            received: prev.received + (event.payload.local ? 0 : 1)
          }));
        }
      );

      const unlistenChatStatus = await listen<ChatStatusEvent>(
        "chat_status",
        (event) => {
          if (!mounted) return;
          setChatStatus(event.payload.status);
        }
      );

      const unlistenChatAck = await listen<ChatAckEvent>("chat_ack", (event) => {
        if (!mounted) return;
        setChatStats((prev) => ({
          ...prev,
          acked: prev.acked + (event.payload.ok ? 1 : 0),
          rejected: prev.rejected + (event.payload.ok ? 0 : 1)
        }));
      });

      const unlistenScanProgress = await listen<{ files: number, total: number }>(
        "scan_progress",
        (event) => {
          if (!mounted) return;
          setScanFiles(event.payload.files);
          setScanTotal(event.payload.total);
          const now = Date.now();
          if (now - lastScanLogAt.current >= 1000) {
            lastScanLogAt.current = now;
            setClientLogs((prev) => [
              `Scanning... ${event.payload.files} files, ${formatBytes(event.payload.total)}`,
              ...prev
            ].slice(0, 100));
          }
        }
      );

      const unlistenScanComplete = await listen<{
        files: number;
        total: number;
        partial?: boolean;
        reason?: string | null;
        elapsed_ms?: number;
        estimated?: boolean;
      }>(
        "scan_complete",
        (event) => {
          if (!mounted) return;
          setScanStatus('completed');
          setScanFiles(event.payload.files);
          setScanTotal(event.payload.total);
          setScanPartial(!!event.payload.partial);
          setScanPartialReason(event.payload.reason ?? null);
          setScanEstimated(!!event.payload.estimated);
          const label =
            scanMode === "deep"
              ? tr("deep_optimize_ready")
              : tr("optimize_ready");
          setClientLogs((prev) => [
            `${label}: ${event.payload.files} files, ${formatBytes(event.payload.total)}`,
            ...prev
          ].slice(0, 100));
          if (optimizePendingRef.current) {
            applyOptimizeSettings();
            optimizePendingRef.current = null;
          }
        }
      );

      const unlistenScanError = await listen<{ message: string }>(
        "scan_error",
        (event) => {
          if (!mounted) return;
          setScanStatus('error');
          setScanError(event.payload.message);
          setClientLogs((prev) => [
            `Scan error: ${event.payload.message}`,
            ...prev
          ].slice(0, 100));
          if (optimizePendingRef.current) {
            optimizePendingRef.current = null;
            setOptimizeMode("none");
            restoreOptimizeSnapshot();
          }
          setScanMode(null);
        }
      );

      return () => {
        if (clientLogFlush.current) {
          clearTimeout(clientLogFlush.current);
          clientLogFlush.current = null;
        }
        if (payloadLogFlush.current) {
          clearTimeout(payloadLogFlush.current);
          payloadLogFlush.current = null;
        }
        unlistenComplete();
        unlistenError();
        unlistenLog();
        unlistenPayloadLog();
        unlistenPayloadDone();
        unlistenPayloadVersion();
        unlistenPayloadBusy();
        unlistenPayloadStatus();
        unlistenQueueHint();
        unlistenConnectionStatus();
        unlistenUpdateReady();
        unlistenUpdateError();
        unlistenManageProgress();
        unlistenManageDone();
        unlistenManageLog();
        unlistenManageList();
        unlistenChatMessage();
        unlistenChatStatus();
        unlistenChatAck();
        unlistenScanProgress();
        unlistenScanComplete();
        unlistenScanError();
      };
    };

    const cleanupPromise = unlisten();
    return () => {
      mounted = false;
      cleanupPromise.then((cleanup) => cleanup && cleanup());
    };
  }, []);

  const toNumber = (value: number | bigint) => toSafeNumber(value, 0);
  const transferTotal = toNumber(transferState.total || 0);
  const transferSent = toNumber(transferState.sent || 0);
  const transferPercent =
    transferTotal > 0 ? Math.min(100, (transferSent / transferTotal) * 100) : 0;
  const transferElapsedDisplay =
    transferState.elapsed > 0
      ? transferState.elapsed
      : transferStartedAt
      ? (Date.now() - transferStartedAt) / 1000
      : 0;
  const transferSpeedInstant =
    transferElapsedDisplay > 0 ? transferSent / transferElapsedDisplay : 0;
  const transferSpeedMinElapsedSec = 3;
  const transferSpeedMinBytes = 1 * 1024 * 1024;
  const transferSpeedReady =
    transferElapsedDisplay >= transferSpeedMinElapsedSec &&
    transferSent >= transferSpeedMinBytes;
  const transferSpeedDisplay =
    transferSpeedReady && transferSpeedEma > 0 ? transferSpeedEma : 0;
  const transferEtaSeconds =
    transferTotal > transferSent && transferSpeedDisplay > 0
      ? Math.ceil((transferTotal - transferSent) / transferSpeedDisplay)
      : null;
  const transferPercentLabel =
    transferTotal > 0 ? `${Math.floor(transferPercent)}%` : "Streaming";
  const showTransferUpdateNote =
    transferState.status.startsWith("Uploading archive") ||
    transferState.status.startsWith("Extracting");
  const scanInProgress = scanStatus === "scanning";
  const scanSummary =
    scanFiles > 0 || scanTotal > 0
      ? `${scanFiles} ${tr("files")} | ${formatBytes(scanTotal)}`
      : "";
  const scanPartialNote = scanPartial
    ? scanPartialReason === "time"
      ? tr("optimize_sample_time", { seconds: Math.round(optimizeSampleLimitMs / 1000) })
    : scanPartialReason === "files"
      ? tr("optimize_sample_files", { count: optimizeSampleLimitFiles })
      : tr("optimize_sampled")
    : "";
  const scanEstimateNote = scanEstimated
    ? tr("optimize_estimated")
    : "";
  const scanStatusLabel =
    scanStatus === "scanning"
      ? scanMode === "deep"
        ? tr("deep_optimize_scanning")
        : tr("optimize_scanning")
    : scanStatus === "completed"
      ? scanMode === "deep"
        ? tr("deep_optimize_ready")
        : tr("optimize_ready")
      : scanStatus === "cancelled"
      ? tr("scan_cancelled")
      : scanStatus === "error"
      ? tr("scan_error")
      : "";
  const connectionsDisabled = optimizeActive || autoTune;
  const isRarSource = /\.rar$/i.test(sourcePath.trim());
  const isArchiveSource = useMemo(
    () => isArchivePath(sourcePath),
    [sourcePath]
  );
  const isRar = useMemo(
    () => isArchivePath(sourcePath) && sourcePath.toLowerCase().endsWith(".rar"),
    [sourcePath]
  );
  const canOptimize = sourcePath.trim().length > 0 && !isArchiveSource;
  const optimizeButtonDisabled =
    !canOptimize || (scanInProgress && optimizePendingRef.current !== "optimize");
  const deepOptimizeButtonDisabled =
    !canOptimize || (scanInProgress && optimizePendingRef.current !== "deep");

  const formatUptime = (seconds: number) => {
    const days = Math.floor(seconds / 86400);
    const hours = Math.floor((seconds % 86400) / 3600);
    const mins = Math.floor((seconds % 3600) / 60);
    const secs = seconds % 60;
    if (days > 0) return `${days}d ${hours}h ${mins}m`;
    if (hours > 0) return `${hours}h ${mins}m ${secs}s`;
    if (mins > 0) return `${mins}m ${secs}s`;
    return `${secs}s`;
  };

  const formatUpdatedAt = (updatedAt: number | null) => {
    if (!updatedAt) return "—";
    return new Date(updatedAt).toLocaleTimeString();
  };

  const applyConnectionSnapshot = (snapshot: ConnectionStatusSnapshot | null) => {
    if (!snapshot) return;
    setConnectionStatus(snapshot.status);
    setIsConnected(snapshot.is_connected);
    setIsConnecting(false);
    setManageStatus((prev) =>
      snapshot.is_connected ? (prev === "Not connected" ? "Connected" : prev) : "Not connected"
    );
    const available = snapshot.storage_locations;
    setStorageLocations(available);
    if (snapshot.is_connected && available.length > 0) {
      setStorageRoot((prev) => {
        if (!available.some((loc) => loc.path === prev)) {
          return available[0].path;
        }
        return prev;
      });
      setManagePath((prev) => {
        const matchesStorage = available.some((loc) => {
          if (prev === loc.path) return true;
          if (prev.startsWith(`${loc.path}/`)) return true;
          return false;
        });
        if (!prev || !matchesStorage) {
          return available[0].path;
        }
        return prev;
      });
    }
  };

  const applyManageSnapshot = (
    snapshot: ManageListSnapshot | null,
    options: { force?: boolean } = {}
  ) => {
    if (!snapshot) return [];
    if (!options.force && snapshot.path && snapshot.path !== managePathRef.current) {
      return [];
    }
    if (typeof snapshot.updated_at_ms === "number") {
      setManageLastUpdated(snapshot.updated_at_ms);
    }
    if (snapshot.error) {
      setManageStatus(`Error: ${snapshot.error}`);
      return [];
    }
    const sorted = sortEntries(snapshot.entries, manageSort);
    const selected = manageSelectionRef.current;
    const nextSelectedIndex =
      selected && selected.name
        ? sorted.findIndex((entry) => {
            if (entry.name !== selected.name) return false;
            if (!selected.type) return true;
            return getEntryType(entry) === selected.type;
          })
        : -1;
    setManageEntries(sorted);
    if (nextSelectedIndex >= 0) {
      manageSelectionRef.current = {
        name: sorted[nextSelectedIndex].name,
        type: getEntryType(sorted[nextSelectedIndex])
      };
      setManageSelected(nextSelectedIndex);
    } else {
      const fallbackIndex = manageSelectedIndexRef.current;
      if (fallbackIndex !== null && fallbackIndex >= 0 && fallbackIndex < sorted.length) {
        const fallbackEntry = sorted[fallbackIndex];
        manageSelectionRef.current = {
          name: fallbackEntry.name,
          type: getEntryType(fallbackEntry)
        };
        setManageSelected(fallbackIndex);
      } else {
        manageSelectionRef.current = null;
        setManageSelected(null);
        setManageMeta(null);
        setManageCoverUrl(null);
      }
    }
    setManageStatus(`Loaded ${sorted.length} items`);
    return sorted;
  };

  const applyPayloadSnapshot = (snapshot: PayloadStatusSnapshot | null) => {
    if (!snapshot) return;
    if (snapshot.status?.items?.length) {
      const nextStatus: Record<number, { status: string; error?: string | null }> = {};
      for (const item of snapshot.status.items) {
        const prev = extractQueueStatusRef.current[item.id];
        nextStatus[item.id] = { status: item.status, error: item.error ?? null };
        if (prev && prev.status === item.status && prev.error === (item.error ?? null)) {
          continue;
        }
        if (item.status === "complete") {
          pushClientLog(
            `Extraction completed: ${getQueueDisplayName(item)} (ID ${item.id})`,
            "info"
          );
          maintenanceRequestedRef.current = true;
        } else if (item.status === "failed") {
          const message = item.error || "Unknown error";
          const level = message.toLowerCase().includes("cancel") ? "warn" : "error";
          pushClientLog(
            `Extraction failed: ${getQueueDisplayName(item)} (ID ${item.id}) - ${message}`,
            level
          );
          maintenanceRequestedRef.current = true;
          const nowSec = Math.floor(Date.now() / 1000);
          const finishedAt = item.completed_at || nowSec;
          const elapsedSec =
            item.started_at && finishedAt >= item.started_at
              ? finishedAt - item.started_at
              : 0;
          const detailParts = [
            item.total_bytes > 0
              ? `${formatBytes(item.processed_bytes)} / ${formatBytes(item.total_bytes)}`
              : `${formatBytes(item.processed_bytes)} extracted`,
            `${item.files_extracted} ${tr("files")}`,
            elapsedSec > 0 ? `Elapsed ${formatDuration(elapsedSec)}` : "Elapsed —"
          ];
          const targetPath = item.dest_path || queueMetaById[item.id]?.dest_path;
          if (targetPath) {
            detailParts.push(`Destination ${targetPath}`);
          }
          pushClientLog(`Details: ${detailParts.join(" · ")}`, level);
        }
      }
      extractQueueStatusRef.current = nextStatus;
    }
    setPayloadFullStatus(snapshot.status ?? null);
    setPayloadStatusError(snapshot.error ?? null);
    if (snapshot.status) {
      localStorage.setItem(
        "ps5upload.payload_status_cache",
        JSON.stringify(snapshot.status)
      );
    }
    if (typeof snapshot.updated_at_ms === "number") {
      setPayloadLastUpdated(snapshot.updated_at_ms);
    }
    if (snapshot.status?.items?.length && chmodAfterUpload && ip.trim()) {
      const completed = snapshot.status.items.filter((item) => item.status === "complete");
      if (completed.length > 0) {
        setQueueChmodDone((prev) => {
          const next = { ...prev };
          for (const item of completed) {
            if (next[item.id]) continue;
            const meta = queueMetaById[item.id];
            const target = item.dest_path || meta?.dest_path;
            if (target) {
              invoke("manage_chmod", { ip, path: target }).catch(() => {
                // ignore chmod failures for queued extraction
              });
              next[item.id] = true;
            }
          }
          return next;
        });
      }
    }
    if (snapshot.status?.version) {
      setPayloadStatus(`Running (v${snapshot.status.version})`);
    } else if (snapshot.error) {
      setPayloadStatus(`Status error: ${snapshot.error}`);
    }
  };

  const handleRefreshPayloadStatus = async () => {
    if (!ip.trim()) return;
    if (payloadStatusLoading) return;
    setPayloadStatusLoading(true);
    try {
      const snapshot = await invoke<PayloadStatusSnapshot>("payload_status_refresh", { ip });
      applyPayloadSnapshot(snapshot);
      syncPayloadState();
    } catch (err) {
      const message = String(err);
      setPayloadStatusError(message);
      setClientLogs((prev) => [`Payload status error: ${message}`, ...prev].slice(0, 100));
    }
    setPayloadStatusLoading(false);
  };

  const handlePayloadReset = async () => {
    if (!ip.trim()) return;
    if (payloadResetting) return;
    setPayloadResetting(true);
    try {
      await invoke("payload_reset", { ip });
      setClientLogs((prev) => ["Payload reset requested.", ...prev].slice(0, 100));
      handleRefreshPayloadStatus();
      handleRefreshQueueStatus();
    } catch (err) {
      setClientLogs((prev) => [`Payload reset failed: ${String(err)}`, ...prev].slice(0, 100));
    }
    setPayloadResetting(false);
  };

  const handleRefreshQueueStatus = async () => {
    if (!ip.trim()) return;
    if (payloadQueueLoading) return;
    setPayloadQueueLoading(true);
    try {
      const snapshot = await invoke<PayloadStatusSnapshot>("payload_status_refresh", { ip });
      applyPayloadSnapshot(snapshot);
      syncPayloadState();
    } catch (err) {
      const message = String(err);
      setPayloadStatusError(message);
      setClientLogs((prev) => [`Queue status error: ${message}`, ...prev].slice(0, 100));
    }
    setPayloadQueueLoading(false);
  };

  const handleQueueExtract = async (src: string, dst: string) => {
    if (!ip.trim()) return;
    try {
      const id = await invoke<number>("payload_queue_extract", { ip, src, dst });
      setClientLogs((prev) => [`Queued extraction (ID: ${id}): ${src}`, ...prev].slice(0, 100));
      setNoticeTitle("Extraction queued");
      setNoticeLines([
        `ID: ${id}`,
        `Source: ${src}`,
        `Destination: ${dst}`,
        "Check the Queues tab for progress."
      ]);
      setNoticeOpen(true);
      handleRefreshPayloadStatus();
    } catch (err) {
      setClientLogs((prev) => [`Queue extract failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  const handleQueueCancel = async (id: number) => {
    if (!ip.trim()) return;
    try {
      await invoke("payload_queue_cancel", { ip, id });
      setClientLogs((prev) => [`Cancelled queue item ${id}`, ...prev].slice(0, 100));
      handleRefreshPayloadStatus();
    } catch (err) {
      setClientLogs((prev) => [`Queue cancel failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  const handleQueuePause = async (id: number) => {
    if (!ip.trim()) return;
    try {
      await invoke("payload_queue_pause", { ip, id });
      setClientLogs((prev) => [`Paused extraction item ${id}`, ...prev].slice(0, 100));
      handleRefreshQueueStatus();
    } catch (err) {
      setClientLogs((prev) => [`Queue pause failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  const handleQueueRetry = async (id: number) => {
    if (!ip.trim()) return;
    try {
      await invoke("payload_queue_retry", { ip, id });
      setClientLogs((prev) => [`Requeued extraction item ${id}`, ...prev].slice(0, 100));
      setExtractionActionById((prev) => ({ ...prev, [id]: "requeue" }));
      handleRefreshQueueStatus();
    } catch (err) {
      setClientLogs((prev) => [`Queue requeue failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  const moveExtractionQueueItem = async (fromIndex: number, toIndex: number) => {
    if (!ip.trim() || !payloadFullStatus) return;
    if (fromIndex < 0 || toIndex < 0) return;
    if (fromIndex === toIndex) return;
    const items = [...payloadFullStatus.items];
    if (fromIndex >= items.length || toIndex >= items.length) return;
    const moving = items[fromIndex];
    if (!moving || moving.status !== "pending") return;
    if (!items[toIndex] || items[toIndex].status !== "pending") return;
    items.splice(fromIndex, 1);
    items.splice(toIndex, 0, moving);
    try {
      await invoke("payload_queue_reorder", { ip, ids: items.map((item) => item.id) });
      setPayloadFullStatus({ ...payloadFullStatus, items });
      setClientLogs((prev) => [`Reordered extraction queue (moved #${moving.id}).`, ...prev].slice(0, 100));
      if (runningExtractItem) {
        const firstPendingIndex = items.findIndex((entry) => entry.status === "pending");
        if (firstPendingIndex === toIndex) {
          handleQueuePause(runningExtractItem.id);
          handleQueueProcess();
        }
      }
    } catch (err) {
      setClientLogs((prev) => [`Queue reorder failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  const handleQueueClear = async () => {
    if (!ip.trim()) return;
    try {
      await invoke("payload_queue_clear", { ip });
      setClientLogs((prev) => ["Cleared completed queue items", ...prev].slice(0, 100));
      handleRefreshPayloadStatus();
    } catch (err) {
      setClientLogs((prev) => [`Queue clear failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  const handleQueueClearAll = async () => {
    if (!ip.trim()) return;
    try {
      await invoke("payload_queue_clear_all", { ip });
      setClientLogs((prev) => ["Cleared extraction queue", ...prev].slice(0, 100));
      handleRefreshPayloadStatus();
    } catch (err) {
      setClientLogs((prev) => [`Queue clear failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  const handleQueueClearFailed = async () => {
    if (!ip.trim()) return;
    try {
      await invoke("payload_queue_clear_failed", { ip });
      setClientLogs((prev) => ["Cleared failed extraction items", ...prev].slice(0, 100));
      handleRefreshPayloadStatus();
    } catch (err) {
      setClientLogs((prev) => [`Queue clear failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  const handleQueueStopAll = async () => {
    if (!ip.trim()) return;
    if (extractionStopping) return;
    if (!payloadFullStatus?.items?.length) return;
    setExtractionStopping(true);
    setClientLogs((prev) => ["Stopping extraction queue...", ...prev].slice(0, 100));
    extractionStopAttemptsRef.current = 0;
    const maxAttempts = 8;
    const attemptIntervalMs = 800;
    const refreshTimeoutMs = 2000;
    const attemptStop = async () => {
      extractionStopAttemptsRef.current += 1;
      const attempt = extractionStopAttemptsRef.current;
      let snapshot: PayloadStatusSnapshot | null = null;
      try {
        snapshot = await Promise.race([
          invoke<PayloadStatusSnapshot>("payload_status_refresh", { ip }),
          new Promise<PayloadStatusSnapshot>((_, reject) =>
            setTimeout(() => reject(new Error("Status refresh timed out")), refreshTimeoutMs)
          )
        ]);
        applyPayloadSnapshot(snapshot);
      } catch {
        // ignore refresh errors; fall back to cached status
      }
      const items = snapshot?.status?.items || payloadFullStatus?.items || [];
      const active = items.filter(
        (item) => item.status === "pending" || item.status === "running" || item.status === "idle"
      );
      if (active.length === 0) {
        setClientLogs((prev) => ["Extraction queue stopped", ...prev].slice(0, 100));
        setExtractionStopping(false);
        return;
      }
      await Promise.allSettled(
        active.map((item) => invoke("payload_queue_cancel", { ip, id: item.id }))
      );
      if (attempt < maxAttempts) {
        setTimeout(attemptStop, attemptIntervalMs);
      } else {
        setClientLogs((prev) => ["Extraction queue stop requested", ...prev].slice(0, 100));
        setTimeout(() => setExtractionStopping(false), 500);
      }
    };
    attemptStop().catch(() => {
      setClientLogs((prev) => ["Queue stop failed", ...prev].slice(0, 100));
      setExtractionStopping(false);
    });
  };

  const handleClearTmp = async () => {
    if (!ip.trim()) return;
    try {
      pushClientLog("Clearing PS5 temp folders...", "info");
      await invoke("payload_clear_tmp", { ip });
      pushClientLog("PS5 temp folders cleared.", "info");
    } catch (err) {
      pushClientLog(`Clear tmp failed: ${String(err)}`, "error");
    }
  };

  const handleQueueProcess = async () => {
    if (!ip.trim()) return;
    try {
      await invoke("payload_queue_process", { ip });
      setClientLogs((prev) => ["Extraction queue started", ...prev].slice(0, 100));
      handleRefreshQueueStatus();
    } catch (err) {
      setClientLogs((prev) => [`Queue start failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  useEffect(() => {
    invoke<PayloadStatusSnapshot>("payload_status_snapshot")
      .then((snapshot) => applyPayloadSnapshot(snapshot))
      .catch(() => {
        // ignore snapshot failures
      });
  }, []);

  useEffect(() => {
    invoke<ConnectionStatusSnapshot>("connection_snapshot")
      .then((snapshot) => applyConnectionSnapshot(snapshot))
      .catch(() => {
        // ignore snapshot failures
      });
  }, []);

  useEffect(() => {
    invoke("payload_set_ip", { ip }).catch(() => {
      // ignore payload ip sync failures
    });
  }, [ip]);

  useEffect(() => {
    const enabled = activeTab === "payload" && isConnected && ip.trim().length > 0;
    invoke("payload_polling_set", { enabled }).catch(() => {
      // ignore poll toggle failures
    });
  }, [activeTab, isConnected, ip]);

  useEffect(() => {
    if (!isConnected || !ip.trim()) return;
    handleRefreshPayloadStatus();
  }, [isConnected, ip]);

  useEffect(() => {
    if (!isConnected || !ip.trim()) return;
    const hasRunningExtraction =
      payloadFullStatus?.items?.some((item) => item.status === "running") ?? false;
    const shouldPoll = activeTab === "payload" || hasRunningExtraction;
    if (!shouldPoll) return;
    const intervalMs = 1000;
    const interval = setInterval(() => {
      if (isConnected && ip.trim()) {
        handleRefreshQueueStatus();
      }
      if (activeTab === "payload") {
        handleRefreshUploadQueue();
      }
    }, intervalMs);
    return () => clearInterval(interval);
  }, [activeTab, isConnected, ip, payloadFullStatus]);

  const handleConnect = async () => {
    if (!ip.trim()) {
      setConnectionStatus("Missing IP");
      pushClientLog("Connect blocked: missing IP");
      return false;
    }
    if (connectCooldown) {
      pushClientLog("Connect blocked: please wait a moment.");
      return false;
    }
    if (isConnecting) return false;
    if (isConnected) return true;
    setConnectCooldown(true);
    setTimeout(() => setConnectCooldown(false), 1000);
    setIsConnecting(true);
    setConnectionStatus("Connecting...");
    pushClientLog(`Connecting to ${ip}...`);
    try {
      const snapshot = await invoke<ConnectionStatusSnapshot>(
        "connection_connect",
        { ip }
      );
      applyConnectionSnapshot(snapshot);
      if (snapshot.is_connected) {
        pushClientLog("Connected.");
      } else {
        pushClientLog(`Connection failed: ${snapshot.status}`);
      }
      return snapshot.is_connected;
    } catch (err) {
      setConnectionStatus(`Error: ${String(err)}`);
      setIsConnected(false);
      pushClientLog(`Connection error: ${String(err)}`);
      return false;
    } finally {
      setIsConnecting(false);
    }
  };

  const ensureConnected = async () => {
    if (isConnected) return true;
    return handleConnect();
  };

  const handleDisconnect = () => {
    setStorageLocations([]);
    setConnectionStatus("Disconnected");
    setManageEntries([]);
    setManageStatus("Not connected");
    setPayloadStatus("Disconnected");
    setPayloadVersion(null);
    setIsConnected(false);
    setIsConnecting(false);
    setPayloadStatusError(null);
    pushClientLog("Disconnected.");
  };

  const handlePayloadBrowse = async () => {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "Payload", extensions: ["elf", "bin"] }]
      });
      if (typeof selected === "string") {
        setPayloadLocalPath(selected);
        pushPayloadLog(`Payload selected: ${selected}`);
      }
    } catch (err) {
      setPayloadStatus(`Error: ${String(err)}`);
      pushPayloadLog(`Payload browse error: ${String(err)}`);
    }
  };

  const runPayloadReload = async (
    modeOverride?: "local" | "current" | "latest",
    force?: boolean
  ) => {
    if (payloadBusy || !ip.trim()) return;
    if (payloadReloadCooldown) {
      pushPayloadLog("Payload reload blocked: please wait a moment.");
      return;
    }
    if (!force) {
      if (payloadFullStatus?.status && !payloadStatusError) {
        setPayloadStatus("Payload already running.");
        pushPayloadLog("Payload already running; reload skipped.");
        return;
      }
      if (payloadVersion) {
        setPayloadStatus("Payload already running.");
        pushPayloadLog("Payload already running; reload skipped.");
        return;
      }
    }
    const mode = modeOverride ?? payloadReloadMode;
    if (mode === "local" && !payloadLocalPath.trim()) {
      setPayloadStatus(tr("payload_local_required"));
      pushPayloadLog("Payload reload blocked: local payload path required.");
      return;
    }
    if (mode === "local" && payloadProbe && !payloadProbe.ok) {
      setPayloadStatus(payloadProbe.message);
      pushPayloadLog(`Payload probe failed: ${payloadProbe.message}`);
      return;
    }
    setPayloadReloadCooldown(true);
    setTimeout(() => setPayloadReloadCooldown(false), 3000);
    const portOpen = await invoke<boolean>("port_check", {
      ip,
      port: 9021
    });
    if (!portOpen) {
      setPayloadStatus("Port 9021 closed");
      pushPayloadLog("Payload reload failed: port 9021 closed.");
      return;
    }
    setPayloadStatus(tr("payload_sending"));
    pushPayloadLog(`Sending payload (${mode})...`);
    try {
      if (mode === "local") {
        await invoke("payload_send", { ip, path: payloadLocalPath });
      } else {
        await invoke("payload_download_and_send", { ip, fetch: mode });
      }
      setPayloadStatus("Waiting for payload...");
      pushPayloadLog("Payload sent. Waiting for status...");
      setTimeout(() => {
        handlePayloadCheck();
      }, 3000);
    } catch (err) {
      setPayloadStatus(`Error: ${String(err)}`);
      pushPayloadLog(`Payload send error: ${String(err)}`);
    }
  };

  const handlePayloadSend = async () => {
    await runPayloadReload(undefined, true);
  };

  const handlePayloadCheck = async () => {
    try {
      setPayloadStatus("Checking...");
      pushPayloadLog("Checking payload status...");
      await invoke("payload_check", { ip });
    } catch (err) {
      setPayloadStatus(`Error: ${String(err)}`);
      pushPayloadLog(`Payload check error: ${String(err)}`);
    }
  };

  const handlePayloadDownload = async (kind: "current" | "latest") => {
    await runPayloadReload(kind, true);
  };

  const handleUpdateCheck = async () => {
    setUpdateStatus("Checking for updates...");
    setUpdatePending(false);
    try {
      const release = await invoke<ReleaseInfo>("update_check", {
        includePrerelease
      });
      setUpdateInfo(release);
      const available = isNewerVersion(release.tag_name, appVersion);
      setUpdateAvailable(available);
      if (!selectClientAsset(release.assets, platformInfo)) {
        setUpdateDownloadStatus("Client build not found for this platform.");
      } else {
        setUpdateDownloadStatus("");
      }
      setUpdateStatus(
        available
          ? `Update available: ${release.tag_name}`
          : `Up to date (${release.tag_name})`
      );
    } catch (err) {
      setUpdateStatus(`Update check failed: ${String(err)}`);
      setUpdateInfo(null);
      setUpdateAvailable(false);
    }
  };

  const handleUpdateDownload = async (asset: ReleaseAsset) => {
    const filePath = await save({ defaultPath: asset.name });
    if (!filePath) return;
    setUpdateDownloadStatus(`Downloading ${asset.name}...`);
    try {
      await invoke("update_download_asset", {
        url: asset.browser_download_url,
        dest_path: filePath
      });
      setUpdateDownloadStatus(`Downloaded to ${filePath}`);
    } catch (err) {
      setUpdateDownloadStatus(`Download failed: ${String(err)}`);
    }
  };

  const handleUpdatePrepareSelf = async () => {
    if (!updateInfo) return;
    try {
      const asset = selectClientAsset(updateInfo.assets, platformInfo);
      if (!asset) {
        setUpdateDownloadStatus("Client asset not found for this platform.");
        return;
      }
      setUpdateDownloadStatus("Preparing update...");
      setUpdatePending(false);
      await invoke("update_prepare_self", { asset_url: asset.browser_download_url });
    } catch (err) {
      setUpdateDownloadStatus(`Update failed: ${String(err)}`);
    }
  };

  const handleUpdateApply = async () => {
    const ok = window.confirm(tr("update_restart_confirm"));
    if (!ok) return;
    try {
      await invoke("update_apply_self");
    } catch (err) {
      setUpdateDownloadStatus(`Update failed: ${String(err)}`);
    }
  };

  const handleUpload = async () => {
    setClientLogs((prev) => ["Upload clicked", ...prev].slice(0, 100));
    await startUploadWithParams({
      sourcePath,
      destPath: finalDestPath,
      resumeOverride: resumeMode
    });
  };

  const sanitizeQueueData = (data: QueueData): QueueData => {
    let inProgressSeen = false;
    return {
      items: (data.items || []).map((item) => {
        const nextItem = {
          ...item,
          size_bytes:
            typeof item.size_bytes === "bigint"
              ? toSafeNumber(item.size_bytes)
              : item.size_bytes ?? null
        };
        if (nextItem.transfer_settings?.resume_mode) {
          nextItem.transfer_settings = {
            ...nextItem.transfer_settings,
            resume_mode: normalizeResumeMode(nextItem.transfer_settings.resume_mode)
          };
        }
        if (nextItem.status === "InProgress") {
          if (inProgressSeen) {
            nextItem.status = "Pending";
            nextItem.paused = true;
          } else {
            inProgressSeen = true;
          }
        }
        return nextItem;
      }),
      next_id: data.next_id || 1,
      rev: data.rev,
      updated_at: data.updated_at
    };
  };

  const sanitizeHistoryData = (data: HistoryData): HistoryData => ({
    records: (data.records || []).map((record) => ({
      ...record,
      file_count: toSafeNumber(record.file_count),
      total_bytes: toSafeNumber(record.total_bytes),
      duration_secs: toSafeNumber(record.duration_secs),
      speed_bps: toSafeNumber(record.speed_bps)
    })),
    rev: data.rev,
    updated_at: data.updated_at
  });

  const applyQueueData = (data: QueueData) => {
    const normalized = sanitizeQueueData(data);
    setQueueData(normalized);
    const activeItem = normalized.items.find((item) => item.status === "InProgress");
    if (activeItem) {
      setCurrentQueueItemId(activeItem.id);
    } else {
      setCurrentQueueItemId(null);
    }
    return normalized;
  };

  const saveQueueData = async (data: QueueData) => {
    const sanitized = sanitizeQueueData(data);
    const rev =
      typeof sanitized.rev === "number"
        ? sanitized.rev
        : (queueData.rev || 0) + 1;
    const next = { ...sanitized, rev, updated_at: Date.now() };
    applyQueueData(next);
    try {
      await invoke("queue_update", { data: next });
    } catch (err) {
      setClientLogs((prev) => [
        `Failed to save queue: ${String(err)}`,
        ...prev
      ]);
    }
  };

  const normalizeQueueData = (data: QueueData | null) =>
    sanitizeQueueData({
      items: data?.items || [],
      next_id: data?.next_id || 1,
      rev: data?.rev || 0,
      updated_at: data?.updated_at || 0
    });

  const normalizeHistoryData = (data: HistoryData | null) =>
    sanitizeHistoryData({
      records: data?.records || [],
      rev: data?.rev || 0,
      updated_at: data?.updated_at || 0
    });

  const syncPayloadState = async () => {
    if (!ip.trim() || !isConnected) return;
    const now = Date.now();
    if (now - lastPayloadSyncAt.current < 2000) return;
    lastPayloadSyncAt.current = now;

    if (payloadStatusError && payloadStatusError.includes("ECONNREFUSED")) {
      return;
    }

    try {
      const info = await invoke<{
        upload_queue_rev?: number;
        history_rev?: number;
        upload_queue_updated_at?: number;
        history_updated_at?: number;
        extract_queue_updated_at?: number;
      }>("payload_sync_info", { ip });

      const localQueueRev = queueData.rev || 0;
      const localHistoryRev = historyData.rev || 0;
      const payloadQueueRev =
        typeof info.upload_queue_rev === "number"
          ? info.upload_queue_rev
          : (info.upload_queue_updated_at || 0) * 1000;
      const payloadHistoryRev =
        typeof info.history_rev === "number"
          ? info.history_rev
          : (info.history_updated_at || 0) * 1000;

      if (payloadQueueRev > localQueueRev && !transferActive) {
        const payloadText = await invoke<string>("payload_upload_queue_get", { ip });
        if (payloadText && payloadText.trim()) {
          try {
            const data = JSON.parse(payloadText);
            const normalized = normalizeQueueData(data);
            const localByKey = new Map(
              queueData.items.map((item) => [
                normalizeQueueKey(item.source_path, item.dest_path),
                item
              ])
            );
            const mergedItems = normalized.items.map((item) => {
              const key = normalizeQueueKey(item.source_path, item.dest_path);
              const local = localByKey.get(key);
              if (local && (local.status === "Completed" || typeof local.status === "object")) {
                return { ...item, status: local.status };
              }
              return item;
            });
            const localTerminal = queueData.items.filter(
              (item) =>
                (item.status === "Completed" || typeof item.status === "object") &&
                !normalized.items.some(
                  (p) =>
                    normalizeQueueKey(p.source_path, p.dest_path) ===
                    normalizeQueueKey(item.source_path, item.dest_path)
                )
            );
            const merged = normalizeQueueData({
              ...normalized,
              items: [...mergedItems, ...localTerminal],
              next_id: Math.max(
                normalized.next_id || 1,
                ...[...mergedItems, ...localTerminal].map((item) => item.id + 1)
              )
            });
            applyQueueData(merged);
            await invoke("queue_update", { data: merged });
            pushClientLog("Upload queue restored from payload.", "debug");
          } catch (err) {
            pushClientLog(`Failed to parse payload queue: ${String(err)}`, "error");
          }
        }
      } else if (localQueueRev > payloadQueueRev) {
        await invoke("payload_upload_queue_sync", {
          ip,
          payload: JSON.stringify(sanitizeQueueData(queueData))
        });
      }

      if (payloadHistoryRev > localHistoryRev) {
        const payloadText = await invoke<string>("payload_history_get", { ip });
        if (payloadText && payloadText.trim()) {
          try {
            const data = JSON.parse(payloadText);
            const normalized = normalizeHistoryData(data);
            setHistoryData(normalized);
            await invoke("history_save", { data: normalized });
            pushClientLog("History restored from payload.", "debug");
          } catch (err) {
            pushClientLog(`Failed to parse payload history: ${String(err)}`, "error");
          }
        }
      } else if (localHistoryRev > payloadHistoryRev) {
        await invoke("payload_history_sync", {
          ip,
          payload: JSON.stringify(sanitizeHistoryData(historyData))
        });
      }
    } catch (err) {
      const message = String(err);
      const last = lastPayloadSyncError.current;
      if (!last || last.message !== message || now - last.at > 10000) {
        pushClientLog(`Payload sync failed: ${message}`, "warn");
        lastPayloadSyncError.current = { message, at: now };
      }
    }
  };

  const handleRefreshUploadQueue = async () => {
    try {
      const queue = await invoke<QueueData>("queue_load");
      applyQueueData(
        normalizeQueueData({
          items: queue.items || [],
          next_id: queue.next_id || 1,
          rev: queue.rev || 0,
          updated_at: queue.updated_at || 0
        })
      );
      const hasActive = (queue.items || []).some(
        (item) => item.status === "InProgress" || item.status === "Pending"
      );
      if (!hasActive) {
        setUploadQueueRunning(false);
      }
    } catch (err) {
      setClientLogs((prev) => [`Queue refresh failed: ${String(err)}`, ...prev].slice(0, 100));
    }
  };

  const buildDestPathForItem = (base: string, item: QueueItem) => {
    if (item.dest_path && item.dest_path.trim()) {
      return item.dest_path;
    }
    const presetPath =
      item.preset_index === 2
        ? item.custom_preset_path
        : presetOptions[item.preset_index] ?? presetOptions[0];
    const baseClean = base.replace(/\/$/, "");
    const presetClean = presetPath.replace(/^\/|\/$/g, "");
    const folder = item.subfolder_name || "App";
    if (!presetClean) {
      return `${baseClean}/${folder}`;
    }
    return `${baseClean}/${presetClean}/${folder}`;
  };

  const getBaseName = (value: string) =>
    value.split(/[/\\]/).filter(Boolean).pop() || "App";

  const buildQueueItem = (id: number, params: {
    sourcePath: string;
    subfolderName: string;
    presetIndex: number;
    customPresetPath: string;
    storageBase?: string;
    destPath?: string;
    sizeBytes?: number | null;
  }): QueueItem => ({
    id,
    source_path: params.sourcePath,
    subfolder_name: params.subfolderName,
    preset_index: params.presetIndex,
    custom_preset_path: params.customPresetPath,
    storage_base: params.storageBase,
    dest_path: params.destPath,
    ps5_ip: ip.trim() || undefined,
    status: "Pending",
    paused: false,
    size_bytes: params.sizeBytes ?? null,
    transfer_settings: {
      use_temp: useTemp,
      connections,
      resume_mode: resumeMode,
      compression,
      bandwidth_limit_mbps: bandwidthLimit,
      auto_tune_connections: autoTune,
      optimize_upload: optimizeActive,
      chmod_after_upload: chmodAfterUpload,
      rar_extract_mode: rarExtractMode,
      rar_temp_root: rarTemp,
      override_on_conflict: overrideOnConflict
    }
  });

  const normalizeQueueKey = (source: string, dest?: string | null) => {
    const normalizePath = (value: string) =>
      value.trim().replace(/\\/g, "/").replace(/\/+$/, "");
    const srcKey = normalizePath(source || "");
    const destKey = dest ? normalizePath(dest) : "";
    return `${srcKey}::${destKey}`;
  };

  const addQueueItems = async (items: QueueItem[]) => {
    if (items.length === 0) return;
    const existingKeys = new Set(
      queueData.items.map((item) => normalizeQueueKey(item.source_path, item.dest_path))
    );
    const deduped: QueueItem[] = [];
    let skipped = 0;
    for (const item of items) {
      const key = normalizeQueueKey(item.source_path, item.dest_path);
      if (existingKeys.has(key)) {
        skipped += 1;
        continue;
      }
      existingKeys.add(key);
      deduped.push(item);
    }
    if (deduped.length === 0) {
      if (skipped > 0) pushClientLog(tr("queue_duplicate"));
      return;
    }
    const nextQueue: QueueData = {
      items: [...deduped, ...queueData.items],
      next_id: Math.max(queueData.next_id || 1, ...deduped.map((item) => item.id + 1))
    };
    await saveQueueData(nextQueue);
    pushClientLog(tr("queue_added", { count: deduped.length }));
    if (skipped > 0) pushClientLog(tr("queue_duplicate"));
    const first = deduped[0];
    setNoticeTitle("Upload queued");
    setNoticeLines([
      `Added: ${deduped.length} item${deduped.length > 1 ? "s" : ""}`,
      `Source: ${first.source_path}`,
      `Destination: ${first.dest_path || "(default)"}`,
      "Check the Upload Queue tab for progress."
    ]);
    setNoticeOpen(true);
  };

  const handleAddToQueue = async () => {
    if (!sourcePath.trim()) return;
    const nextId = queueData.next_id || 1;
    const subfolderName = subfolder || getBaseName(sourcePath);
    const presetIndex = presetOptions.indexOf(preset);
    const item = buildQueueItem(nextId, {
      sourcePath,
      subfolderName,
      presetIndex: presetIndex === -1 ? 0 : presetIndex,
      customPresetPath: customPreset,
      storageBase: storageRoot,
      destPath: finalDestPath,
      sizeBytes: transferState.total ? toSafeNumber(transferState.total) : null
    });
    await addQueueItems([item]);
  };

  const logUploadQueueTerminal = (item: QueueItem, status: QueueStatus) => {
    const label = item.subfolder_name || getBaseName(item.source_path);
    const base = item.storage_base || storageRoot;
    const dest = buildDestPathForItem(base, item);
    if (status === "Completed") {
      pushClientLog(`Upload completed: ${label} → ${dest}`, "info");
      return;
    }
    if (typeof status === "object") {
      const message = status.Failed || "Unknown error";
      if (message === USER_STOPPED_SENTINEL || message === tr("stopped")) {
        pushClientLog(`Upload stopped: ${label} → ${dest}`, "warn");
      } else {
        const level = message.toLowerCase().includes("cancel") ? "warn" : "error";
        pushClientLog(`Upload failed: ${label} → ${dest} (${message})`, level);
      }
    }
  };

  const updateQueueItemStatus = async (
    id: number,
    status: QueueStatus,
    updates?: Partial<QueueItem>
  ) => {
    const clearPaused =
      status === "InProgress" ||
      status === "Completed" ||
      typeof status === "object";
    const currentItem = queueSnapshot.current.data.items.find((item) => item.id === id);
    const nowSec = Math.floor(Date.now() / 1000);
    if (currentItem) {
      const prevStatus = currentItem.status;
      const nextIsTerminal =
        status === "Completed" || typeof status === "object";
      const prevIsTerminal =
        prevStatus === "Completed" || typeof prevStatus === "object";
      if (nextIsTerminal && (!prevIsTerminal || prevStatus !== status)) {
        logUploadQueueTerminal(currentItem, status);
      }
      uploadQueueStatusRef.current[id] = status;
    }
    const nextQueue: QueueData = {
      ...queueSnapshot.current.data,
      items: queueSnapshot.current.data.items.map((item) =>
        item.id === id
          ? {
              ...item,
              status,
              ...(status === "InProgress" && item.status !== "InProgress"
                ? {
                    attempts: (item.attempts ?? 0) + 1,
                    last_started_at: updates?.last_started_at ?? nowSec,
                    last_run_action: updates?.last_run_action ?? item.last_run_action ?? "new"
                  }
                : null),
              ...(status === "Completed"
                ? {
                    last_completed_at: updates?.last_completed_at ?? nowSec
                  }
                : null),
              ...(typeof status === "object"
                ? {
                    last_failed_at: updates?.last_failed_at ?? nowSec,
                    last_failed_bytes:
                      updates?.last_failed_bytes ?? item.last_failed_bytes,
                    last_failed_total_bytes:
                      updates?.last_failed_total_bytes ?? item.last_failed_total_bytes,
                    last_failed_files:
                      updates?.last_failed_files ?? item.last_failed_files,
                    last_failed_elapsed_sec:
                      updates?.last_failed_elapsed_sec ?? item.last_failed_elapsed_sec
                  }
                : null),
              ...(updates ? updates : null),
              ...(clearPaused ? { paused: false } : null)
            }
          : item
      )
    };
    await saveQueueData(nextQueue);
  };

  const handleRemoveQueueItem = async (id: number) => {
    const nextQueue: QueueData = {
      ...queueData,
      items: queueData.items.filter((item) => item.id !== id)
    };
    await saveQueueData(nextQueue);
  };

  const handleRequeueUploadItem = async (id: number) => {
    const nextQueue: QueueData = {
      ...queueData,
      items: queueData.items.map((item) => {
        if (item.id !== id) return item;
        return {
          ...item,
          paused: true,
          status: "Pending",
          last_run_action: "requeue",
          transfer_settings: {
            ...(item.transfer_settings || {}),
            resume_mode: "none",
            override_on_conflict: true
          }
        };
      })
    };
    await saveQueueData(nextQueue);
  };

  const moveUploadQueueItem = async (fromIndex: number, toIndex: number) => {
    if (fromIndex < 0 || toIndex < 0) return;
    if (fromIndex === toIndex) return;
    if (fromIndex >= queueData.items.length || toIndex >= queueData.items.length) return;
    const items = [...queueData.items];
    const moving = items[fromIndex];
    if (!moving || moving.status !== "Pending") return;
    if (!items[toIndex] || items[toIndex].status !== "Pending") return;
    items.splice(fromIndex, 1);
    items.splice(toIndex, 0, moving);
    await saveQueueData({ ...queueData, items });
    if (transferActive && currentQueueItemId) {
      const firstPendingIndex = items.findIndex((entry) => entry.status === "Pending");
      if (firstPendingIndex === toIndex) {
        pauseActiveUploadAndRequeue(items, Math.min(firstPendingIndex + 1, items.length));
      }
    }
  };

  const handleClearCompletedQueue = async () => {
    const nextQueue: QueueData = {
      ...queueData,
      items: queueData.items.filter(
        (item) => item.status !== "Completed" && typeof item.status !== "object"
      )
    };
    await saveQueueData(nextQueue);
  };

  const handleClearFailedQueue = async () => {
    const nextQueue: QueueData = {
      ...queueData,
      items: queueData.items.filter((item) => !isUploadFailed(item))
    };
    await saveQueueData(nextQueue);
  };

  const handleClearQueue = async () => {
    const nextQueue: QueueData = {
      ...queueData,
      items: []
    };
    setCurrentQueueItemId(null);
    setUploadQueueRunning(false);
    await saveQueueData(nextQueue);
  };

  const handleResumeFromHistory = (record: TransferRecord) => {
    setResumeRecord(record);
    setResumeQueueItem(null);
    setResumeChoice("size");
    setShowResumePrompt(true);
  };

  const handleResumeFromFailedItem = (item: QueueItem) => {
    setResumeQueueItem(item);
    setResumeRecord(null);
    setResumeChoice("size");
    setShowResumePrompt(true);
  };

  const startUploadWithParams = async (params: {
    sourcePath: string;
    destPath: string;
    resumeOverride?: ResumeOption;
  }) => {
    if (transferActive) {
      try {
        const status = await invoke<TransferStatusSnapshot>("transfer_status");
        const activeStates = [
          "Starting",
          "Scanning",
          "Uploading",
          "Uploading archive",
          "Extracting"
        ];
        const isActive = activeStates.some((state) => status.status.startsWith(state));
        if (isActive) {
          setTransferState((prev) => ({ ...prev, status: "Transfer already running" }));
          setClientLogs((prev) => [
            "Upload blocked: A transfer is already running. Cancel it first.",
            ...prev
          ].slice(0, 100));
          return;
        }
        await invoke("transfer_reset");
      } catch {
        setTransferState((prev) => ({ ...prev, status: "Transfer already running" }));
        setClientLogs((prev) => [
          "Upload blocked: A transfer is already running. Cancel it first.",
          ...prev
        ].slice(0, 100));
        return;
      }
    }
    if (!ip.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing IP" }));
      setClientLogs((prev) => ["Upload blocked: Missing IP", ...prev].slice(0, 100));
      return;
    }
    if (!params.sourcePath.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing source" }));
      setClientLogs((prev) => ["Upload blocked: Missing source", ...prev].slice(0, 100));
      return;
    }
    if (!params.destPath.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing destination" }));
      setClientLogs((prev) => ["Upload blocked: Missing destination", ...prev].slice(0, 100));
      return;
    }
    if (!(await ensureConnected())) {
      setTransferState((prev) => ({ ...prev, status: "Not connected" }));
      setClientLogs((prev) => ["Upload blocked: Not connected", ...prev].slice(0, 100));
      return;
    }
    if (payloadUnderLoad) {
      setNoticeTitle(tr("payload_status"));
      setNoticeLines([tr("payload_busy"), tr("try_again")]);
      setNoticeOpen(true);
      setClientLogs((prev) => ["Upload delayed: Payload is busy.", ...prev].slice(0, 100));
      return;
    }

    const resumeToUse = normalizeResumeMode(params.resumeOverride ?? resumeMode);
    const useTempForRun =
      resumeToUse !== "none" && useTemp
        ? false
        : useTemp;
    if (resumeToUse !== "none" && useTemp) {
      setNoticeTitle(tr("resume"));
      setNoticeLines([tr("resume_requires_direct")]);
      setNoticeOpen(true);
      pushClientLog("Resume forced Direct mode (Temp off).");
    }
    try {
      setClientLogs((prev) => [
        `Upload preparing: ${params.sourcePath} -> ${params.destPath}`,
        ...prev
      ].slice(0, 100));
      const shouldCheckDest = resumeToUse === "none" && !overrideOnConflict;
      const exists = shouldCheckDest
        ? await invoke<boolean>("transfer_check_dest", {
            ip,
            destPath: params.destPath
          })
        : false;
      if (exists && !overrideOnConflict) {
        setTransferState((prev) => ({ ...prev, status: tr("destination_exists") }));
        setClientLogs((prev) => [
          "Upload blocked: Destination already exists",
          ...prev
        ].slice(0, 100));
        return;
      }
      const runId = await invoke<number>("transfer_start", {
        req: {
          ip,
          source_path: params.sourcePath,
          dest_path: params.destPath,
          use_temp: useTempForRun,
          connections,
          resume_mode: resumeToUse,
          compression,
          bandwidth_limit_mbps: bandwidthLimit,
          auto_tune_connections: autoTune,
          optimize_upload: optimizeActive,
          chmod_after_upload: chmodAfterUpload,
          rar_extract_mode: rarExtractMode,
          rar_temp_root: rarTemp,
          override_on_conflict: overrideOnConflict,
          payload_version: payloadVersion,
          storage_root: storageRoot,
          required_size: transferState.total || null
        }
      });
      setActiveRunId(runId);
      setActiveTransferSource(params.sourcePath);
      setActiveTransferDest(params.destPath);
      setActiveTransferViaQueue(false);
      setTransferStartedAt(Date.now());
      const startMsg = `Upload started: ${params.sourcePath} -> ${params.destPath}`;
      setClientLogs((prev) => [startMsg, ...prev].slice(0, 100));
    } catch (err) {
      setClientLogs((prev) => [`Upload failed: ${String(err)}`, ...prev].slice(0, 100));
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
    }
  };

  const handleConfirmResumeFromHistory = () => {
    if (!resumeRecord) return;
    if (isArchivePath(resumeRecord.source_path)) {
      setNoticeTitle(tr("resume"));
      setNoticeLines([tr("resume_unsupported_archive")]);
      setNoticeOpen(true);
      setShowResumePrompt(false);
      setResumeRecord(null);
      return;
    }
    setSourcePath(resumeRecord.source_path);
    setFinalPathMode("manual");
    setFinalPath(resumeRecord.dest_path);
    setResumeMode(resumeChoice);
    setActiveTab("transfer");
    setShowResumePrompt(false);
    pushClientLog(`Loaded from history: ${resumeRecord.source_path}`);
    if (resumeRecord.dest_path) {
      pushClientLog(`Destination set: ${resumeRecord.dest_path}`);
    }
    pushClientLog(`Resume mode set: ${resumeChoice}.`);
    startUploadWithParams({
      sourcePath: resumeRecord.source_path,
      destPath: resumeRecord.dest_path,
      resumeOverride: resumeChoice
    });
    setResumeRecord(null);
  };

  const handleConfirmResumeFromQueue = async () => {
    if (!resumeQueueItem) return;
    if (isArchivePath(resumeQueueItem.source_path)) {
      setNoticeTitle(tr("resume"));
      setNoticeLines([tr("resume_unsupported_archive")]);
      setNoticeOpen(true);
      setShowResumePrompt(false);
      setResumeQueueItem(null);
      return;
    }
    const nextQueue: QueueData = {
      ...queueData,
      items: queueData.items.map((item) => {
        if (item.id !== resumeQueueItem.id) return item;
        return {
          ...item,
          status: "Pending",
          last_run_action: "resume",
          transfer_settings: {
            ...(item.transfer_settings || {}),
            resume_mode: resumeChoice,
            use_temp: false
          }
        };
      })
    };
    pushClientLog("Resume set to Direct mode (Temp off).");
    await saveQueueData(nextQueue);
    setUploadQueueTab("current");
    setShowResumePrompt(false);
    setResumeQueueItem(null);
  };

  const startQueueItem = async (item: QueueItem) => {
    if (transferActive) {
      return;
    }
    if (payloadUnderLoad) {
      pushClientLog("Upload queue paused: Payload is busy.", "warn");
      if (uploadQueueRetryTimeoutRef.current == null) {
        uploadQueueRetryTimeoutRef.current = window.setTimeout(() => {
          uploadQueueRetryTimeoutRef.current = null;
          processNextQueueItem();
        }, 10000);
      }
      return;
    }
    const targetIp = item.ps5_ip || ip.trim() || configSnapshot.current.ip;
    if (!targetIp || !targetIp.trim()) {
      await updateQueueItemStatus(item.id, { Failed: "Missing PS5 IP address." });
      pushClientLog("Upload failed to start: PS5 IP address is required.");
      setUploadQueueRunning(false);
      setCurrentQueueItemId(null);
      setActiveRunId(null);
      setActiveTransferSource("");
      setActiveTransferDest("");
      setActiveTransferViaQueue(false);
      setTransferStartedAt(null);
      return;
    }
    setUploadQueueRunning(true);
    const settings = item.transfer_settings;
    const base = item.storage_base || storageRoot;
    const dest = buildDestPathForItem(base, item);
    const itemResumeMode = normalizeResumeMode(settings?.resume_mode ?? resumeMode);
    const itemOverride = settings?.override_on_conflict ?? overrideOnConflict;
    let itemUseTemp = settings?.use_temp ?? useTemp;
    if (itemResumeMode !== "none" && itemUseTemp) {
      itemUseTemp = false;
      pushClientLog("Resume forced Direct mode (Temp off).", "warn");
    }
    if (itemResumeMode === "none" && !itemOverride) {
      const exists = await invoke<boolean>("transfer_check_dest", {
        ip: targetIp,
        destPath: dest
      });
      if (exists) {
        await updateQueueItemStatus(item.id, { Failed: tr("destination_exists") });
        setCurrentQueueItemId(null);
        pushClientLog(tr("destination_exists"));
        processNextQueueItem();
        return;
      }
    }
    pushClientLog(`Starting upload: ${item.source_path}`);
    const runAction =
      item.last_run_action === "requeue"
        ? "requeue"
        : itemResumeMode !== "none"
        ? "resume"
        : "new";
    await updateQueueItemStatus(item.id, "InProgress", {
      last_run_action: runAction
    });
    setCurrentQueueItemId(item.id);
    setActiveTransferSource(item.source_path);
    setActiveTransferDest(dest);
    setActiveTransferViaQueue(true);
    setTransferStartedAt(Date.now());
    try {
      const runId = await invoke<number>("transfer_start", {
        req: {
          ip: targetIp,
          source_path: item.source_path,
          dest_path: dest,
          use_temp: itemUseTemp,
          connections: settings?.connections ?? connections,
          resume_mode: itemResumeMode,
          compression: settings?.compression ?? compression,
          bandwidth_limit_mbps: settings?.bandwidth_limit_mbps ?? bandwidthLimit,
          auto_tune_connections: settings?.auto_tune_connections ?? autoTune,
          optimize_upload: settings?.optimize_upload ?? optimizeActive,
          chmod_after_upload: settings?.chmod_after_upload ?? chmodAfterUpload,
          override_on_conflict: settings?.override_on_conflict ?? overrideOnConflict,
          rar_extract_mode: settings?.rar_extract_mode ?? rarExtractMode,
          rar_temp_root: settings?.rar_temp_root ?? rarTemp,
          payload_version: payloadVersion,
          storage_root: base,
          required_size: item.size_bytes || null
        }
      });
      if (typeof runId !== "number" || Number.isNaN(runId)) {
        throw new Error("Transfer start failed.");
      }
      setActiveRunId(runId);
    } catch (err) {
      const message = String(err);
      if (message.includes("Transfer already running")) {
        setCurrentQueueItemId(null);
        setActiveTransferSource("");
        setActiveTransferDest("");
        setActiveTransferViaQueue(false);
        setTransferStartedAt(null);
        setUploadQueueRunning(true);
        pushClientLog("Transfer already running. Waiting to retry queue...");
        return;
      }
      await updateQueueItemStatus(item.id, { Failed: message });
      setCurrentQueueItemId(null);
      setActiveRunId(null);
      setActiveTransferSource("");
      setActiveTransferDest("");
      setActiveTransferViaQueue(false);
      setTransferStartedAt(null);
      pushClientLog(`Upload failed to start: ${message}`);
      processNextQueueItem();
    }
  };

  const processNextQueueItem = async () => {
    if (transferActive) {
      return;
    }
    const next = queueSnapshot.current.data.items.find(
      (item) => item.status === "Pending"
    );
    if (next) {
      await startQueueItem(next);
    } else {
      // Queue is done, reset all transfer state to re-enable UI
      setCurrentQueueItemId(null);
      setActiveRunId(null);
      setActiveTransferSource("");
      setActiveTransferDest("");
      setActiveTransferViaQueue(false);
      setTransferStartedAt(null);
      setUploadQueueRunning(false);
    }
  };

  const handleUploadQueue = async () => {
    if (!ip.trim()) {
      setConnectionStatus("Missing IP");
      return;
    }
    if (!(await ensureConnected())) {
      return;
    }
    if (transferActive) {
      pushClientLog("Upload already running.");
      return;
    }
    pushClientLog("Starting upload queue...");
    if (currentQueueItemId && !transferActive) {
      setCurrentQueueItemId(null);
    }
    if (!transferActive) {
      const stalled = queueSnapshot.current.data.items.filter(
        (item) => item.status === "InProgress"
      );
      if (stalled.length > 0) {
        const nextItems = queueSnapshot.current.data.items.map((item) => {
          if (item.status !== "InProgress") return item;
          return {
            ...item,
            paused: true,
            status: "Pending",
            last_run_action: "resume",
            transfer_settings: {
              ...(item.transfer_settings || {}),
              resume_mode: "size"
            }
          };
        });
        await saveQueueData({ ...queueSnapshot.current.data, items: nextItems });
        pushClientLog(`Recovered ${stalled.length} stalled upload item(s).`);
      }
    }
    setUploadQueueRunning(true);
    const hasPending = queueSnapshot.current.data.items.some(
      (item) => item.status === "Pending"
    );
    if (!hasPending) {
      const hasCurrent = queueSnapshot.current.data.items.some(
        (item) => item.status === "InProgress" || item.status === "Pending"
      );
      if (!hasCurrent) {
        pushClientLog("Queue is empty or no pending items.");
        setUploadQueueRunning(false);
        return;
      }
      const nextItems = queueSnapshot.current.data.items.map((item) => {
        if (item.status === "InProgress") {
          return {
            ...item,
            paused: true,
            status: "Pending",
            transfer_settings: {
              ...(item.transfer_settings || {}),
              resume_mode: "size"
            }
          };
        }
        return item;
      });
      await saveQueueData({ ...queueSnapshot.current.data, items: nextItems });
    }
    try {
      await processNextQueueItem();
    } catch (err) {
      setUploadQueueRunning(false);
      pushClientLog(`Queue failed to start: ${String(err)}`);
    }
  };

  const pauseActiveUploadAndRequeue = async (items: QueueItem[], targetIndex: number) => {
    if (!currentQueueItemId) return;
    try {
      await invoke("transfer_cancel");
    } catch (err) {
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
      return;
    }

    const nextItems = [...items];
    const currentIndex = nextItems.findIndex((entry) => entry.id === currentQueueItemId);
    if (currentIndex >= 0) {
      const [activeItem] = nextItems.splice(currentIndex, 1);
      activeItem.paused = true;
      activeItem.transfer_settings = {
        ...(activeItem.transfer_settings || {}),
        resume_mode: "size"
      };
      activeItem.status = "Pending";
      const insertIndex = Math.min(Math.max(targetIndex, 0), nextItems.length);
      nextItems.splice(insertIndex, 0, activeItem);
    }

    await saveQueueData({ ...queueSnapshot.current.data, items: nextItems });
    setTransferState((prev) => ({ ...prev, status: "Paused" }));
    setCurrentQueueItemId(null);
    setActiveRunId(null);
    setActiveTransferSource("");
    setActiveTransferDest("");
    setActiveTransferViaQueue(false);
    setTransferStartedAt(null);
    setUploadQueueRunning(true);
    processNextQueueItem();
  };

  const handleCancel = async () => {
    try {
      if (currentQueueItemId) {
        await invoke("transfer_cancel");
        const nowSec = Math.floor(Date.now() / 1000);
        const elapsedSec = transferStartedAt
          ? Math.max(0, Math.floor((Date.now() - transferStartedAt) / 1000))
          : 0;
        await updateQueueItemStatus(
          currentQueueItemId,
          { Failed: USER_STOPPED_SENTINEL },
          {
            last_failed_at: nowSec,
            last_failed_bytes: transferState.sent,
            last_failed_total_bytes: transferState.total,
            last_failed_files: transferState.files,
            last_failed_elapsed_sec: elapsedSec
          }
        );
        setTransferState((prev) => ({ ...prev, status: tr("stopped") }));
        setCurrentQueueItemId(null);
        setActiveRunId(null);
        setActiveTransferSource("");
        setActiveTransferDest("");
        setActiveTransferViaQueue(false);
        setTransferStartedAt(null);
        setUploadQueueRunning(false);
        pushClientLog("Upload stopped by user.");
        return;
      }
      await invoke("transfer_cancel");
      setTransferState((prev) => ({ ...prev, status: tr("stopped") }));
      // Reset transfer state to re-enable UI
      setActiveRunId(null);
      setActiveTransferSource("");
      setActiveTransferDest("");
      setActiveTransferViaQueue(false);
      setTransferStartedAt(null);
      setUploadQueueRunning(false);
    } catch (err) {
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
    }
  };

  const handleResetTransfer = () => {
    setSourcePath("");
    setStorageRoot("/data");
    setPreset(presetOptions[0]);
    setCustomPreset("");
    setSubfolder("");
    setFinalPathMode("auto");
    setFinalPath("");
    setOverrideOnConflict(true);
    setCompression("auto");
    setResumeMode("none");
    setConnections(4);
    setBandwidthLimit(0);
    setOptimizeMode("none");
    restoreOptimizeSnapshot();
    setAutoTune(true);
    setUseTemp(false);
    setTransferState({
      status: "Idle",
      sent: 0,
      total: 0,
      files: 0,
      elapsed: 0,
      currentFile: ""
    });
  };

  const handleManageSelectIndex = (index: number | null) => {
    if (index === null) {
      manageSelectionRef.current = null;
      setManageSelected(null);
      return;
    }
    const entry = manageEntries[index];
    if (!entry) {
      manageSelectionRef.current = null;
      setManageSelected(null);
      return;
    }
    manageSelectionRef.current = {
      name: entry.name,
      type: getEntryType(entry)
    };
    setManageSelected(index);
  };

  const handleManageOpenDir = async (entry: DirEntry) => {
    if (getEntryType(entry) !== "dir") return;
    const nextPath = joinRemote(managePathRef.current, entry.name);
    await handleManageRefresh(nextPath);
    manageSelectionRef.current = null;
    setManageSelected(null);
  };

  const loadManageDirectory = async (targetPath: string) => {
    const normalized = targetPath && targetPath.trim() ? targetPath : "/";
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    if (manageLoadInFlight.current.has(normalized)) return;
    manageLoadInFlight.current.add(normalized);
    setManageStatus("Loading...");
    try {
      const snapshot = await invoke<ManageListSnapshot>("manage_list_refresh", {
        ip,
        path: normalized
      });
      managePathRef.current = snapshot.path || normalized;
      setManagePath(managePathRef.current);
      applyManageSnapshot(snapshot, { force: true });
    } catch (err) {
      setManageStatus(`Error: ${String(err)}`);
    } finally {
      manageLoadInFlight.current.delete(normalized);
    }
  };

  const handleManageRefresh = async (pathOverride?: string) => {
    const override = typeof pathOverride === "string" ? pathOverride : undefined;
    const targetPath = override ?? managePath;
    await loadManageDirectory(targetPath);
  };

  const handleManageUp = () => {
    if (managePath === "/") return;
    const parts = managePath.split("/").filter(Boolean);
    parts.pop();
    const next = `/${parts.join("/")}`;
    handleManageRefresh(next || "/");
  };

  const refreshManageDest = async (pathOverride?: string) => {
    if (!ip.trim()) {
      setManageDestStatus("Not connected");
      return;
    }
    const override = typeof pathOverride === "string" ? pathOverride : undefined;
    const targetPath = override ?? manageDestPath;
    setManageDestStatus("Loading...");
    try {
      const entries = await invoke<DirEntry[]>("manage_list", {
        ip,
        path: targetPath
      });
      const dirs = sortEntries(
        entries.filter((entry) => getEntryType(entry) === "dir"),
        manageSort
      );
      setManageDestEntries(dirs);
      setManageDestSelected(null);
      setManageDestStatus(`Loaded ${dirs.length} folders`);
    } catch (err) {
      setManageDestStatus(`Error: ${String(err)}`);
    }
  };

  const handleManageDestUp = () => {
    if (manageDestPath === "/") return;
    const parts = manageDestPath.split("/").filter(Boolean);
    parts.pop();
    const next = `/${parts.join("/")}`;
    const target = next || "/";
    setManageDestPath(target);
    refreshManageDest(target);
  };

  const handleOpenDestPicker = (action: ManageAction) => {
    if (!manageSelectedEntry) return;
    if (action === "Extract") {
      const isRar = manageSelectedEntry.name.toLowerCase().endsWith(".rar");
      if (!isRar) {
        setManageStatus("Extract only supports .rar files");
        return;
      }
    }
    setManageDestAction(action);
    setManageDestOpen(true);
    const nextPath = storageRoot || managePath || "/data";
    setManageDestPath(nextPath);
    setManageDestSelected(null);
    if (getEntryType(manageSelectedEntry) === "file") {
      setManageDestFilename(manageSelectedEntry.name);
    } else {
      setManageDestFilename("");
    }
    refreshManageDest(nextPath);
  };

  const handleConfirmDest = async () => {
    if (!manageSelectedEntry || !manageDestAction) return;
    const destEntry =
      manageDestSelected !== null ? manageDestEntries[manageDestSelected] : null;
    const destBase = destEntry
      ? joinRemote(manageDestPath, destEntry.name)
      : manageDestPath;
    const srcPath = joinRemote(managePath, manageSelectedEntry.name);
    const destName = getEntryType(manageSelectedEntry) === "file"
      ? (manageDestFilename.trim() || manageSelectedEntry.name)
      : manageSelectedEntry.name;
    const dstPath = joinRemote(destBase, destName);

    setManageDestOpen(false);
    setManageBusy(true);
    setManageStatus(`${manageDestAction}...`);
    if (manageDestAction === "Move") {
      setManageModalOpen(true);
      setManageModalDone(false);
      setManageModalError(null);
      setManageModalSummary(null);
      setManageModalOp("Move");
      manageModalOpRef.current = "Move";
      setManageModalStatus("Moving...");
      const startAt = Date.now();
      setManageModalStartedAt(startAt);
      manageModalStartedAtRef.current = startAt;
    }
    try {
      if (manageDestAction === "Move") {
        await invoke("manage_copy", { ip, src_path: srcPath, dst_path: dstPath });
        await invoke("manage_delete", { ip, path: srcPath });
        setManageModalDone(true);
        setManageModalError(null);
        setManageModalStatus("Done");
      } else if (manageDestAction === "Copy") {
        setManageModalOpen(true);
        setManageModalDone(false);
        setManageModalError(null);
        setManageModalSummary(null);
        setManageModalOp("Copy");
        manageModalOpRef.current = "Copy";
        setManageModalStatus("Copying...");
        const startAt = Date.now();
        setManageModalStartedAt(startAt);
        manageModalStartedAtRef.current = startAt;
        await invoke("manage_copy", { ip, src_path: srcPath, dst_path: dstPath });
      } else if (manageDestAction === "Extract") {
        await handleQueueExtract(srcPath, destBase);
        setManageBusy(false);
        setManageStatus("Extraction queued.");
      }
    } catch (err) {
      setManageBusy(false);
      setManageStatus(`Error: ${String(err)}`);
      if (manageDestAction === "Move") {
        setManageModalDone(true);
        setManageModalError(String(err));
        setManageModalStatus("Failed");
      }
    } finally {
      setManageDestAction(null);
    }
  };

  const handleManageCancel = async () => {
    try {
      await invoke("manage_cancel");
      setManageStatus("Cancelling...");
      setManageModalOpen(false);
      setManageModalStatus("");
      setManageModalSummary(null);
      setManageModalStartedAt(null);
      manageModalStartedAtRef.current = null;
      setManageModalLastProgressAt(null);
    } catch (err) {
      setClientLogs((prev) => [
        `Failed to cancel: ${String(err)}`,
        ...prev
      ]);
    }
  };

  const handleManageResetUI = async () => {
    try {
      await invoke("manage_cancel");
    } catch (err) {
      pushClientLog(`Manage cancel failed: ${String(err)}`, "warn");
    }
    setManageBusy(false);
    setManageStatus(isConnected ? "Connected" : "Not connected");
    setManageEntries([]);
    setManageSelected(null);
    manageSelectionRef.current = null;
    setManageMeta(null);
    setManageCoverUrl(null);
    setManageModalOpen(false);
    setManageModalDone(false);
    setManageModalError(null);
    setManageModalOp("");
    setManageModalStatus("");
    setManageModalSummary(null);
    setManageModalStartedAt(null);
    manageModalStartedAtRef.current = null;
    setManageModalLastProgressAt(null);
    setManageProgress({
      op: "",
      processed: 0,
      total: 0,
      currentFile: "",
      speed_bps: 0
    });
    setManageDestOpen(false);
    setManageDestAction(null);
    setManageDestStatus("");
    setManageDestEntries([]);
    setManageDestSelected(null);
    setManageDestFilename("");
    setShowRenamePrompt(false);
    setShowDeleteConfirm(false);
    setShowCreatePrompt(false);
    setRenameValue("");
    setNewFolderName("");
    pushClientLog("Manage UI reset.", "info");
  };

  const handleManageUpload = async () => {
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    const selected = await open({ multiple: true, directory: false });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    if (!paths.length) return;
    setManageBusy(true);
    setManageModalOpen(true);
    setManageModalDone(false);
    setManageModalError(null);
    setManageModalSummary(null);
    setManageModalOp("Upload");
    manageModalOpRef.current = "Upload";
    setManageModalStatus("Uploading...");
    const startAt = Date.now();
    setManageModalStartedAt(startAt);
    manageModalStartedAtRef.current = startAt;
    setManageStatus(
      transferActive ? "Uploading (deprioritized)..." : "Uploading..."
    );
    try {
      await invoke("manage_upload", { ip, dest_root: managePath, paths });
    } catch (err) {
      setManageBusy(false);
      setManageStatus(`Error: ${String(err)}`);
    }
  };

  const handleManageUploadFolder = async () => {
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    const selected = await open({ multiple: true, directory: true });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    if (!paths.length) return;
    setManageBusy(true);
    setManageModalOpen(true);
    setManageModalDone(false);
    setManageModalError(null);
    setManageModalSummary(null);
    setManageModalOp("Upload");
    manageModalOpRef.current = "Upload";
    setManageModalStatus("Uploading...");
    const startAt = Date.now();
    setManageModalStartedAt(startAt);
    manageModalStartedAtRef.current = startAt;
    setManageStatus(
      transferActive ? "Uploading (deprioritized)..." : "Uploading..."
    );
    try {
      await invoke("manage_upload", { ip, dest_root: managePath, paths });
    } catch (err) {
      setManageBusy(false);
      setManageStatus(`Error: ${String(err)}`);
    }
  };

  const joinLocalPath = (root: string, name: string) => {
    const separator = root.includes("\\") ? "\\" : "/";
    return `${root.replace(/[\\/]+$/, "")}${separator}${name}`;
  };

  const handleManageDownload = async () => {
    if (!manageSelectedEntry) return;
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    const srcPath = joinRemote(managePath, manageSelectedEntry.name);
    try {
      setManageBusy(true);
      setManageStatus(
        transferActive ? "Downloading (deprioritized)..." : "Downloading..."
      );
      if (getEntryType(manageSelectedEntry) === "dir") {
        const destRoot = await open({
          directory: true,
          multiple: false,
          defaultPath: "temp"
        });
        if (!destRoot || typeof destRoot !== "string") {
          setManageBusy(false);
          return;
        }
        const destPath = joinLocalPath(destRoot, manageSelectedEntry.name);
        setManageModalOpen(true);
        setManageModalDone(false);
        setManageModalError(null);
        setManageModalSummary(null);
        setManageModalOp("Download");
        manageModalOpRef.current = "Download";
        setManageModalStatus("Downloading...");
        const startAt = Date.now();
        setManageModalStartedAt(startAt);
        manageModalStartedAtRef.current = startAt;
        await invoke("manage_download_dir", {
          ip,
          path: srcPath,
          dest_path: destPath,
          compression: downloadCompression
        });
      } else {
        const destPath = await save({
          defaultPath: `temp/${manageSelectedEntry.name}`
        });
        if (!destPath) {
          setManageBusy(false);
          return;
        }
        setManageModalOpen(true);
        setManageModalDone(false);
        setManageModalError(null);
        setManageModalSummary(null);
        setManageModalOp("Download");
        manageModalOpRef.current = "Download";
        setManageModalStatus("Downloading...");
        const startAt = Date.now();
        setManageModalStartedAt(startAt);
        manageModalStartedAtRef.current = startAt;
        await invoke("manage_download_file", {
          ip,
          path: srcPath,
          dest_path: destPath
        });
      }
    } catch (err) {
      setManageBusy(false);
      setManageStatus(`Error: ${String(err)}`);
    }
  };

  const handleManageRename = () => {
    if (!manageSelectedEntry) return;
    setRenameValue(manageSelectedEntry.name);
    setShowRenamePrompt(true);
  };

  const handleConfirmRename = async () => {
    if (!manageSelectedEntry) return;
    const nextName = renameValue.trim();
    if (!nextName) return;
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    const srcPath = joinRemote(managePath, manageSelectedEntry.name);
    const dstPath = joinRemote(managePath, nextName);
    setShowRenamePrompt(false);
    setManageBusy(true);
    setManageStatus("Renaming...");
    try {
      await invoke("manage_rename", { ip, src_path: srcPath, dst_path: dstPath });
    } catch (err) {
      setManageBusy(false);
      setManageStatus(`Error: ${String(err)}`);
    }
  };

  const handleManageDelete = () => {
    if (!manageSelectedEntry) return;
    setShowDeleteConfirm(true);
  };

  const handleConfirmDelete = async () => {
    if (!manageSelectedEntry) return;
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    const target = joinRemote(managePath, manageSelectedEntry.name);
    setShowDeleteConfirm(false);
    setManageBusy(true);
    setManageStatus("Deleting...");
    setManageModalOpen(true);
    setManageModalDone(false);
    setManageModalError(null);
    setManageModalSummary(null);
    setManageModalOp("Delete");
    manageModalOpRef.current = "Delete";
    setManageModalStatus("Deleting...");
    const startAt = Date.now();
    setManageModalStartedAt(startAt);
    manageModalStartedAtRef.current = startAt;
    try {
      await invoke("manage_delete", { ip, path: target });
    } catch (err) {
      setManageBusy(false);
      setManageStatus(`Error: ${String(err)}`);
    }
  };

  const handleManageCreateFolder = () => {
    setNewFolderName("");
    setShowCreatePrompt(true);
  };

  const handleConfirmCreateFolder = async () => {
    const name = newFolderName.trim();
    if (!name) return;
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    const target = joinRemote(managePath, name);
    setShowCreatePrompt(false);
    setManageBusy(true);
    setManageStatus("Creating folder...");
    try {
      await invoke("manage_create_dir", { ip, path: target });
    } catch (err) {
      setManageBusy(false);
      setManageStatus(`Error: ${String(err)}`);
    }
  };

  const handleManageChmod = async () => {
    if (!manageSelectedEntry) return;
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    const target = joinRemote(managePath, manageSelectedEntry.name);
    setManageBusy(true);
    setManageStatus("Applying chmod...");
    try {
      await invoke("manage_chmod", { ip, path: target });
    } catch (err) {
      setManageBusy(false);
      setManageStatus(`Error: ${String(err)}`);
    }
  };

  const handleFaqReload = async () => {
    setFaqLoading(true);
    setFaqError(null);
    try {
      const content = await invoke<string>("faq_load");
      setFaqContent(content);
    } catch (err) {
      setFaqError(String(err));
    } finally {
      setFaqLoading(false);
    }
  };

  const handleChatSend = async () => {
    if (!chatEnabled) {
      setChatStatus("Chat unavailable");
      return;
    }
    const name = chatDisplayName.trim();
    if (!name) {
      setChatStatus("Set a display name");
      return;
    }
    const text = chatInput.trim();
    if (!text) return;
    try {
      await invoke("chat_send", { name, text });
      setChatInput("");
    } catch (err) {
      setChatStatus(`Send failed: ${String(err)}`);
    }
  };

  const handleChatConnect = async () => {
    try {
      setChatStatus("Connecting...");
      const started = await invoke<ChatInfo>("chat_start");
      setChatRoomId(started.room_id);
      setChatEnabled(started.enabled);
    } catch (err) {
      setChatStatus(`Error: ${String(err)}`);
    }
  };

  const handleChatDisconnect = async () => {
    try {
      await invoke("chat_stop");
      setChatEnabled(false);
      setChatStatus("Disconnected");
    } catch (err) {
      setChatStatus(`Error: ${String(err)}`);
    }
  };

  const handleChatRefresh = async () => {
    try {
      const info = await invoke<ChatInfo>("chat_info");
      setChatRoomId(info.room_id);
      setChatEnabled(info.enabled);
    } catch (err) {
      setChatStatus(`Error: ${String(err)}`);
    }
  };

  const status = useMemo(
    () => ({
      connection: connectionStatus,
      payload: payloadStatus,
      transfer: transferState.status,
      storage: storageRoot
    }),
    [connectionStatus, payloadStatus, storageRoot, transferState.status]
  );
  const runningExtractItem = payloadFullStatus?.items.find((item) => item.status === "running") ?? null;
  const extractionItems = payloadFullStatus?.items ?? [];
  const uploadPendingIndices = queueData.items.reduce((acc: number[], item, index) => {
    if (item.status === "Pending") acc.push(index);
    return acc;
  }, []);
  const extractionPendingIndices = extractionItems.reduce((acc: number[], item, index) => {
    if (item.status === "pending") acc.push(index);
    return acc;
  }, []);
  const manageActionLabel = manageDestAction
    ? manageDestAction === "Move"
      ? tr("move")
      : manageDestAction === "Copy"
      ? tr("copy")
      : tr("extract")
    : null;

  const isUploadFailed = (item: QueueItem) => typeof item.status === "object";
  const isUploadCompleted = (item: QueueItem) => item.status === "Completed";
  const isUploadCurrent = (item: QueueItem) =>
    item.status === "Pending" || item.status === "InProgress";
  const uploadCurrentCount = queueData.items.filter(isUploadCurrent).length;
  const uploadCompletedCount = queueData.items.filter(isUploadCompleted).length;
  const uploadFailedCount = queueData.items.filter(isUploadFailed).length;
  const hasUploadCurrent = queueData.items.some(isUploadCurrent);
  const hasUploadCompleted = queueData.items.some(isUploadCompleted);
  const hasUploadFailed = queueData.items.some(isUploadFailed);
  const hasUploadPending = queueData.items.some((item) => item.status === "Pending");
  const hasUploadStalled = !transferActive && queueData.items.some((item) => item.status === "InProgress");
  const hasUploadStartable = hasUploadPending || hasUploadStalled;

  const historyDurationByKey = useMemo(() => {
    const map = new Map<string, { success?: number | null; failed?: number | null }>();
    for (let i = historyData.records.length - 1; i >= 0; i -= 1) {
      const record = historyData.records[i];
      const key = `${record.source_path}||${record.dest_path}`;
      const existing = map.get(key) || {};
      if (record.success && existing.success == null) {
        existing.success = record.duration_secs ?? null;
      } else if (!record.success && existing.failed == null) {
        existing.failed = record.duration_secs ?? null;
      }
      map.set(key, existing);
      if (existing.success != null && existing.failed != null) {
        continue;
      }
    }
    return map;
  }, [historyData.records]);

  const hasExtractionItems = extractionItems.length > 0;
  const isExtractionFailed = (item: ExtractQueueItem) =>
    item.status === "failed";
  const isExtractionCompleted = (item: ExtractQueueItem) =>
    item.status === "complete";
  const extractionCurrentCount = extractionItems.filter(
    (item) => !isExtractionCompleted(item) && !isExtractionFailed(item)
  ).length;
  const extractionCompletedCount = extractionItems.filter(isExtractionCompleted).length;
  const extractionFailedCount = extractionItems.filter(isExtractionFailed).length;
  const hasExtractionRunning = extractionItems.some((item) => item.status === "running");
  const hasExtractionCurrent = extractionItems.some(
    (item) => !isExtractionCompleted(item) && !isExtractionFailed(item)
  );
  const hasExtractionFailed = extractionItems.some(isExtractionFailed);
  const hasExtractionCompleted = extractionItems.some(isExtractionCompleted);
  const hasExtractionPending = extractionItems.some(
    (item) => item.status === "pending" || item.status === "idle"
  );
  const payloadUnderLoad = !!payloadFullStatus?.is_busy || hasExtractionRunning;

  const keepAwakeActivity =
    transferActive ||
    hasUploadCurrent ||
    hasExtractionCurrent ||
    manageBusy ||
    payloadFullStatus?.is_busy;

  useEffect(() => {
    if (keepAwakeMode !== "auto") {
      return;
    }
    if (keepAwakeActivity) {
      if (keepAwakeAutoTimeoutRef.current != null) {
        window.clearTimeout(keepAwakeAutoTimeoutRef.current);
        keepAwakeAutoTimeoutRef.current = null;
      }
      setKeepAwakeAutoHold(true);
      return;
    }
    if (keepAwakeAutoTimeoutRef.current == null) {
      setKeepAwakeAutoHold(true);
      keepAwakeAutoTimeoutRef.current = window.setTimeout(() => {
        setKeepAwakeAutoHold(false);
        keepAwakeAutoTimeoutRef.current = null;
      }, KEEP_AWAKE_IDLE_MS);
    }
  }, [keepAwakeMode, keepAwakeActivity, KEEP_AWAKE_IDLE_MS]);

  const shouldKeepAwake =
    keepAwakeMode === "on" ? true : keepAwakeMode === "auto" ? keepAwakeAutoHold : false;

  useEffect(() => {
    if (!configLoaded) return;
    invoke("sleep_set", { enabled: shouldKeepAwake }).catch(() => {
      // ignore keep awake failures
    });
  }, [shouldKeepAwake, configLoaded]);

  const maintenanceBlocked =
    !isConnected ||
    !ip.trim() ||
    transferActive ||
    hasExtractionRunning ||
    hasExtractionPending ||
    payloadFullStatus?.is_busy;

  const runPayloadMaintenance = async (reason: string) => {
    if (maintenanceBlocked) return;
    const now = Date.now();
    if (now - lastMaintenanceAtRef.current < 5 * 60 * 1000) return;
    lastMaintenanceAtRef.current = now;
    maintenanceRequestedRef.current = false;
    try {
      const response = await invoke<string>("payload_maintenance", { ip });
      if (response && !response.startsWith("BUSY")) {
        pushPayloadLog(`Maintenance (${reason}): ${response}`);
      }
    } catch (err) {
      pushPayloadLog(`Maintenance failed: ${String(err)}`);
    }
  };

  useEffect(() => {
    if (maintenanceBlocked) return;
    const interval = setInterval(() => {
      runPayloadMaintenance("interval");
    }, 5 * 60 * 1000);
    return () => clearInterval(interval);
  }, [maintenanceBlocked, ip]);

  useEffect(() => {
    if (!maintenanceRequestedRef.current) return;
    runPayloadMaintenance("transition");
  }, [maintenanceBlocked, ip, transferState.status, payloadLastUpdated]);

  const clientAsset = updateInfo
    ? selectClientAsset(updateInfo.assets, platformInfo)
    : null;

  return (
    <div
      className="app"
      dir={isRtl ? "rtl" : "ltr"}
      onContextMenu={(event) => event.preventDefault()}
    >
      <div className="ambient" aria-hidden="true" />

      <header className="header shell">
        <div
          className="header-main app-drag-region"
          onDoubleClick={handleToggleMaximize}
        >
          <div className="brand">
            <div className="brand-logo-wrap">
              <img className="brand-logo" src="logo.png" alt="PS5Upload" />
            </div>
            <div className="brand-text">
              <span className="brand-title">PS5Upload</span>
              <span className="brand-sub">v{appVersion}</span>
            </div>
          </div>
        </div>
        <div className="header-actions">
          <button
            className="btn coffee"
            onClick={() => openExternal("https://ko-fi.com/B0B81S0WUA")}
            title={tr("buy_coffee")}
          >
            <span className="icon-coffee">♥</span> {tr("buy_coffee")}
          </button>
          <button
            className="btn discord"
            onClick={() => openExternal("https://discord.gg/fzK3xddtrM")}
            title="Discord server"
          >
            <span className="icon-discord" aria-hidden="true">
              <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
                <path d="M19.54 4.86C18.05 4.18 16.46 3.7 14.8 3.5c-.2.36-.43.84-.59 1.23-1.76-.26-3.5-.26-5.22 0-.16-.39-.4-.87-.6-1.23-1.66.2-3.25.68-4.74 1.36C1.42 8.06.82 11.2 1.12 14.28c1.55 1.15 3.05 1.85 4.53 2.31.36-.5.68-1.03.96-1.6-.52-.2-1.02-.45-1.5-.75.12-.09.24-.19.35-.29 2.9 1.35 6.05 1.35 8.92 0 .12.1.24.2.36.29-.48.3-.98.56-1.5.75.28.57.6 1.1.96 1.6 1.48-.46 2.98-1.16 4.53-2.31.35-3.55-.6-6.66-2.71-9.42ZM8.6 13.41c-.87 0-1.58-.8-1.58-1.78 0-.99.7-1.79 1.58-1.79.87 0 1.58.8 1.58 1.79 0 .98-.7 1.78-1.58 1.78Zm6.82 0c-.87 0-1.58-.8-1.58-1.78 0-.99.7-1.79 1.58-1.79.87 0 1.58.8 1.58 1.79 0 .98-.7 1.78-1.58 1.78Z"/>
              </svg>
            </span>
            Discord
          </button>
          <button
            className={`btn ${
              keepAwakeMode === "on"
                ? "success"
                : keepAwakeMode === "auto"
                ? "warning"
                : "danger"
            }`}
            onClick={handleToggleKeepAwake}
            title={tr("keep_awake")}
          >
            {tr("keep_awake")}{" "}
            {keepAwakeMode === "on"
              ? tr("on")
              : keepAwakeMode === "auto"
              ? tr("auto")
              : tr("off")}
          </button>
          <button
            className={`btn ghost theme-toggle ${theme}`}
            onClick={handleToggleTheme}
            aria-label={
              theme === "dark" ? tr("switch_light_mode") : tr("switch_dark_mode")
            }
            title={theme === "dark" ? tr("switch_light_mode") : tr("switch_dark_mode")}
          >
            {theme === "dark" ? "◐" : "◑"}
          </button>
          <div className="lang-switch" aria-label={tr("language")} ref={langMenuRef}>
            <span>{tr("language")}</span>
            <div className={`lang-select ${langMenuOpen ? "open" : ""}`}>
              <button
                className="lang-select-trigger"
                type="button"
                aria-haspopup="listbox"
                aria-expanded={langMenuOpen}
                onClick={() => setLangMenuOpen((prev) => !prev)}
              >
                <span className="lang-select-label">{currentLanguageLabel}</span>
                <span className="lang-select-caret">▾</span>
              </button>
              {langMenuOpen && (
                <div className="lang-select-menu" role="listbox">
                  {languages.map((item) => (
                    <button
                      key={item.code}
                      type="button"
                      role="option"
                      aria-selected={item.code === language}
                      className={`lang-select-option ${item.code === language ? "active" : ""}`}
                      onClick={() => {
                        setLanguage(item.code);
                        setLangMenuOpen(false);
                      }}
                    >
                      <span className="lang-native">{item.label}</span>
                      <span className="lang-code">{item.code}</span>
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>
          <div className="window-controls">
            <button
              className="window-btn minimize"
              onClick={handleMinimize}
              aria-label={tr("minimize")}
              title={tr("minimize")}
            >
              —
            </button>
            <button
              className="window-btn maximize"
              onClick={handleToggleMaximize}
              aria-label={tr("maximize")}
              title={tr("maximize")}
            >
              □
            </button>
            <button
              className="window-btn close"
              onClick={handleCloseWindow}
              aria-label={tr("close")}
              title={tr("close")}
            >
              ×
            </button>
          </div>
        </div>
      </header>

      <aside className="panel left shell">
        <section className="card">
          <header className="card-title">
            <span className="card-title-icon">●</span>
            {tr("connect")}
          </header>
          <label className="field inline">
            <span>{tr("profile")}</span>
            <div className="inline-field profile-controls">
              <select
                value={profileSelectValue || currentProfile || ""}
                onChange={(event) =>
                  handleSelectProfile(event.target.value || null)
                }
              >
                {profilesData.profiles.length === 0 && (
                  <option value="">{tr("none")}</option>
                )}
                {profilesData.profiles.map((profile) => (
                  <option key={profile.name} value={profile.name}>
                    {profile.name}
                  </option>
                ))}
                <option value={NEW_PROFILE_OPTION}>{tr("new_profile")}</option>
              </select>
              <button
                className="btn"
                onClick={handleSaveCurrentProfile}
                disabled={!currentProfile}
              >
                {tr("save_current_profile")}
              </button>
              <button
                className="btn danger"
                onClick={handleDeleteProfile}
                disabled={!currentProfile}
              >
                {tr("delete_profile")}
              </button>
            </div>
          </label>
          <label className="field">
            <span>{tr("ps5_address")}</span>
            <input
              placeholder="192.168.0.105"
              value={ip}
              onChange={(event) => setIp(event.target.value)}
            />
          </label>
          <div className="split">
            <button
              className="btn success"
              onClick={handleConnect}
              disabled={isConnecting || connectCooldown}
            >
              {tr("connect_btn")}
            </button>
            <button
              className="btn ghost"
              onClick={handleDisconnect}
              disabled={!isConnected && !isConnecting}
            >
              {tr("disconnect")}
            </button>
          </div>
          <div className="status-grid">
            <div>
              <p>{tr("state")}</p>
              <strong>{status.connection}</strong>
            </div>
            <div>
              <p>{tr("storage")}</p>
              <strong>{status.storage}</strong>
            </div>
          </div>
          <div className="card-divider" />
          <label className="field inline">
            <span>{tr("auto_connect")}</span>
            <input
              type="checkbox"
              checked={autoConnect}
              onChange={(event) => setAutoConnect(event.target.checked)}
            />
          </label>
        </section>

        <section className="card">
          <header className="card-title">
            <span className="card-title-icon">▸</span>
            {tr("payload")}
          </header>
          <p className="muted">{tr("payload_required")}</p>
          <label className="field">
            <span>{tr("payload_source")}</span>
            <select
              value={payloadReloadMode}
              onChange={(event) =>
                setPayloadReloadMode(event.target.value as "local" | "current" | "latest")
              }
            >
              <option value="local">{tr("payload_source_local")}</option>
              <option value="current">{tr("payload_source_current")}</option>
              <option value="latest">{tr("payload_source_latest")}</option>
            </select>
            {payloadAutoReload && (
              <p className="muted small">{tr("payload_auto_note")}</p>
            )}
          </label>
          {payloadReloadMode === "local" && (
            <label className="field">
              <span>{tr("payload_file")}</span>
              <div className="inline-field">
                <input
                  placeholder={tr("payload_file")}
                  value={payloadLocalPath}
                  onChange={(event) => setPayloadLocalPath(event.target.value)}
                />
                <button className="btn" onClick={handlePayloadBrowse}>
                  {tr("browse")}
                </button>
              </div>
              {payloadAutoReload && !payloadLocalPath.trim() && (
                <p className="muted small warn">{tr("payload_local_required")}</p>
              )}
              {payloadProbe && (
                <p className={`muted small ${payloadProbe.ok ? "ok" : "warn"}`}>
                  {payloadProbe.message}
                </p>
              )}
            </label>
          )}
          <div className="split">
            <button
              className="btn warning"
              onClick={handlePayloadSend}
              disabled={payloadBusy || payloadReloadCooldown}
            >
              {tr("payload_send")}
            </button>
            <button className="btn info" onClick={handlePayloadCheck} disabled={payloadBusy}>
              {tr("check")}
            </button>
          </div>
          <label className="field inline spaced">
            <span>{tr("auto_reload_payload")}</span>
            <input
              type="checkbox"
              checked={payloadAutoReload}
              onChange={(event) => setPayloadAutoReload(event.target.checked)}
            />
          </label>
          <div className="card-divider" />
          <div className={`pill status-pill ${payloadStatusClass}`}>
            {tr("status")}: {payloadStatus}
          </div>
          {payloadVersion && (
            <div className="pill status-pill">
              {tr("version")}: {payloadVersion}
            </div>
          )}
        </section>

        <section className="card">
          <header className="card-title">
            <span className="card-title-icon">■</span>
            {tr("storage_title")}
          </header>
          {storageLocations.length === 0 ? (
            <p className="muted">{tr("not_connected")}</p>
          ) : (
            <div className="stack">
              {storageLocations.map((loc) => (
                <label className="field inline" key={loc.path}>
                  <input
                    type="radio"
                    name="storageRoot"
                    checked={storageRoot === loc.path}
                    onChange={() => setStorageRoot(loc.path)}
                  />
                  <span>
                    {loc.path} ({tr("gb_free", { size: loc.free_gb.toFixed(1) })})
                  </span>
                </label>
              ))}
            </div>
          )}
        </section>

        <section className="card">
          <header className="card-title">
            <span className="card-title-icon">↻</span>
            {tr("updates")}
          </header>
          <div className="pill">
            {tr("current_version")}: v{appVersion}
          </div>
          <p className="muted">{updateStatus}</p>
          <label className="field inline">
            <span>{tr("include_prerelease")}</span>
            <input
              type="checkbox"
              checked={includePrerelease}
              onChange={(event) => setIncludePrerelease(event.target.checked)}
            />
          </label>
          <button className="btn" onClick={handleUpdateCheck}>
            {tr("check_updates")}
          </button>
          {updateInfo && (
            <div className="stack">
              <button
                className="btn ghost"
                onClick={() => openExternal(updateInfo.html_url)}
              >
                {tr("open_release_page")}
              </button>
              {updateInfo.assets.find((asset) => asset.name === "ps5upload.elf") && (
                <button
                  className="btn ghost"
                  onClick={() => {
                    const asset = updateInfo.assets.find(
                      (item) => item.name === "ps5upload.elf"
                    );
                    if (asset) {
                      handleUpdateDownload(asset);
                    }
                  }}
                >
                  ↓ {tr("download_payload_btn")}
                </button>
              )}
              {clientAsset && (
                <button
                  className="btn ghost"
                  onClick={() => handleUpdateDownload(clientAsset)}
                >
                  ↓ {tr("download_client")}
                </button>
              )}
              {updateAvailable && (
                <button className="btn primary" onClick={handleUpdatePrepareSelf}>
                  {tr("update_now")}
                </button>
              )}
              {updatePending && (
                <button className="btn primary" onClick={handleUpdateApply}>
                  ↻ {tr("restart_to_update")}
                </button>
              )}
            </div>
          )}
          {updateDownloadStatus && <p className="muted">{updateDownloadStatus}</p>}
        </section>

      </aside>

      <main className="panel main shell">
        <nav className="tabs">
          {tabs.map((tab) => (
            <button
              key={tab.id}
              className={`tab ${activeTab === tab.id ? "active" : ""}`}
              onClick={() => setActiveTab(tab.id)}
            >
              {tab.icon} {tab.label}
            </button>
          ))}
        </nav>

        <section className="content">
          {activeTab === "transfer" && (
            <div className="grid-two">
              <div className="card source-card">
                <header className="card-title">
                  <span className="card-title-icon">▢</span>
                  {tr("source")}
                </header>
                <label className="field">
                  <span>{tr("folder")}</span>
                  <div className="inline-field">
                    <input
                      placeholder="/Users/you/PKG"
                      value={sourcePath}
                      onChange={(event) => setSourcePath(event.target.value)}
                    />
                    <button className="btn" onClick={handleBrowse}>
                      {tr("browse_folder")}
                    </button>
                    <button className="btn" onClick={handleBrowseArchive}>
                      {tr("browse_archive")}
                    </button>
                  </div>
                </label>
                <div className="split">
                  <button
                    className={`btn ${optimizeMode === "optimize" ? "primary" : ""}`}
                    onClick={() => handleOptimizeMode("optimize")}
                    disabled={optimizeButtonDisabled}
                  >
                    {tr("optimize")}
                  </button>
                  <button
                    className={`btn ${optimizeMode === "deep" ? "primary" : ""}`}
                    onClick={() => handleOptimizeMode("deep")}
                    disabled={deepOptimizeButtonDisabled}
                  >
                    {tr("deep_optimize")}
                  </button>
                </div>
                {scanStatus !== "idle" && (
                  <div className="stack">
                    {scanStatusLabel && (
                      <div className="pill status-pill">{scanStatusLabel}</div>
                    )}
                    {(scanStatus === "scanning" || scanStatus === "completed") && (
                      <div className="progress">
                        <div
                          className={`progress-fill ${scanStatus === "scanning" ? "streaming" : ""}`}
                          style={{ width: "100%" }}
                        />
                      </div>
                    )}
                    {scanSummary && <p className="muted small">{scanSummary}</p>}
                    {scanPartialNote && (
                      <p className="muted small">{scanPartialNote}</p>
                    )}
                    {scanEstimateNote && (
                      <p className="muted small">{scanEstimateNote}</p>
                    )}
                    {scanStatus === "error" && scanError && (
                      <p className="muted small">{scanError}</p>
                    )}
                  </div>
                )}
                <p className="muted small">{tr("optimize_help")}</p>
                <p className="muted small">{tr("deep_optimize_help")}</p>
                {isArchiveSource && (
                  <p className="muted small">{tr("scan_archive_not_supported")}</p>
                )}
                {optimizeActive && (
                  <p className="muted small">{tr("optimize_lock_note")}</p>
                )}
                {gameMeta && (
                  <div className="source-meta-fill">
                    <div className="meta-block">
                      {gameCoverUrl && (
                        <img src={gameCoverUrl} alt={gameMeta.title} />
                      )}
                      <div>
                        <strong>{gameMeta.title}</strong>
                        {gameMeta.content_id && (
                          <div className="muted">
                            Content ID: {gameMeta.content_id}
                          </div>
                        )}
                        {gameMeta.title_id && (
                          <div className="muted">Title ID: {gameMeta.title_id}</div>
                        )}
                        {gameMeta.version && (
                          <div className="muted">Version: {gameMeta.version}</div>
                        )}
                      </div>
                    </div>
                  </div>
                )}
              </div>

              <div className="card">
                <header className="card-title">
                  <span className="card-title-icon">◉</span>
                  {tr("destination")}
                </header>
                <label className="field">
                  <span>{tr("storage_label")}</span>
                  <select
                    value={storageRoot}
                    onChange={(event) => setStorageRoot(event.target.value)}
                  >
                    {storageLocations.length > 0 ? (
                      storageLocations.map((loc) => (
                        <option key={loc.path} value={loc.path}>
                          {loc.path} ({tr("gb_free", { size: loc.free_gb.toFixed(1) })})
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
                  <span>{tr("preset")}</span>
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
                    <span>{tr("custom_path")}</span>
                    <input
                      value={customPreset}
                      onChange={(event) => setCustomPreset(event.target.value)}
                      placeholder="homebrew/custom"
                    />
                  </label>
                )}
                <label className="field">
                  <span>{tr("subfolder")}</span>
                  <input
                    placeholder="auto"
                    value={subfolder}
                    onChange={(event) => setSubfolder(event.target.value)}
                  />
                </label>
{isRar && (
  <>
    <div className="form-row">
      <label htmlFor="rar-mode">{tr("rar_extract")}</label>
      <select
        id="rar-mode"
        value={rarExtractMode}
        onChange={(e) =>
          setRarExtractMode(e.target.value as RarExtractMode)
        }
      >
        <option value="normal">{tr("rar_normal")}</option>
        <option value="safe">{tr("rar_safe")}</option>
        <option value="turbo">{tr("rar_turbo")}</option>
      </select>
    </div>
    <div className="form-row">
      <label htmlFor="rar-temp-storage">{tr("rar_temp_storage")}</label>
      <select
        id="rar-temp-storage"
        value={rarTemp}
        onChange={(e) => setRarTemp(e.target.value)}
      >
        <option value="">{tr("rar_temp_default", { path: storageRoot })}</option>
        {storageLocations.map((loc) => (
          <option key={`rar-temp-${loc.path}`} value={loc.path}>
            {loc.path} ({tr("gb_free", { size: loc.free_gb.toFixed(1) })})
          </option>
        ))}
      </select>
    </div>
  </>
)}                <label className="field inline">
                  <span>{tr("chmod_after")}</span>
                  <input
                    type="checkbox"
                    checked={chmodAfterUpload}
                    onChange={(event) => setChmodAfterUpload(event.target.checked)}
                  />
                  <span className="muted small" style={{ marginLeft: 8 }}>
                    {tr("chmod_note")} {tr("chmod_recursive_note")}
                  </span>
                </label>
                <label className="field">
                  <span>{tr("final_path")}</span>
                  <div className="inline-field">
                    <input
                      value={finalDestPath}
                      onChange={(event) => {
                        setFinalPathMode("custom");
                        setFinalPath(event.target.value);
                      }}
                      readOnly={finalPathMode === "auto"}
                    />
                    {finalPathMode === "auto" ? (
                      <button
                        className="btn ghost"
                        onClick={() => {
                          setFinalPathMode("custom");
                          setFinalPath(finalDestPath);
                        }}
                      >
                        {tr("edit")}
                      </button>
                    ) : (
                      <button
                        className="btn ghost"
                        onClick={() => {
                          setFinalPathMode("auto");
                          setFinalPath(defaultDestPath);
                        }}
                      >
                        {tr("use_auto")}
                      </button>
                    )}
                  </div>
                  <p className="muted small">{tr("final_path_note")}</p>
                </label>
                <label className="field inline">
                  <span className="override-label">{tr("override_conflict")}</span>
                  <input
                    type="checkbox"
                    checked={overrideOnConflict}
                    disabled={resumeMode !== "none"}
                    onChange={(event) => setOverrideOnConflict(event.target.checked)}
                  />
                </label>
                <p className="muted small">{tr("override_conflict_note")}</p>
                {resumeMode !== "none" ? (
                  <p className="muted small warn">{tr("override_conflict_resume_note")}</p>
                ) : (
                  !overrideOnConflict && (
                    <p className="muted small warn">{tr("override_conflict_warn")}</p>
                  )
                )}
              </div>

              <div className="card wide">
                <header className="card-title">
                  <span className="card-title-icon">⚙</span>
                  {tr("transfer_settings")}
                </header>
                <div className="grid-two">
                  <div className="stack">
                    <div className="section-label">{tr("connection_settings")}</div>
                    <label className="field">
                      <span>{tr("bandwidth_limit")}</span>
                      <input
                        type="number"
                        min={0}
                        value={bandwidthLimit}
                        disabled={optimizeActive}
                        onChange={(event) =>
                          setBandwidthLimit(Number(event.target.value))
                        }
                      />
                    </label>
                    <label className="field inline">
                      <span>{tr("auto_tune")}</span>
                      <input
                        type="checkbox"
                        checked={autoTune}
                        disabled={optimizeActive}
                        onChange={(event) => setAutoTune(event.target.checked)}
                      />
                    </label>
                    <p className="muted small">{tr("auto_tune_desc")}</p>
                    <p className="muted small">{tr("auto_tune_note")}</p>
                    <label
                      className={`field ${connectionsDisabled ? "is-disabled" : ""}`}
                    >
                      <span>{tr("connections")}</span>
                      <input
                        type="number"
                        min={1}
                        max={10}
                        value={connections}
                        disabled={connectionsDisabled}
                        onChange={(event) =>
                          setConnections(
                            Math.min(10, Math.max(1, Number(event.target.value)))
                          )
                        }
                      />
                    </label>
                    {autoTune && (
                      <p className="muted small note-tight">
                        {tr("connections_auto_note")}
                      </p>
                    )}
                    <p className="muted small">{tr("connections_note")}</p>
                    <p className="muted small">{tr("connections_note_extra")}</p>
                  </div>
                  <div className="stack">
                    <div className="section-label">{tr("transfer_options")}</div>
                    <label className="field">
                      <span>{tr("compression")}</span>
                      <select
                        value={compression}
                        disabled={optimizeActive}
                        onChange={(event) =>
                          setCompression(event.target.value as CompressionOption)
                        }
                      >
                        <option value="auto">{tr("compression_auto")}</option>
                        <option value="none">{tr("compression_none")}</option>
                        <option value="lz4">{tr("compression_lz4")}</option>
                        <option value="zstd">{tr("compression_zstd")}</option>
                        <option value="lzma">{tr("compression_lzma")}</option>
                      </select>
                    </label>
                    <label className="field">
                      <span>{tr("resume_mode")}</span>
                      <select
                        value={resumeMode}
                        onChange={(event) =>
                          setResumeMode(event.target.value as ResumeOption)
                        }
                      >
                        <option value="none">{tr("resume_off")}</option>
                        <option value="size">{tr("resume_fastest")}</option>
                        <option value="hash_large">{tr("resume_faster")}</option>
                        <option value="hash_medium">{tr("resume_fast_hash")}</option>
                        <option value="sha256">{tr("resume_normal")}</option>
                      </select>
                    </label>
                    {resumeMode !== "none" && (
                      <>
                        <p className="muted small">{tr("resume_note")}</p>
                        <p className="muted small">{tr("resume_override_note")}</p>
                        <p className="muted small">{tr("resume_note_change")}</p>
                      </>
                    )}
                    <div className="card-divider" />
                    <label className="field inline">
                      <span>{tr("use_temp")}</span>
                      <input
                        type="checkbox"
                        checked={useTemp}
                        onChange={(event) => setUseTemp(event.target.checked)}
                      />
                    </label>
                    <p className="muted small">{tr("use_temp_desc")}</p>
                  </div>
                </div>
              </div>

              <div className="card wide">
                <header className="card-title">
                  <span className="card-title-icon">▶</span>
                  {tr("actions")}
                </header>
                <div className="split">
                  <button
                    className="btn primary"
                    onClick={handleAddToQueue}
                    disabled={scanInProgress || !sourcePath.trim()}
                    title={tr("add_to_queue")}
                  >
                    +
                  </button>
                  <button
                    className="btn warning transfer-reset"
                    onClick={handleResetTransfer}
                  >
                    {tr("reset_settings")}
                  </button>
                </div>
                <p className="muted small" style={{ marginTop: 8 }}>
                  {transferActive || uploadQueueRunning
                    ? tr("upload_queue_only")
                    : sourcePath.trim()
                    ? tr("ready_to_upload")
                    : tr("select_source")}
                </p>
              </div>

            </div>
          )}

          {activeTab === "payload" && (
            <div className="grid-two">
              <div className="card">
                <header className="card-title">
                  <span className="card-title-icon">◎</span>
                  {tr("payload_status")}
                </header>
                {!isConnected ? (
                  <p className="muted">{tr("not_connected")}</p>
                ) : payloadFullStatus ? (
                  <div className="payload-status">
                    <div className="payload-status-top">
                      <div className="payload-status-pills">
                        <span className="pill">
                          {tr("version")}: {payloadFullStatus.version}
                        </span>
                        <span className="pill">
                          {tr("uptime")}: {formatUptime(payloadFullStatus.uptime)}
                        </span>
                        <span className={`pill ${payloadFullStatus.is_busy ? "warn" : "ok"}`}>
                          {payloadFullStatus.is_busy ? tr("busy") : tr("idle")}
                        </span>
                      </div>
                      <div className={`payload-status-state pill ${payloadStatusClass}`}>
                        {tr("status")}: {payloadStatus}
                      </div>
                    </div>
                    <div className="payload-status-grid">
                      {payloadFullStatus.system && (
                        <div className="payload-metric-card">
                          <div className="payload-metric-title">{tr("system_metrics")}</div>
                          <div className="payload-metric-row">
                            <span>{tr("cpu_usage")}</span>
                            <span>
                              {payloadFullStatus.system.cpu_percent >= 0
                                ? `${payloadFullStatus.system.cpu_percent.toFixed(1)}%`
                                : payloadFullStatus.system.cpu_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("payload_cpu_usage")}</span>
                            <span>
                              {payloadFullStatus.system.proc_cpu_percent != null &&
                              payloadFullStatus.system.proc_cpu_percent >= 0
                                ? `${payloadFullStatus.system.proc_cpu_percent.toFixed(1)}%`
                                : payloadFullStatus.system.proc_cpu_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("memory_usage")}</span>
                            <span>
                              {payloadFullStatus.system.rss_bytes >= 0
                                ? formatBytes(payloadFullStatus.system.rss_bytes)
                                : payloadFullStatus.system.rss_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("memory_total")}</span>
                            <span>
                              {payloadFullStatus.system.mem_total_bytes >= 0
                                ? formatBytes(payloadFullStatus.system.mem_total_bytes)
                                : payloadFullStatus.system.mem_total_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("memory_free")}</span>
                            <span>
                              {payloadFullStatus.system.mem_free_bytes >= 0
                                ? formatBytes(payloadFullStatus.system.mem_free_bytes)
                                : payloadFullStatus.system.mem_free_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("thread_count")}</span>
                            <span>
                              {payloadFullStatus.system.thread_count >= 0
                                ? payloadFullStatus.system.thread_count
                                : payloadFullStatus.system.thread_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("net_rx")}</span>
                            <span>
                              {payloadFullStatus.system.net_rx_bytes != null &&
                              payloadFullStatus.system.net_rx_bytes >= 0
                                ? formatBytes(payloadFullStatus.system.net_rx_bytes)
                                : payloadFullStatus.system.net_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("net_tx")}</span>
                            <span>
                              {payloadFullStatus.system.net_tx_bytes != null &&
                              payloadFullStatus.system.net_tx_bytes >= 0
                                ? formatBytes(payloadFullStatus.system.net_tx_bytes)
                                : payloadFullStatus.system.net_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("net_rx_rate")}</span>
                            <span>
                              {payloadFullStatus.system.net_rx_bps != null &&
                              payloadFullStatus.system.net_rx_bps >= 0
                                ? `${formatBytes(payloadFullStatus.system.net_rx_bps)}/s`
                                : payloadFullStatus.system.net_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("net_tx_rate")}</span>
                            <span>
                              {payloadFullStatus.system.net_tx_bps != null &&
                              payloadFullStatus.system.net_tx_bps >= 0
                                ? `${formatBytes(payloadFullStatus.system.net_tx_bps)}/s`
                                : payloadFullStatus.system.net_supported === false
                                ? tr("restricted")
                                : "—"}
                            </span>
                          </div>
                        </div>
                      )}
                      {payloadFullStatus.transfer && (
                        <div className="payload-metric-card">
                          <div className="payload-metric-title">{tr("transfer_metrics")}</div>
                          <div className="payload-metric-row">
                            <span>{tr("active_sessions")}</span>
                            <span>{payloadFullStatus.transfer.active_sessions}</span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("writer_queue")}</span>
                            <span>{payloadFullStatus.transfer.queue_count}</span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("pack_in_use")}</span>
                            <span>{payloadFullStatus.transfer.pack_in_use}</span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("pool_count")}</span>
                            <span>{payloadFullStatus.transfer.pool_count}</span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("backpressure")}</span>
                            <span>
                              {payloadFullStatus.transfer.backpressure_events} /{" "}
                              {payloadFullStatus.transfer.backpressure_wait_ms} ms
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("last_progress")}</span>
                            <span>
                              {payloadFullStatus.transfer.last_progress
                                ? formatTimestamp(payloadFullStatus.transfer.last_progress)
                                : "—"}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("abort_requested")}</span>
                            <span>
                              {payloadFullStatus.transfer.abort_requested ? tr("on") : tr("off")}
                            </span>
                          </div>
                          <div className="payload-metric-row">
                            <span>{tr("workers_initialized")}</span>
                            <span>
                              {payloadFullStatus.transfer.workers_initialized ? tr("on") : tr("off")}
                            </span>
                          </div>
                          {payloadFullStatus.extract_last_progress ? (
                            <div className="payload-metric-row">
                              <span>{tr("extract_last_progress")}</span>
                              <span>{formatTimestamp(payloadFullStatus.extract_last_progress)}</span>
                            </div>
                          ) : null}
                        </div>
                      )}
                    </div>
                    <div className="payload-status-actions">
                      <button
                        className="btn"
                        onClick={handleRefreshPayloadStatus}
                        disabled={payloadStatusLoading}
                      >
                        {payloadStatusLoading ? tr("loading") : tr("refresh")}
                      </button>
                      <button
                        className="btn danger"
                        onClick={handlePayloadReset}
                        disabled={payloadResetting}
                      >
                        {payloadResetting ? tr("loading") : tr("reset")}
                      </button>
                    </div>
                    <p className="muted small payload-status-updated">
                      Last update: {formatUpdatedAt(payloadLastUpdated)}
                    </p>
                  </div>
                ) : (
                  <div className="stack">
                    <p className="muted">
                      {payloadStatusError
                        ? `Status error: ${payloadStatusError}`
                        : `${tr("loading")}...`}
                    </p>
                    <div className="split">
                      <button
                        className="btn"
                        onClick={handleRefreshPayloadStatus}
                        disabled={payloadStatusLoading}
                      >
                        {tr("refresh")}
                      </button>
                      <button
                        className="btn danger"
                        onClick={handlePayloadReset}
                        disabled={payloadResetting}
                      >
                        {payloadResetting ? tr("loading") : tr("reset")}
                      </button>
                    </div>
                    <p className="muted small">
                      Last update: {formatUpdatedAt(payloadLastUpdated)}
                    </p>
                  </div>
                )}
              </div>

              <div className="card wide">
                <header className="card-title">
                  <span className="card-title-icon">▣</span>
                  {tr("upload_queue")}
                </header>
                <div className="queue-tabs">
                  <button
                    className={`queue-tab ${uploadQueueTab === "current" ? "active" : ""}`}
                    onClick={() => setUploadQueueTab("current")}
                  >
                    {tr("current")}
                    {uploadCurrentCount > 0 && (
                      <span className="badge">{uploadCurrentCount}</span>
                    )}
                  </button>
                  <button
                    className={`queue-tab ${uploadQueueTab === "completed" ? "active" : ""}`}
                    onClick={() => setUploadQueueTab("completed")}
                  >
                    {tr("completed")}
                    {uploadCompletedCount > 0 && (
                      <span className="badge">{uploadCompletedCount}</span>
                    )}
                  </button>
                  <button
                    className={`queue-tab ${uploadQueueTab === "failed" ? "active" : ""}`}
                    onClick={() => setUploadQueueTab("failed")}
                  >
                    {tr("stopped")}
                    {uploadFailedCount > 0 && (
                      <span className="badge">{uploadFailedCount}</span>
                    )}
                  </button>
                </div>
                <div className="split" style={{ marginBottom: 8 }}>
                  <button
                    className="btn success"
                    onClick={handleUploadQueue}
                    disabled={
                      uploadQueueRunning ||
                      transferActive ||
                      !hasUploadStartable
                    }
                  >
                    {tr("start")}
                  </button>
                  <button
                    className="btn danger"
                    onClick={handleCancel}
                    disabled={!uploadQueueRunning && !transferActive}
                  >
                    {tr("stop")}
                  </button>
                  <button className="btn" onClick={handleRefreshUploadQueue}>
                    {tr("refresh")}
                  </button>
                  <button
                    className="btn"
                    onClick={handleClearCompletedQueue}
                    disabled={!hasUploadCompleted}
                  >
                    {tr("clear_completed")}
                  </button>
                  <button
                    className="btn"
                    onClick={handleClearFailedQueue}
                    disabled={!hasUploadFailed}
                  >
                    {tr("clear_stopped")}
                  </button>
                  <button
                    className="btn"
                    onClick={handleClearQueue}
                    disabled={queueData.items.length === 0}
                  >
                    {tr("clear_queue")}
                  </button>
                </div>
                {queueData.items.length === 0 ? (
                  <div className="stack">
                    <p className="muted">{tr("queue_empty")}</p>
                  </div>
                ) : uploadQueueTab === "current" && !hasUploadCurrent ? (
                  <div className="stack">
                    <p className="muted">{tr("queue_empty")}</p>
                  </div>
                ) : uploadQueueTab === "completed" && !hasUploadCompleted ? (
                  <div className="stack">
                    <p className="muted">No completed uploads yet.</p>
                  </div>
                ) : uploadQueueTab === "failed" && !hasUploadFailed ? (
                  <div className="stack">
                    <p className="muted">{tr("no_stopped_uploads")}</p>
                  </div>
                ) : (
                  <div className="stack">
                    {queueData.items.map((item, index) => {
                      const showItem =
                        uploadQueueTab === "current"
                          ? isUploadCurrent(item)
                          : uploadQueueTab === "completed"
                          ? isUploadCompleted(item)
                          : isUploadFailed(item);
                      if (!showItem) return null;
                      const statusClass = item.status === "InProgress"
                        ? "active"
                        : item.status === "Completed"
                        ? "completed"
                        : item.status === "Failed"
                        ? "failed"
                        : "";
                      const isActive = item.id === currentQueueItemId;
                      const isStalled = item.status === "InProgress" && !transferActive;
                      const uploadFailedMessage = isUploadFailed(item)
                        ? (item.status as { Failed: string }).Failed
                        : "";
                      const isUserStopped =
                        uploadFailedMessage === USER_STOPPED_SENTINEL ||
                        uploadFailedMessage === tr("stopped");
                      const destPathForItem = buildDestPathForItem(
                        item.storage_base || storageRoot,
                        item
                      );
                      const historyKey = `${item.source_path}||${destPathForItem}`;
                      const historyDuration =
                        (isUploadCompleted(item)
                          ? historyDurationByKey.get(historyKey)?.success
                          : isUploadFailed(item)
                          ? historyDurationByKey.get(historyKey)?.failed
                          : null) ?? null;
                      const runActionLabel =
                        item.last_run_action === "resume"
                          ? tr("resumed")
                          : item.last_run_action === "requeue"
                          ? tr("requeued")
                          : null;
                      const resumeSupported = !isArchivePath(item.source_path);
                      const failedAt =
                        item.last_failed_at ? formatTimestamp(item.last_failed_at) : null;
                      const failedDetailParts: string[] = [];
                      if (item.last_failed_bytes != null) {
                        if (item.last_failed_total_bytes && item.last_failed_total_bytes > 0) {
                          failedDetailParts.push(
                            `${formatBytes(item.last_failed_bytes)} / ${formatBytes(
                              item.last_failed_total_bytes
                            )}`
                          );
                        } else {
                          failedDetailParts.push(
                            `${formatBytes(item.last_failed_bytes)} transferred`
                          );
                        }
                      }
                      if (item.last_failed_files != null) {
                        failedDetailParts.push(`${item.last_failed_files} ${tr("files")}`);
                      }
                      if (item.last_failed_elapsed_sec != null) {
                        failedDetailParts.push(
                          `Elapsed ${formatDuration(item.last_failed_elapsed_sec)}`
                        );
                      }
                      const queueSettings = item.transfer_settings || {};
                      const isSkipping =
                        transferState.status.toLowerCase().includes("resume scan") ||
                        transferState.status.toLowerCase().includes("skipping");
                      const pendingPos = uploadPendingIndices.indexOf(index);
                      const canMoveUp = item.status === "Pending" && pendingPos > 0;
                      const canMoveDown =
                        item.status === "Pending" && pendingPos >= 0 && pendingPos < uploadPendingIndices.length - 1;
                      const prevPendingIndex = canMoveUp ? uploadPendingIndices[pendingPos - 1] : -1;
                      const nextPendingIndex = canMoveDown ? uploadPendingIndices[pendingPos + 1] : -1;
                      return (
                        <div className={`queue-item ${statusClass}`} key={item.id}>
                          <div className="queue-item-body">
                            <strong>
                              <span className="queue-type-icon">
                                {isArchivePath(item.source_path) ? <ArchiveIcon /> : <FolderIcon />}
                              </span>
                              {item.subfolder_name}
                              {item.size_bytes != null && (
                                <span className="muted small" style={{ marginLeft: 8 }}>
                                  {formatBytes(item.size_bytes)}
                                </span>
                              )}
                            </strong>
                            {item.paused && item.status === "Pending" && (
                              <div className="muted small">{tr("paused")}</div>
                            )}
                            {isStalled && (
                              <div className="muted small">{tr("stalled")}</div>
                            )}
                            <div className="muted small">
                              {destPathForItem}
                            </div>
                            {runActionLabel && (isUploadCompleted(item) || isUploadFailed(item) || isActive) && (
                              <div className="muted small">{runActionLabel}</div>
                            )}
                            {isUploadCompleted(item) && (
                              <div className="muted small">
                                Duration{" "}
                                {historyDuration != null
                                  ? formatDuration(Math.max(1, Math.round(historyDuration)))
                                  : "—"}
                              </div>
                            )}
                            {isUploadFailed(item) && (
                              <div className="muted small">
                                {isUserStopped ? tr("stopped") : tr("failed")}:{" "}
                                {isUserStopped
                                  ? tr("stopped_by_user")
                                  : uploadFailedMessage || "Unknown error"}
                                {failedAt ? ` · ${tr("failed_at", { time: failedAt })}` : ""}
                                {failedDetailParts.length > 0
                                  ? ` · ${failedDetailParts.join(" · ")}`
                                  : item.size_bytes != null
                                  ? ` · ${formatBytes(item.size_bytes)}`
                                  : ""}
                              </div>
                            )}
                            {isActive && (
                              <div className="progress-info" style={{ marginTop: 8 }}>
                                <div className="progress-bar">
                                  <div
                                    className="progress-fill"
                                    style={{ width: `${transferPercent}%` }}
                                  />
                                </div>
                                <div className="muted small">
                                  {isSkipping ? (
                                    <>
                                      {tr("skipping")} ·{" "}
                                      {transferState.total > 0
                                        ? `${transferState.files} / ${transferState.total} ${tr("files")}`
                                        : `${transferState.files} ${tr("files")}`}{" "}
                                      · {transferState.status}
                                    </>
                                  ) : (
                                    <>
                                      {transferPercentLabel} ·{" "}
                                      {transferTotal > 0
                                        ? `${formatBytes(transferState.sent)} / ${formatBytes(transferState.total)}`
                                        : `${formatBytes(transferState.sent)} transferred`}{" "}
                                      · {transferState.files} {tr("files")} ·{" "}
                                      {transferSpeedDisplay > 0
                                        ? `Avg ${formatBytes(transferSpeedDisplay)}/s`
                                        : "Avg —"}{" "}
                                      ·{" "}
                                      {transferElapsedDisplay > 0
                                        ? `Elapsed ${formatDuration(transferElapsedDisplay)}`
                                        : "Elapsed —"}{" "}
                                      ·{" "}
                                      {transferEtaSeconds != null
                                        ? `ETA ${formatDuration(transferEtaSeconds)}`
                                        : "ETA —"}{" "}
                                      ·{" "}
                                      {transferUpdatedAt
                                        ? `Updated ${formatUpdatedAt(transferUpdatedAt)}`
                                        : "Updated —"}{" "}
                                      · {transferState.status}
                                    </>
                                  )}
                                </div>
                                {showTransferUpdateNote && (
                                  <div className="muted small">
                                    Progress updates are periodic during archive upload/extraction.
                                    Totals may be unavailable.
                                  </div>
                                )}
                                {transferState.currentFile && (
                                  <div className="muted small">
                                    {transferState.currentFile}
                                  </div>
                                )}
                                {activeTransferSource && (
                                  <div className="muted small">Source: {activeTransferSource}</div>
                                )}
                                {activeTransferDest && (
                                  <div className="muted small">Destination: {activeTransferDest}</div>
                                )}
                              </div>
                            )}
                          </div>
                          <div className="queue-controls">
                            <button
                              className="btn ghost small"
                              onClick={() => setUploadInfoItem(item)}
                            >
                              info
                            </button>
                            {item.status === "Pending" && (
                              <>
                                <button
                                  className="btn ghost small queue-move"
                                  disabled={!canMoveUp}
                                  onClick={() => moveUploadQueueItem(index, prevPendingIndex)}
                                >
                                  {tr("move_up")}
                                </button>
                                <button
                                  className="btn ghost small queue-move"
                                  disabled={!canMoveDown}
                                  onClick={() => moveUploadQueueItem(index, nextPendingIndex)}
                                >
                                  {tr("move_down")}
                                </button>
                              </>
                            )}
                            {item.status === "Pending" && (
                              <button
                                className="btn ghost small"
                                onClick={() => handleRemoveQueueItem(item.id)}
                              >
                                {tr("cancel")}
                              </button>
                            )}
                            {isStalled && (
                              <button
                                className="btn ghost small"
                                onClick={() => handleRemoveQueueItem(item.id)}
                              >
                                {tr("cancel")}
                              </button>
                            )}
                            {isUploadFailed(item) && (
                              <>
                                <button
                                  className="btn ghost small"
                                  onClick={() => handleResumeFromFailedItem(item)}
                                  disabled={!resumeSupported}
                                  title={!resumeSupported ? tr("resume_unsupported_archive") : undefined}
                                >
                                  {tr("resume")}
                                </button>
                                <button
                                  className="btn ghost small"
                                  onClick={() => handleRequeueUploadItem(item.id)}
                                >
                                  {tr("requeue")}
                                </button>
                              </>
                            )}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>

              <div className="card wide">
                <header className="card-title">
                  <span className="card-title-icon">≣</span>
                  {tr("extraction_queue")}
                </header>
                <div className="queue-tabs">
                  <button
                    className={`queue-tab ${extractQueueTab === "current" ? "active" : ""}`}
                    onClick={() => setExtractQueueTab("current")}
                  >
                    {tr("current")}
                    {extractionCurrentCount > 0 && (
                      <span className="badge">{extractionCurrentCount}</span>
                    )}
                  </button>
                  <button
                    className={`queue-tab ${extractQueueTab === "completed" ? "active" : ""}`}
                    onClick={() => setExtractQueueTab("completed")}
                  >
                    {tr("completed")}
                    {extractionCompletedCount > 0 && (
                      <span className="badge">{extractionCompletedCount}</span>
                    )}
                  </button>
                  <button
                    className={`queue-tab ${extractQueueTab === "failed" ? "active" : ""}`}
                    onClick={() => setExtractQueueTab("failed")}
                  >
                    Stopped
                    {extractionFailedCount > 0 && (
                      <span className="badge">{extractionFailedCount}</span>
                    )}
                  </button>
                </div>
                {payloadFullStatus && (
                  <div className="split" style={{ marginBottom: 8 }}>
                    {(() => {
                      return (
                        <>
                          <button
                            className="btn success"
                            onClick={handleQueueProcess}
                            disabled={
                              !isConnected ||
                              !hasExtractionPending ||
                              payloadFullStatus.is_busy ||
                              extractionStopping
                            }
                          >
                            {tr("start")}
                          </button>
                          <button
                            className="btn danger"
                            onClick={handleQueueStopAll}
                            disabled={!isConnected || !hasExtractionRunning || extractionStopping}
                          >
                            {tr("stop")}
                          </button>
                          <button
                            className="btn"
                            onClick={handleRefreshQueueStatus}
                            disabled={!isConnected || payloadQueueLoading}
                          >
                            {payloadQueueLoading ? tr("loading") : tr("refresh")}
                          </button>
                          <button
                            className="btn"
                            onClick={handleQueueClear}
                            disabled={!isConnected || !hasExtractionCompleted}
                          >
                            {tr("clear_completed")}
                          </button>
                          <button
                            className="btn"
                            onClick={handleQueueClearFailed}
                            disabled={!isConnected || !hasExtractionFailed}
                          >
                            {tr("clear_failed")}
                          </button>
                          <button
                            className="btn"
                            onClick={handleQueueClearAll}
                            disabled={!isConnected || !hasExtractionItems}
                          >
                            {tr("clear_queue")}
                          </button>
                          <button
                            className="btn"
                            onClick={handleClearTmp}
                            disabled={!isConnected || payloadQueueLoading}
                          >
                            {tr("clear_tmp")}
                          </button>
                        </>
                      );
                    })()}
                  </div>
                )}
                {!payloadFullStatus ? (
                  <div className="stack">
                    <p className="muted">
                      {!isConnected
                        ? tr("not_connected")
                        : payloadStatusError
                        ? `Status error: ${payloadStatusError}`
                        : `${tr("loading")}...`}
                    </p>
                    {isConnected && (
                      <button
                        className="btn"
                        onClick={handleRefreshQueueStatus}
                        disabled={payloadQueueLoading}
                      >
                        {payloadQueueLoading ? tr("loading") : tr("refresh")}
                      </button>
                    )}
                  </div>
                ) : payloadFullStatus.items.length === 0 ? (
                  <div className="stack">
                    <p className="muted">{tr("extraction_queue_empty")}</p>
                  </div>
                ) : extractQueueTab === "current" && !hasExtractionCurrent ? (
                  <div className="stack">
                    <p className="muted">{tr("extraction_queue_empty")}</p>
                  </div>
                ) : extractQueueTab === "completed" && !hasExtractionCompleted ? (
                  <div className="stack">
                    <p className="muted">No completed extractions yet.</p>
                  </div>
                ) : extractQueueTab === "failed" && !hasExtractionFailed ? (
                  <div className="stack">
                    <p className="muted">No failed extractions.</p>
                  </div>
                ) : (
                  <div className="stack">
                    {!isConnected && (
                      <p className="muted small">Showing cached queue (disconnected).</p>
                    )}
                    {payloadFullStatus.items.map((item, itemIndex) => {
                      const showItem =
                        extractQueueTab === "current"
                          ? !isExtractionCompleted(item) && !isExtractionFailed(item)
                          : extractQueueTab === "completed"
                          ? isExtractionCompleted(item)
                          : isExtractionFailed(item);
                      if (!showItem) return null;
                      const pendingPos = extractionPendingIndices.indexOf(itemIndex);
                      const canMoveUp = item.status === "pending" && pendingPos > 0;
                      const canMoveDown =
                        item.status === "pending" && pendingPos >= 0 && pendingPos < extractionPendingIndices.length - 1;
                      const prevPendingIndex = canMoveUp ? extractionPendingIndices[pendingPos - 1] : -1;
                      const nextPendingIndex = canMoveDown ? extractionPendingIndices[pendingPos + 1] : -1;
                      const meta = queueMetaById[item.id];
                      const displayName = getQueueDisplayName(item);
                      const isCancelled =
                        item.status === "failed" &&
                        typeof item.error === "string" &&
                        item.error.toLowerCase().includes("cancel");
                      const statusLabel =
                        item.status === "running"
                          ? tr("extracting")
                          : item.status === "complete"
                          ? tr("complete")
                          : item.status === "failed"
                          ? isCancelled
                            ? "Cancelled"
                            : tr("failed")
                          : tr("pending");
                      const statusClass =
                        item.status === "running"
                          ? "warn"
                          : item.status === "complete"
                          ? "ok"
                          : item.status === "failed"
                          ? isCancelled
                            ? "warn"
                            : "error"
                          : "";
                      const extractionElapsedSec = item.started_at
                        ? Math.max(1, Math.floor(Date.now() / 1000) - item.started_at)
                        : 0;
                      const extractionSpeed =
                        extractionElapsedSec > 0
                          ? item.processed_bytes / extractionElapsedSec
                          : 0;
                      const extractionEta =
                        item.total_bytes > 0 && extractionSpeed > 0
                          ? Math.max(
                              1,
                              Math.ceil(
                                (item.total_bytes - item.processed_bytes) / extractionSpeed
                              )
                            )
                          : null;
                      const extractionPercent =
                        item.total_bytes > 0
                          ? Math.min(
                              100,
                              Math.floor((item.processed_bytes / item.total_bytes) * 100)
                            )
                          : null;
                      const extractionActionLabel =
                        extractionActionById[item.id] === "requeue"
                          ? tr("requeued")
                          : null;
                      return (
                        <div
                          key={item.id}
                          className={`queue-item ${item.status === "running" ? "active" : ""} ${item.status === "complete" ? "completed" : ""} ${item.status === "failed" ? "failed" : ""}`}
                        >
                          <div className="queue-item-body">
                          <div className="queue-item-header">
                            <div className="queue-title">
                              {meta?.cover_url && (
                                <img
                                  className="queue-cover"
                                  src={meta.cover_url}
                                  alt={displayName}
                                />
                              )}
                              <div className="queue-text">
                                <strong>{displayName}</strong>
                                {meta?.game_meta?.content_id && (
                                  <div className="muted small">
                                    Content ID: {meta.game_meta.content_id}
                                  </div>
                                )}
                                {meta?.game_meta?.title_id && (
                                  <div className="muted small">
                                    Title ID: {meta.game_meta.title_id}
                                  </div>
                                )}
                                {meta?.game_meta?.version && (
                                  <div className="muted small">
                                    Version: {meta.game_meta.version}
                                  </div>
                                )}
                              </div>
                            </div>
                            <span className={`chip ${statusClass}`}>
                              {statusLabel}
                            </span>
                          </div>
                          {(meta?.dest_path || item.dest_path) && (
                            <div className="muted small">{meta?.dest_path || item.dest_path}</div>
                          )}
                          {extractionActionLabel && (item.status === "failed" || item.status === "complete" || item.status === "running") && (
                            <div className="muted small">{extractionActionLabel}</div>
                          )}
                          <div className="muted small">
                            ID #{item.id} · {item.files_extracted} {tr("files")} · {item.total_bytes > 0 ? formatBytes(item.total_bytes) : `${formatBytes(item.processed_bytes)} extracted`}{meta?.size_bytes ? ` · Upload: ${formatBytes(meta.size_bytes)}` : ""}
                          </div>
                          {(item.started_at || (item.status !== "pending" && item.status !== "running")) && (
                            <div className="muted small">
                              Started: {formatTimestamp(item.started_at)}
                              {item.status === "complete" && item.completed_at ? (
                                <> · Completed: {formatTimestamp(item.completed_at)}</>
                              ) : null}
                            </div>
                          )}
                          {item.status === "pending" && !item.started_at && (
                            <div className="muted small">
                              {tr("waiting_for_payload_status")}
                            </div>
                          )}
                          {item.status === "complete" && (
                            <div className="muted small">
                              Duration{" "}
                              {item.started_at && item.completed_at
                                ? formatDuration(Math.max(1, item.completed_at - item.started_at))
                                : "—"}
                            </div>
                          )}
                          {item.status === "running" && (
                            <div className="progress-info">
                              <div className="progress-bar">
                                <div
                                  className={`progress-fill${item.total_bytes > 0 ? "" : " streaming"}`}
                                  style={{
                                    width: `${
                                      item.total_bytes > 0
                                        ? extractionPercent ?? 0
                                        : (item.processed_bytes > 0 ? 100 : 0)
                                    }%`
                                  }}
                                />
                              </div>
                              <div className="muted small">
                                {item.processed_bytes === 0
                                  ? "Starting extraction..."
                                  : item.total_bytes > 0
                                  ? `${extractionPercent}%`
                                  : "Streaming"}{" "}
                                ·{" "}
                                {item.total_bytes > 0
                                  ? `${formatBytes(item.processed_bytes)} / ${formatBytes(item.total_bytes)}`
                                  : `${formatBytes(item.processed_bytes)} extracted`}{" "}
                                ·{" "}
                                {extractionSpeed > 0
                                  ? `Avg ${formatBytes(extractionSpeed)}/s`
                                  : "Avg —"}{" "}
                                ·{" "}
                                {extractionElapsedSec > 0
                                  ? `Elapsed ${formatDuration(extractionElapsedSec)}`
                                  : "Elapsed —"}{" "}
                                ·{" "}
                                {extractionEta != null
                                  ? `ETA ${formatDuration(extractionEta)}`
                                  : "ETA —"}{" "}
                                ·{" "}
                                {payloadLastUpdated
                                  ? `Updated ${formatUpdatedAt(payloadLastUpdated)}`
                                  : "Updated —"}
                              </div>
                              <div className="muted small">
                                Progress updates are periodic during extraction. Totals may be unavailable.
                              </div>
                            </div>
                          )}
                          {item.status === "failed" && item.error && (
                            <div className="small" style={{ color: "#c86464" }}>
                              {isCancelled ? "Cancelled by user" : `${tr("error")}: ${item.error}`}
                            </div>
                          )}
                          {item.status === "failed" && (
                            <div className="muted small">
                              {item.completed_at
                                ? `${tr("failed_at", { time: formatTimestamp(item.completed_at) })} · `
                                : `${tr("failed")} · `}
                              {item.total_bytes > 0
                                ? `${formatBytes(item.processed_bytes)} / ${formatBytes(item.total_bytes)}`
                                : `${formatBytes(item.processed_bytes)} extracted`}{" "}
                              ·{" "}
                              {item.started_at && item.completed_at
                                ? `Elapsed ${formatDuration(
                                    Math.max(1, item.completed_at - item.started_at)
                                  )}`
                                : "Elapsed —"}
                            </div>
                          )}
                          {item.status === "pending" && (
                            <div className="split">
                              <div className="queue-controls">
                                <button
                                  className="btn ghost small queue-move"
                                  disabled={!canMoveUp}
                                  onClick={() => moveExtractionQueueItem(itemIndex, prevPendingIndex)}
                                >
                                  {tr("move_up")}
                                </button>
                                <button
                                  className="btn ghost small queue-move"
                                  disabled={!canMoveDown}
                                  onClick={() => moveExtractionQueueItem(itemIndex, nextPendingIndex)}
                                >
                                  {tr("move_down")}
                                </button>
                              </div>
                              <button
                                className="btn ghost small"
                                onClick={() => handleQueueCancel(item.id)}
                              >
                                {tr("cancel")}
                              </button>
                            </div>
                          )}
                          {item.status === "failed" && (
                            <div className="queue-controls">
                              <button
                                className="btn ghost small"
                                onClick={() => handleQueueRetry(item.id)}
                              >
                                {tr("requeue")}
                              </button>
                            </div>
                          )}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            </div>
          )}

          {activeTab === "manage" && (
            <div className="grid-two">
              <div className="card wide file-browser">
                <header className="card-title">
                  <span className="card-title-icon">☰</span>
                  {tr("remote_browser")}
                </header>
                <div className="browser-toolbar">
                  <input
                    value={managePath}
                    onChange={(event) => setManagePath(event.target.value)}
                    placeholder={tr("enter_path")}
                    onKeyDown={(e) => e.key === "Enter" && handleManageRefresh()}
                  />
                  <button className="btn" onClick={() => handleManageRefresh()}>
                    {tr("go")}
                  </button>
                  <button className="btn" onClick={handleManageUp}>
                    ↑ {tr("up")}
                  </button>
                  <button className="btn" onClick={() => handleManageRefresh()}>
                    ↻ {tr("refresh")}
                  </button>
                </div>
                <p className="muted small">
                  Last update: {formatUpdatedAt(manageLastUpdated)}
                </p>
                <div className="manage-list">
                  <table>
                    <thead>
                      <tr>
                        <th />
                        <th>{tr("name")}</th>
                        <th>{tr("type")}</th>
                        <th>{tr("size")}</th>
                        <th>{tr("modified")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {manageEntries.length === 0 ? (
                        <tr>
                          <td colSpan={5} className="muted">
                            {tr("no_files")}
                          </td>
                        </tr>
                      ) : (
                        manageEntries.map((entry, index) => {
                          const entryType = getEntryType(entry);
                          const isDir = entryType === "dir";
                          return (
                            <tr
                              key={`${entry.name}-${index}`}
                              className={manageSelected === index ? "selected" : ""}
                              onClick={() => handleManageSelectIndex(index)}
                              onDoubleClick={() => handleManageOpenDir(entry)}
                            >
                              <td className="manage-icon-cell">
                                {isDir ? <FolderIcon /> : <FileIcon />}
                              </td>
                              <td>{isDir ? `${entry.name}/` : entry.name}</td>
                              <td>{isDir ? tr("folder") : tr("file")}</td>
                              <td>
                                {isDir ? "—" : formatBytes(entry.size ?? 0)}
                              </td>
                              <td>{entry.mtime ? formatTimestamp(entry.mtime) : "—"}</td>
                            </tr>
                          );
                        })
                      )}
                    </tbody>
                  </table>
                </div>
              </div>

              <div className="card actions-panel">
                <header className="card-title">
                  <span className="card-title-icon">★</span>
                  {tr("actions")}
                </header>
                <div className="stack">
                  {manageSelectedEntry ? (
                    <div className="selected-item">
                      <span className="selected-item-icon">
                        {getEntryType(manageSelectedEntry) === "dir" ? <FolderIcon /> : <FileIcon />}
                      </span>
                      <div className="selected-item-info">
                        <div className="selected-item-name">{manageSelectedEntry.name}</div>
                        <div className="selected-item-type">
                          {getEntryType(manageSelectedEntry) === "dir"
                            ? tr("folder")
                            : formatBytes(manageSelectedEntry.size)}
                        </div>
                      </div>
                    </div>
                  ) : (
                    <p className="muted" style={{ textAlign: "center", padding: "8px 0" }}>
                      {tr("select_file")}
                    </p>
                  )}
                  <p className="muted small" style={{ textAlign: "center", marginBottom: "10px" }}>
                    {tr("manage_transfer_warning")}
                  </p>
                  <div className="split" style={{ marginBottom: "8px" }}>
                    <button className="btn warning" onClick={handleManageResetUI}>
                      Reset UI
                    </button>
                  </div>

                  <div className="action-group">
                    <div className="action-group-title">↓ {tr("transfer")}</div>
                    <div className="action-buttons">
                      <button
                        className="btn primary"
                        onClick={handleManageUpload}
                        disabled={manageBusy}
                      >
                        {tr("upload")} {tr("file")}
                      </button>
                      <button
                        className="btn"
                        onClick={handleManageUploadFolder}
                        disabled={manageBusy}
                      >
                        {tr("upload")} {tr("folder")}
                      </button>
                      <button
                        className="btn primary"
                        onClick={handleManageDownload}
                        disabled={!manageSelectedEntry || manageBusy}
                      >
                        {tr("download")}
                      </button>
                      <button
                        className="btn"
                        onClick={() => handleOpenDestPicker("Move")}
                        disabled={!manageSelectedEntry || manageBusy}
                      >
                        {tr("move")}
                      </button>
                      <button
                        className="btn"
                        onClick={() => handleOpenDestPicker("Copy")}
                        disabled={!manageSelectedEntry || manageBusy}
                      >
                        {tr("copy")}
                      </button>
                      <button
                        className="btn"
                        onClick={() => handleOpenDestPicker("Extract")}
                        disabled={
                          !manageSelectedEntry ||
                          manageBusy ||
                          getEntryType(manageSelectedEntry) !== "file" ||
                          !manageSelectedEntry.name.toLowerCase().endsWith(".rar")
                        }
                      >
                        {tr("extract")}
                      </button>
                    </div>
                    <p className="muted small">{tr("extract_note")}</p>
                    {transferActive && (
                      <div className="muted small" style={{ marginTop: "6px" }}>
                        {tr("manage_deprioritized")}
                      </div>
                    )}
                  </div>

                  <label className="field">
                    <span>{tr("download_compression")}</span>
                    <select
                      value={downloadCompression}
                      onChange={(event) =>
                        setDownloadCompression(
                          event.target.value as DownloadCompressionOption
                        )
                      }
                    >
                      <option value="auto">{tr("compression_auto")}</option>
                      <option value="none">{tr("compression_none")}</option>
                      <option value="lz4">{tr("compression_lz4")}</option>
                      <option value="zstd">{tr("compression_zstd")}</option>
                      <option value="lzma">{tr("compression_lzma")}</option>
                    </select>
                  </label>

                  <div className="action-group">
                    <div className="action-group-title">◆ {tr("manage")}</div>
                    <div className="action-buttons">
                      <button
                        className="btn"
                        onClick={handleManageRename}
                        disabled={!manageSelectedEntry || manageBusy}
                      >
                        {tr("rename")}
                      </button>
                      <button
                        className="btn danger"
                        onClick={handleManageDelete}
                        disabled={!manageSelectedEntry || manageBusy}
                      >
                        × {tr("delete")}
                      </button>
                      <button
                        className="btn"
                        onClick={handleManageChmod}
                        disabled={!manageSelectedEntry || manageBusy}
                      >
                        {tr("chmod")}
                      </button>
                      <button
                        className="btn"
                        onClick={handleManageCreateFolder}
                        disabled={manageBusy}
                      >
                        {tr("new_folder")}
                      </button>
                    </div>
                  </div>

                  {manageMeta && (
                    <div className="meta-block">
                      {manageCoverUrl && (
                        <img src={manageCoverUrl} alt={manageMeta.title} />
                      )}
                      <div>
                        <strong>{manageMeta.title}</strong>
                        {manageMeta.content_id && (
                          <div className="muted small">Content ID: {manageMeta.content_id}</div>
                        )}
                        {manageMeta.title_id && (
                          <div className="muted small">Title ID: {manageMeta.title_id}</div>
                        )}
                        {manageMeta.version && (
                          <div className="muted small">Version: {manageMeta.version}</div>
                        )}
                      </div>
                    </div>
                  )}
                </div>
              </div>
            </div>
          )}

          {activeTab === "chat" && (
            <div className="grid-two chat-grid">
              <div className="card wide faq-card">
                <header className="card-title">
                  <span className="card-title-icon">?</span>
                  {tr("faq")}
                  <div className="faq-actions">
                    <button
                      className="btn ghost small"
                      onClick={() =>
                        openExternal(
                          "https://github.com/phantomptr/ps5upload/blob/main/FAQ.md"
                        )
                      }
                    >
                      {tr("open_faq")}
                    </button>
                    <button className="btn info small" onClick={handleFaqReload}>
                      {tr("refresh")}
                    </button>
                  </div>
                </header>
                <div className="faq-window">
                  {faqLoading ? (
                    <p className="muted">{tr("loading")}...</p>
                  ) : faqError ? (
                    <div className="stack">
                      <p className="muted">FAQ failed to load: {faqError}</p>
                      <button className="btn" onClick={handleFaqReload}>
                        {tr("refresh")}
                      </button>
                    </div>
                  ) : (
                    <div className="faq-content">
                      {renderMarkdownBlocks(faqContent, openExternal)}
                    </div>
                  )}
                </div>
              </div>
            </div>
          )}
        </section>
      </main>

      <aside className="panel right shell">
        <section className="card">
          <header className="card-title">
            <span className="card-title-icon">≣</span>
            {tr("logs")}
          </header>
          <div className="split">
            <button
              className={`btn ${logTab === "client" ? "primary" : ""}`}
              onClick={() => setLogTab("client")}
            >
              {tr("client")}
            </button>
            <button
              className={`btn ${logTab === "payload" ? "primary" : ""}`}
              onClick={() => setLogTab("payload")}
            >
              {tr("payload")}
            </button>
            <button
              className={`btn ${logTab === "history" ? "primary" : ""}`}
              onClick={() => setLogTab("history")}
            >
              {tr("history")}
            </button>
          </div>

          {logTab === "history" ? (
            <div className="stack">
              <button
                className="btn"
                onClick={() => {
                  invoke("history_clear")
                    .then(() => {
                      setHistoryData({ records: [], rev: (historyData.rev || 0) + 1, updated_at: Date.now() });
                      pushClientLog("History cleared.");
                    })
                    .catch((err) => {
                      pushClientLog(`Failed to clear history: ${String(err)}`);
                    });
                }}
              >
                {tr("clear_history")}
              </button>
              {historyData.records.length === 0 ? (
                <p className="muted">{tr("no_history")}</p>
              ) : (
                historyData.records
                  .slice()
                  .reverse()
                  .map((record) => (
                    <div className="history-item" key={record.timestamp}>
                      <div className="history-main">
                        <div className="history-title">
                          {record.cover_url && (
                            <img
                              className="history-cover"
                              src={record.cover_url}
                              alt={record.game_meta?.title || "Cover"}
                            />
                          )}
                          <div className="history-text">
                            <strong style={{ color: record.success ? "#64c864" : "#c86464" }}>
                              {record.success ? "✓" : "✗"}{" "}
                              {record.game_meta?.title ||
                                getLeafName(record.dest_path) ||
                                getLeafName(record.source_path)}
                            </strong>
                            {record.game_meta?.content_id && (
                              <div className="muted small">
                                Content ID: {record.game_meta.content_id}
                              </div>
                            )}
                            {record.game_meta?.title_id && (
                              <div className="muted small">
                                Title ID: {record.game_meta.title_id}
                              </div>
                            )}
                            {record.game_meta?.version && (
                              <div className="muted small">
                                Version: {record.game_meta.version}
                              </div>
                            )}
                          </div>
                        </div>
                        <div className="muted">{record.dest_path}</div>
                        <div className="muted small">
                          {formatTimestamp(record.timestamp)} · {record.via_queue ? tr("queue") : tr("single")}
                        </div>
                        {record.error && (
                          <div className="small" style={{ color: "#c86464" }}>
                            {tr("error")}: {record.error}
                          </div>
                        )}
                      </div>
                      <div className="history-meta">
                        <span className="history-metric">
                          {record.file_count} {tr("files")}
                        </span>
                        <span className="history-metric history-metric-split">
                          <span>{formatBytes(record.total_bytes)}</span>
                          <span>{formatBytes(record.speed_bps)}/s</span>
                        </span>
                        <span className="history-metric">
                          {formatDuration(record.duration_secs)}
                        </span>
                        <button
                          className="btn ghost small"
                          onClick={() => handleResumeFromHistory(record)}
                        >
                          ↻ {tr("resume")}
                        </button>
                      </div>
                    </div>
                  ))
              )}
            </div>
          ) : (
            <>
              <label className="field inline">
                <span>{tr("log_level")}</span>
                <select
                  className="log-level-select"
                  value={logLevel}
                  onChange={(event) => setLogLevel(event.target.value as LogLevel)}
                >
                  <option value="debug">{tr("log_level_debug")}</option>
                  <option value="info">{tr("log_level_info")}</option>
                  <option value="warn">{tr("log_level_warn")}</option>
                  <option value="error">{tr("log_level_error")}</option>
                </select>
              </label>
              <label className="field inline">
                <span>{tr("save_logs")}</span>
                <input
                  type="checkbox"
                  checked={saveLogs}
                  onChange={(event) => setSaveLogs(event.target.checked)}
                />
              </label>
              <button
                className="btn"
                onClick={() => {
                  if (logTab === "client") {
                    setClientLogs(["Client logs cleared."]);
                  } else {
                    setPayloadLogs(["Payload logs cleared."]);
                  }
                }}
              >
                {tr("clear_logs")}
              </button>
              <div className="log-window">
                {(() => {
                  const source = logTab === "client" ? clientLogs : payloadLogs;
                  const normalized = source.map(normalizeLogEntry);
                  const filtered = normalized.filter(
                    (entry) => LOG_LEVEL_ORDER[entry.level] >= LOG_LEVEL_ORDER[logLevel]
                  );
                  if (filtered.length === 0) {
                    return <p>{tr("no_logs")}</p>;
                  }
                  return filtered
                    .slice(0, 50)
                    .map((entry, index) => (
                      <p key={`${index}`} className={`log-entry log-${entry.level}`}>
                        <span className="log-level">{entry.level.toUpperCase()}</span>{" "}
                        {entry.message}
                      </p>
                    ));
                })()}
              </div>
            </>
          )}
        </section>
      </aside>

      <footer className="status-bar shell">
        <span>{status.transfer}</span>
        <span>
          <a
            href="https://x.com/phantomptr"
            onClick={(event) => {
              event.preventDefault();
              openExternal("https://x.com/phantomptr");
            }}
          >
            Created by PhantomPtr
          </a>{" "}
          |{" "}
          <a
            href="https://github.com/phantomptr/ps5upload"
            onClick={(event) => {
              event.preventDefault();
              openExternal("https://github.com/phantomptr/ps5upload");
            }}
          >
            Source Code
          </a>
        </span>
      </footer>

      {manageModalOpen && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">{manageModalOp} Progress</header>
            <p className="muted">{manageModalStatus || manageStatus}</p>
            {!manageModalDone && (
              <>
                <div className="progress">
                  <div
                    className={`progress-fill${manageProgress.total === 0 && manageProgress.processed > 0 ? " streaming" : ""}`}
                    style={{
                      width:
                        manageProgress.total > 0
                          ? `${Math.min(
                              100,
                              (manageProgress.processed /
                                manageProgress.total) *
                                100
                            )}%`
                          : manageProgress.processed > 0
                            ? "100%"
                            : "0%"
                    }}
                  />
                </div>
                <div className="progress-meta">
                  <span>
                    {manageProgress.total > 0
                      ? `${formatBytes(manageProgress.processed)} / ${formatBytes(manageProgress.total)}`
                      : `${formatBytes(manageProgress.processed)} transferred`}
                  </span>
                  <span>
                    {manageProgress.speed_bps > 0
                      ? `${formatBytes(manageProgress.speed_bps)}/s`
                      : "—"}
                  </span>
                  <span>
                    {manageProgress.total > 0
                      ? `${Math.round(
                          (manageProgress.processed /
                            manageProgress.total) *
                            100
                        )}%`
                      : "streaming"}
                  </span>
                </div>
                {manageModalLastProgressAt && (
                  <div className="muted small" style={{ marginTop: "6px" }}>
                    Last activity {Math.max(0, Math.round((Date.now() - manageModalLastProgressAt) / 1000))}s ago
                  </div>
                )}
                {manageProgress.currentFile && (
                  <div className="pill">{manageProgress.currentFile}</div>
                )}
              </>
            )}
            {manageModalDone && manageModalOp === "Upload" && manageModalSummary && (
              <div className="pill">{manageModalSummary}</div>
            )}
            <div className="split">
              {!manageModalDone ? (
                <button className="btn danger" onClick={handleManageCancel}>
                  {tr("stop")}
                </button>
              ) : (
                <button
                  className="btn primary"
                  onClick={() => {
                  setManageModalOpen(false);
                  setManageModalDone(false);
                  setManageModalError(null);
                  setManageModalStatus("");
                  setManageModalSummary(null);
                  setManageModalStartedAt(null);
                  manageModalStartedAtRef.current = null;
                  setManageModalLastProgressAt(null);
                }}
              >
                OK
              </button>
              )}
            </div>
          </div>
        </div>
      )}

      {uploadInfoItem && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">{tr("upload_parameters")}</header>
            {(() => {
              const settings = uploadInfoItem.transfer_settings || {};
              const effectiveIp = uploadInfoItem.ps5_ip || ip.trim() || "—";
              const destPath = buildDestPathForItem(
                uploadInfoItem.storage_base || storageRoot,
                uploadInfoItem
              );
              const bandwidth =
                (settings.bandwidth_limit_mbps ?? bandwidthLimit) > 0
                  ? `${settings.bandwidth_limit_mbps ?? bandwidthLimit} Mbps`
                  : tr("unlimited");
              const onLabel = tr("on");
              const offLabel = tr("off");
              const isActiveItem = uploadInfoItem.id === currentQueueItemId;
              const effectiveOptimize =
                isActiveItem && transferState.effectiveOptimize != null
                  ? transferState.effectiveOptimize
                  : settings.optimize_upload ?? optimizeActive;
              const rarTempLabel = settings.rar_temp_root ?? rarTemp;
              const resumeModeValue = normalizeResumeMode(settings.resume_mode ?? resumeMode);
              const resumeModeLabel =
                resumeModeValue === "none"
                  ? tr("resume_off")
                  : resumeModeValue === "size"
                  ? tr("resume_fastest")
                  : resumeModeValue === "hash_large"
                  ? tr("resume_faster")
                  : resumeModeValue === "hash_medium"
                  ? tr("resume_fast_hash")
                  : tr("resume_normal");
              const resumeBehavior =
                resumeModeValue !== "none"
                  ? tr("resume_behavior_skip")
                  : tr("resume_behavior_reupload");
              const tempEffective = (settings.use_temp ?? useTemp)
                ? onLabel
                : offLabel;
              const lastRunLabel =
                uploadInfoItem.last_run_action === "resume"
                  ? tr("resumed")
                  : uploadInfoItem.last_run_action === "requeue"
                  ? tr("requeued")
                  : tr("new");
              const lastFailedParts: string[] = [];
              const lastFailedReason =
                uploadInfoItem.status && typeof uploadInfoItem.status === "object"
                  ? (uploadInfoItem.status as { Failed: string }).Failed
                  : null;
              const lastFailedIsStopped =
                lastFailedReason === USER_STOPPED_SENTINEL ||
                lastFailedReason === tr("stopped");
              if (uploadInfoItem.last_failed_at) {
                lastFailedParts.push(
                  tr("failed_at", { time: formatTimestamp(uploadInfoItem.last_failed_at) })
                );
              }
              if (uploadInfoItem.last_failed_bytes != null) {
                if (uploadInfoItem.last_failed_total_bytes && uploadInfoItem.last_failed_total_bytes > 0) {
                  lastFailedParts.push(
                    `${formatBytes(uploadInfoItem.last_failed_bytes)} / ${formatBytes(
                      uploadInfoItem.last_failed_total_bytes
                    )}`
                  );
                } else {
                  lastFailedParts.push(
                    `${formatBytes(uploadInfoItem.last_failed_bytes)} transferred`
                  );
                }
              }
              if (uploadInfoItem.last_failed_files != null) {
                lastFailedParts.push(`${uploadInfoItem.last_failed_files} ${tr("files")}`);
              }
              if (uploadInfoItem.last_failed_elapsed_sec != null) {
                lastFailedParts.push(
                  `Elapsed ${formatDuration(uploadInfoItem.last_failed_elapsed_sec)}`
                );
              }
              return (
                <div className="info-modal">
                  <div className="info-title">
                    {uploadInfoItem.subfolder_name || getBaseName(uploadInfoItem.source_path)}
                  </div>

                  <div className="info-section">
                    <div className="info-section-title">{tr("source")}</div>
                    <div className="info-grid">
                      <div className="info-row">
                        <div className="info-label">{tr("source")}</div>
                        <div className="info-value">{uploadInfoItem.source_path}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("subfolder")}</div>
                        <div className="info-value">{uploadInfoItem.subfolder_name || "—"}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("destination")}</div>
                        <div className="info-value">{destPath}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("storage_label")}</div>
                        <div className="info-value">{uploadInfoItem.storage_base || storageRoot}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("ps5_ip")}</div>
                        <div className="info-value">{effectiveIp}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("size")}</div>
                        <div className="info-value">
                          {uploadInfoItem.size_bytes != null
                            ? formatBytes(uploadInfoItem.size_bytes)
                            : "—"}
                        </div>
                      </div>
                    </div>
                  </div>

                  <div className="info-section">
                    <div className="info-section-title">{tr("transfer_settings")}</div>
                    <div className="info-grid">
                      <div className="info-row">
                        <div className="info-label">{tr("connections")}</div>
                        <div className="info-value">{settings.connections ?? connections}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("temp_short")}</div>
                        <div className="info-value">
                          <span className={`info-badge ${tempEffective === onLabel ? "on" : "off"}`}>
                            {tempEffective}
                          </span>
                        </div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("resume_mode")}</div>
                        <div className="info-value">{resumeModeLabel}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("resume")}</div>
                        <div className="info-value">{resumeBehavior}</div>
                      </div>
                      {resumeModeValue !== "none" && (
                        <div className="info-note">
                          {tr("resume_file_level")}
                        </div>
                      )}
                      <div className="info-row">
                        <div className="info-label">{tr("compression")}</div>
                        <div className="info-value">{settings.compression ?? compression}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("bandwidth")}</div>
                        <div className="info-value">{bandwidth}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("auto_tune_short")}</div>
                        <div className="info-value">
                          <span className={`info-badge ${(settings.auto_tune_connections ?? autoTune) ? "on" : "off"}`}>
                            {(settings.auto_tune_connections ?? autoTune) ? onLabel : offLabel}
                          </span>
                        </div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("optimize_short")}</div>
                        <div className="info-value">
                          <span className={`info-badge ${effectiveOptimize ? "on" : "off"}`}>
                            {effectiveOptimize ? onLabel : offLabel}
                          </span>
                        </div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("override_short")}</div>
                        <div className="info-value">
                          <span className={`info-badge ${(settings.override_on_conflict ?? overrideOnConflict) ? "on" : "off"}`}>
                            {(settings.override_on_conflict ?? overrideOnConflict) ? onLabel : offLabel}
                          </span>
                        </div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("chmod_after")}</div>
                        <div className="info-value">
                          <span className={`info-badge ${(settings.chmod_after_upload ?? chmodAfterUpload) ? "on" : "off"}`}>
                            {(settings.chmod_after_upload ?? chmodAfterUpload) ? onLabel : offLabel}
                          </span>
                        </div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("rar_mode")}</div>
                        <div className="info-value">{settings.rar_extract_mode ?? rarExtractMode}</div>
                      </div>
                      <div className="info-row">
                        <div className="info-label">{tr("rar_temp")}</div>
                        <div className="info-value">{rarTempLabel || "—"}</div>
                      </div>
                    </div>
                  </div>

                  <div className="info-section">
                    <div className="info-section-title">{tr("last_run")}</div>
                    <div className="info-grid">
                      <div className="info-row">
                        <div className="info-label">{tr("last_run")}</div>
                        <div className="info-value">{lastRunLabel}</div>
                      </div>
                      {lastFailedParts.length > 0 && (
                        <div className="info-row">
                          <div className="info-label">{tr("last_failed")}</div>
                          <div className="info-value">{lastFailedParts.join(" · ")}</div>
                        </div>
                      )}
                      {lastFailedReason && (
                        <div className="info-row">
                          <div className="info-label">{lastFailedIsStopped ? tr("stopped") : tr("failed")}</div>
                          <div className="info-value">
                            {lastFailedIsStopped ? tr("stopped_by_user") : lastFailedReason}
                          </div>
                        </div>
                      )}
                    </div>
                  </div>
                </div>
              );
            })()}
            <div className="split" style={{ marginTop: 12 }}>
              <button className="btn primary" onClick={() => setUploadInfoItem(null)}>
                {tr("ok")}
              </button>
            </div>
          </div>
        </div>
      )}

      {manageDestOpen && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">
              {tr("select_destination")}{" "}
              {manageActionLabel ? `(${manageActionLabel})` : ""}
            </header>
            <div className="path-row">
              <input
                value={manageDestPath}
                onChange={(event) => setManageDestPath(event.target.value)}
              />
              <button className="btn" onClick={() => refreshManageDest(manageDestPath)}>
                {tr("go")}
              </button>
              <button className="btn" onClick={handleManageDestUp}>
                {tr("up")}
              </button>
              <button className="btn" onClick={() => refreshManageDest()}>
                {tr("refresh")}
              </button>
            </div>
            <p className="muted">{manageDestStatus}</p>
              <div className="table">
                <div className="table-row header">
                  <span>{tr("name")}</span>
                  <span>{tr("type")}</span>
                  <span>{tr("size")}</span>
                  <span>{tr("modified")}</span>
                </div>
                {manageDestEntries.map((entry, index) => (
                  <div
                    className={`table-row ${manageDestSelected === index ? "selected" : ""}`}
                    key={`${entry.name}-${index}`}
                    onClick={() => setManageDestSelected(index)}
                    onDoubleClick={() => {
                      setManageDestPath(joinRemote(manageDestPath, entry.name));
                      refreshManageDest(joinRemote(manageDestPath, entry.name));
                    }}
                  >
                    <span>▸ {entry.name}</span>
                    <span>{getEntryType(entry)}</span>
                    <span>-</span>
                    <span>{formatTimestamp(entry.mtime)}</span>
                  </div>
                ))}
              </div>
              {manageSelectedEntry && getEntryType(manageSelectedEntry) === "file" && (
                <label className="field">
                  <span>Destination file name</span>
                  <input
                    value={manageDestFilename}
                    onChange={(event) => setManageDestFilename(event.target.value)}
                  />
                </label>
              )}
            <p className="muted">
              {tr("destination")}: {manageDestPreview}
            </p>
            <div className="split">
              <button className="btn primary" onClick={handleConfirmDest}>
                {tr("select_here")}
              </button>
              <button
                className="btn ghost"
                onClick={() => {
                  setManageDestOpen(false);
                  setManageDestAction(null);
                }}
              >
                {tr("cancel")}
              </button>
            </div>
          </div>
        </div>
      )}

      {noticeOpen && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">{noticeTitle}</header>
            <div className="stack">
              {noticeLines.map((line, index) => (
                <div key={`${index}-${line}`} className="muted">
                  {line}
                </div>
              ))}
            </div>
            <div className="split">
              <button
                className="btn primary"
                onClick={() => {
                  setNoticeOpen(false);
                  setNoticeTitle("");
                  setNoticeLines([]);
                }}
              >
                OK
              </button>
            </div>
          </div>
        </div>
      )}

      {showRenamePrompt && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">{tr("rename")}</header>
            <label className="field">
              <span>{tr("new_name")}</span>
              <input
                value={renameValue}
                onChange={(event) => setRenameValue(event.target.value)}
              />
            </label>
            <div className="split">
              <button className="btn primary" onClick={handleConfirmRename}>
                {tr("rename")}
              </button>
              <button
                className="btn ghost"
                onClick={() => setShowRenamePrompt(false)}
              >
                {tr("cancel")}
              </button>
            </div>
          </div>
        </div>
      )}

      {showResumePrompt && (resumeRecord || resumeQueueItem) && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">{tr("resume_mode")}</header>
            {(() => {
              const sourcePath = resumeRecord?.source_path ?? resumeQueueItem?.source_path ?? "";
              const destPath =
                resumeRecord?.dest_path ??
                (resumeQueueItem
                  ? buildDestPathForItem(
                      resumeQueueItem.storage_base || storageRoot,
                      resumeQueueItem
                    )
                  : "");
              const resumeSupported = sourcePath ? !isArchivePath(sourcePath) : true;
              return (
                <>
                  <p className="muted">{tr("resume_note")}</p>
                  <p className="muted small">{tr("resume_file_level")}</p>
                  <div className="muted small">
                    {tr("source")}: {sourcePath || "—"}
                  </div>
                  <div className="muted small">
                    {tr("destination")}: {destPath || "—"}
                  </div>
                  {!resumeSupported && (
                    <div className="muted small">{tr("resume_unsupported_archive")}</div>
                  )}
                  {resumeSupported && (
                    <label className="field">
                      <span>{tr("resume_mode")}</span>
                      <select
                        value={resumeChoice}
                        onChange={(event) => setResumeChoice(event.target.value as ResumeOption)}
                      >
                        <option value="size">{tr("resume_fastest")}</option>
                        <option value="hash_large">{tr("resume_faster")}</option>
                        <option value="hash_medium">{tr("resume_fast_hash")}</option>
                        <option value="sha256">{tr("resume_normal")}</option>
                      </select>
                    </label>
                  )}
                </>
              );
            })()}
            <div className="split">
              {resumeQueueItem ? (
                <button className="btn primary" onClick={handleConfirmResumeFromQueue}>
                  {tr("resume")}
                </button>
              ) : (
                <button className="btn primary" onClick={handleConfirmResumeFromHistory}>
                  {tr("resume")}
                </button>
              )}
              <button
                className="btn ghost"
                onClick={() => {
                  setShowResumePrompt(false);
                  setResumeRecord(null);
                  setResumeQueueItem(null);
                }}
              >
                {tr("cancel")}
              </button>
            </div>
          </div>
        </div>
      )}

      {showDeleteConfirm && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">{tr("delete")}</header>
            <p>
              {tr("delete_confirm")
                .replace(
                  "{item}",
                  manageSelectedEntry ? manageSelectedEntry.name : tr("item")
                )
                .replace("{warning}", tr("cannot_undo"))}
            </p>
            <div className="split">
              <button className="btn primary" onClick={handleConfirmDelete}>
                {tr("delete")}
              </button>
              <button
                className="btn ghost"
                onClick={() => setShowDeleteConfirm(false)}
              >
                {tr("cancel")}
              </button>
            </div>
          </div>
        </div>
      )}

      {showCreatePrompt && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">{tr("new_folder_title")}</header>
            <label className="field">
              <span>{tr("folder_name")}</span>
              <input
                value={newFolderName}
                onChange={(event) => setNewFolderName(event.target.value)}
              />
            </label>
            <div className="split">
              <button className="btn primary" onClick={handleConfirmCreateFolder}>
                {tr("create")}
              </button>
              <button
                className="btn ghost"
                onClick={() => setShowCreatePrompt(false)}
              >
                {tr("cancel")}
              </button>
            </div>
          </div>
        </div>
      )}

      {showProfileCreatePrompt && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">{tr("new_profile")}</header>
            <label className="field">
              <span>{tr("new_profile_name")}</span>
              <input
                value={newProfileName}
                onChange={(event) => setNewProfileName(event.target.value)}
              />
            </label>
            {newProfileExists && (
              <p className="muted warn small">{tr("profile_exists_error")}</p>
            )}
            <div className="split">
              <button
                className="btn primary"
                onClick={() => {
                  handleCreateProfile();
                  setShowProfileCreatePrompt(false);
                }}
                disabled={!trimmedNewProfileName || newProfileExists}
              >
                {tr("create_profile")}
              </button>
              <button
                className="btn ghost"
                onClick={() => {
                  setShowProfileCreatePrompt(false);
                  setProfileSelectValue(currentProfile ?? "");
                }}
              >
                {tr("cancel")}
              </button>
            </div>
          </div>
        </div>
      )}

      {showProfileDeleteConfirm && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-title">{tr("delete_profile")}</header>
            <p>
              {tr("delete_profile_confirm").replace(
                "{name}",
                currentProfile ?? ""
              )}
            </p>
            <div className="split">
              <button
                className="btn danger"
                onClick={handleConfirmDeleteProfile}
              >
                {tr("delete")}
              </button>
              <button
                className="btn ghost"
                onClick={() => setShowProfileDeleteConfirm(false)}
              >
                {tr("cancel")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
