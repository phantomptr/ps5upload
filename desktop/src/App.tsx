import { useEffect, useMemo, useRef, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import {
  getCurrentWindow,
  currentMonitor,
  LogicalPosition,
  LogicalSize
} from "@tauri-apps/api/window";
import { t } from "./i18n";

const appWindow = getCurrentWindow();

type TabId = "transfer" | "payload" | "manage" | "chat";

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

type ManageProgressEvent = {
  op: string;
  processed: number;
  total: number;
  current_file?: string | null;
};

type ManageDoneEvent = {
  op: string;
  bytes?: number | null;
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

type QueueItem = {
  id: number;
  source_path: string;
  subfolder_name: string;
  preset_index: number;
  custom_preset_path: string;
  storage_base?: string;
  dest_path?: string;
  status: QueueStatus;
  size_bytes?: number | null;
};

type QueueData = {
  items: QueueItem[];
  next_id: number;
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
};

type HistoryData = {
  records: TransferRecord[];
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
  window_width: number;
  window_height: number;
  window_x: number;
  window_y: number;
};

type ThemeMode = "dark" | "light";

const presetOptions = ["etaHEN/games", "homebrew", "custom"] as const;

type CompressionOption = "auto" | "none" | "lz4" | "zstd" | "lzma";

type ResumeOption = "none" | "size" | "size_mtime" | "sha256";

type DownloadCompressionOption = "auto" | "none" | "lz4" | "zstd" | "lzma";

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

type ExtractQueueItem = {
  id: number;
  archive_name: string;
  status: string;
  percent: number;
  processed_bytes: number;
  total_bytes: number;
  files_extracted: number;
  started_at: number;
  completed_at: number;
  error?: string | null;
};

type PayloadStatusResponse = {
  version: string;
  uptime: number;
  queue_count: number;
  is_busy: boolean;
  items: ExtractQueueItem[];
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

  const handleToggleTheme = () => {
    setTheme((prev) => (prev === "dark" ? "light" : "dark"));
  };

  const [appVersion, setAppVersion] = useState("...");
  const [profilesData, setProfilesData] = useState<ProfilesData>({
    profiles: [],
    default_profile: null
  });
  const [currentProfile, setCurrentProfile] = useState<string | null>(null);
  const [profileSelectValue, setProfileSelectValue] = useState("");
  const [showProfileCreatePrompt, setShowProfileCreatePrompt] = useState(false);
  const [newProfileName, setNewProfileName] = useState("");
  const [queueData, setQueueData] = useState<QueueData>({ items: [], next_id: 1 });
  const [currentQueueItemId, setCurrentQueueItemId] = useState<number | null>(null);
  const [historyData, setHistoryData] = useState<HistoryData>({ records: [] });
  const [logTab, setLogTab] = useState<"client" | "payload" | "history">("client");
  const [clientLogs, setClientLogs] = useState<string[]>([]);
  const [payloadLogs, setPayloadLogs] = useState<string[]>([]);
  const clientLogBuffer = useRef<string[]>([]);
  const payloadLogBuffer = useRef<string[]>([]);
  const clientLogFlush = useRef<ReturnType<typeof setTimeout> | null>(null);
  const payloadLogFlush = useRef<ReturnType<typeof setTimeout> | null>(null);
  const profileAutosaveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const configSaveRef = useRef<AppConfig | null>(null);
  const resizeSaveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [payloadLocalPath, setPayloadLocalPath] = useState("");
  const [payloadStatus, setPayloadStatus] = useState("Unknown");
  const [payloadVersion, setPayloadVersion] = useState<string | null>(null);
  const [payloadBusy, setPayloadBusy] = useState(false);
  const [updateInfo, setUpdateInfo] = useState<ReleaseInfo | null>(null);
  const [updateStatus, setUpdateStatus] = useState("Checking for updates...");
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const [updateDownloadStatus, setUpdateDownloadStatus] = useState("");
  const [updatePending, setUpdatePending] = useState(false);
  const [includePrerelease, setIncludePrerelease] = useState(false);
  const [currentAssetName, setCurrentAssetName] = useState<string | null>(null);
  const [language, setLanguage] = useState("en");
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
  const payloadStatusInterval = useRef<ReturnType<typeof setInterval> | null>(null);
  const [downloadCompression, setDownloadCompression] =
    useState<DownloadCompressionOption>("auto");
  const [chmodAfterUpload, setChmodAfterUpload] = useState(true);
  const [rarExtractMode, setRarExtractMode] = useState<RarExtractMode>("normal");
  const [chatDisplayName, setChatDisplayName] = useState("");
  const [configDefaults, setConfigDefaults] = useState<AppConfig | null>(null);
  const [configLoaded, setConfigLoaded] = useState(false);
  const [ip, setIp] = useState("");
  const [connectionStatus, setConnectionStatus] = useState("Disconnected");
  const [isConnected, setIsConnected] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);
  const [storageLocations, setStorageLocations] = useState<StorageLocation[]>([]);
  const [sourcePath, setSourcePath] = useState("");
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
  const [optimizeUpload, setOptimizeUpload] = useState(false);
  const [autoTune, setAutoTune] = useState(true);
  const [useTemp, setUseTemp] = useState(false);
  const [managePath, setManagePath] = useState("/data");
  const [manageEntries, setManageEntries] = useState<DirEntry[]>([]);
  const [manageStatus, setManageStatus] = useState("Not connected");
  const [manageSelected, setManageSelected] = useState<number | null>(null);
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
  const [manageBusy, setManageBusy] = useState(false);
  const [manageProgress, setManageProgress] = useState({
    op: "",
    processed: 0,
    total: 0,
    currentFile: "",
    speed_bps: 0
  });
  const manageSpeedRef = useRef({
    op: "",
    processed: 0,
    at: 0,
    speed: 0
  });
  const [manageMeta, setManageMeta] = useState<GameMetaPayload | null>(null);
  const [manageCoverUrl, setManageCoverUrl] = useState<string | null>(null);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [showRenamePrompt, setShowRenamePrompt] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [showCreatePrompt, setShowCreatePrompt] = useState(false);
  const [newFolderName, setNewFolderName] = useState("");
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
  const NEW_PROFILE_OPTION = "__new_profile__";
  const trimmedNewProfileName = newProfileName.trim();
  const newProfileExists =
    trimmedNewProfileName.length > 0 &&
    (trimmedNewProfileName === NEW_PROFILE_OPTION ||
      profilesData.profiles.some((profile) => profile.name === trimmedNewProfileName));
  const isRtl = language === "ar";
  const tr = (key: string) => t(language, key);
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
  const tabs = useMemo(
    () => [
      { id: "transfer" as TabId, label: tr("transfer"), icon: "↑" },
      { id: "payload" as TabId, label: tr("payload"), icon: "◎" },
      { id: "manage" as TabId, label: tr("manage"), icon: "≡" },
      { id: "chat" as TabId, label: tr("chat"), icon: "◈" }
    ],
    [language]
  );

  useEffect(() => {
    document.documentElement.dir = isRtl ? "rtl" : "ltr";
    document.documentElement.lang = language;
  }, [isRtl, language]);
  const [transferState, setTransferState] = useState<TransferState>({
    status: "Idle",
    sent: 0,
    total: 0,
    files: 0,
    elapsed: 0,
    currentFile: ""
  });
  const [activeRunId, setActiveRunId] = useState<number | null>(null);
  const lastProgressUpdate = useRef(0);
  const [activeTransferSource, setActiveTransferSource] = useState("");
  const [activeTransferDest, setActiveTransferDest] = useState("");
  const [activeTransferViaQueue, setActiveTransferViaQueue] = useState(false);
  const [transferStartedAt, setTransferStartedAt] = useState<number | null>(null);
  const transferSnapshot = useRef({
    runId: null as number | null,
    source: "",
    dest: "",
    viaQueue: false,
    startedAt: null as number | null,
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
  const manageSnapshot = useRef({
    ip: "",
    path: "/data"
  });
  const manageClickRef = useRef<{ index: number | null; time: number }>({
    index: null,
    time: 0
  });
  const manageRefreshSkip = useRef<string | null>(null);
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
    transferSnapshot.current = {
      runId: activeRunId,
      source: activeTransferSource,
      dest: activeTransferDest,
      viaQueue: activeTransferViaQueue,
      startedAt: transferStartedAt,
      state: transferState
    };
  }, [
    activeRunId,
    activeTransferSource,
    activeTransferDest,
    activeTransferViaQueue,
    transferStartedAt,
    transferState
  ]);

  useEffect(() => {
    queueSnapshot.current = {
      data: queueData,
      currentId: currentQueueItemId
    };
  }, [queueData, currentQueueItemId]);

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
      await invoke("profiles_save", { data });
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
    const ok = window.confirm(
      tr("delete_profile_confirm").replace("{name}", currentProfile)
    );
    if (!ok) return;
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
        setResumeMode((normalizedConfig.resume_mode as ResumeOption) || "none");
        setAutoTune(normalizedConfig.auto_tune_connections ?? true);
        setOptimizeUpload(normalizedConfig.optimize_upload ?? false);
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
        setChatDisplayName(normalizedConfig.chat_display_name ?? "");
        setIncludePrerelease(normalizedConfig.update_channel === "all");
        setLanguage(normalizedConfig.language || "en");

        if (!normalizedConfig.chat_display_name?.trim() && active) {
          setChatDisplayName(generateChatName());
        }
      } catch (err) {
        setClientLogs((prev) => [`Failed to load config: ${String(err)}`, ...prev]);
      }

      // Load update asset name - independent
      try {
        const assetName = await invoke<string>("update_current_asset_name");
        if (active) {
          setCurrentAssetName(assetName);
        }
      } catch {
        if (active) {
          setCurrentAssetName(null);
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
        setQueueData(queue);
      } catch (err) {
        setClientLogs((prev) => [`Failed to load queue: ${String(err)}`, ...prev]);
      }

      // Load history - independent
      try {
        const history = await invoke<HistoryData>("history_load");
        if (!active) return;
        setHistoryData(history);
      } catch (err) {
        setClientLogs((prev) => [`Failed to load history: ${String(err)}`, ...prev]);
      }

      // Load chat - independent
      try {
        const info = await invoke<ChatInfo>("chat_info");
        if (!active) return;
        setChatRoomId(info.room_id);
        setChatEnabled(info.enabled);
        if (info.enabled) {
          setChatStatus("Connecting...");
          const started = await invoke<ChatInfo>("chat_start");
          if (active) {
            setChatRoomId(started.room_id);
            setChatEnabled(started.enabled);
          }
        } else if (active) {
          setChatStatus("Disabled");
        }
      } catch {
        if (active) {
          setChatEnabled(false);
          setChatStatus("Disabled");
        }
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

    load();
    return () => {
      active = false;
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
    if (profileAutosaveTimer.current) {
      clearTimeout(profileAutosaveTimer.current);
    }
    profileAutosaveTimer.current = setTimeout(() => {
      const updatedProfiles = profilesData.profiles.map((profile) =>
        profile.name === currentProfile ? nextProfile : profile
      );
      persistProfiles({
        ...profilesData,
        profiles: updatedProfiles
      });
    }, 500);
    return () => {
      if (profileAutosaveTimer.current) {
        clearTimeout(profileAutosaveTimer.current);
        profileAutosaveTimer.current = null;
      }
    };
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
            invoke("config_save", {
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
          invoke("config_save", {
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
    if (!subfolder && sourcePath) {
      const name = sourcePath.split(/[/\\]/).filter(Boolean).pop();
      if (name) {
        const clean = name.replace(/\.(zip|7z|rar)$/i, "");
        setSubfolder(clean || name);
      }
    }
  }, [sourcePath, subfolder]);

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
    if (activeTab === "manage") {
      handleManageRefresh();
    }
  }, [activeTab]);

  useEffect(() => {
    if (activeTab === "manage") {
      if (manageRefreshSkip.current === managePath) {
        manageRefreshSkip.current = null;
        return;
      }
      handleManageRefresh();
    }
  }, [managePath]);

  useEffect(() => {
    setManageEntries((prev) => sortEntries(prev, manageSort));
  }, [manageSort]);

  useEffect(() => {
    let active = true;
    const entry = manageSelectedEntry;
    if (
      !entry ||
      entry.entry_type !== "file" ||
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
    if (!autoConnect || isConnected || isConnecting || !ip.trim()) return undefined;
    const timer = setInterval(() => {
      if (!isConnected && !isConnecting) {
        handleConnect();
      }
    }, 5000);
    return () => clearInterval(timer);
  }, [autoConnect, isConnected, isConnecting, ip]);

  useEffect(() => {
    if (!payloadAutoReload || !isConnected || !ip.trim()) {
      return undefined;
    }
    const timer = setInterval(() => {
      runPayloadReload();
    }, 60000);
    return () => clearInterval(timer);
  }, [payloadAutoReload, payloadReloadMode, payloadLocalPath, isConnected, ip, payloadBusy]);

  useEffect(() => {
    if (payloadAutoReload && isConnected && ip.trim()) {
      runPayloadReload();
    }
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
      optimize_upload: optimizeUpload,
      update_channel: includePrerelease ? "all" : "stable",
      language,
      auto_check_payload: payloadAutoReload,
      payload_auto_reload: payloadAutoReload,
      payload_reload_mode: payloadReloadMode,
      payload_local_path: payloadLocalPath,
      download_compression: downloadCompression,
      chmod_after_upload: chmodAfterUpload,
      chat_display_name: chatDisplayName,
      rar_extract_mode: rarExtractMode
    };

    configSaveRef.current = nextConfig;
    const handle = setTimeout(() => {
      invoke("config_save", { config: nextConfig }).catch((err) => {
        setClientLogs((prev) => [
          `Failed to save config: ${String(err)}`,
          ...prev
        ]);
      });
    }, 80);

    return () => clearTimeout(handle);
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
    optimizeUpload,
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
    rarExtractMode
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
          const now = Date.now();
          if (now - lastProgressUpdate.current < 100) {
            return;
          }
          lastProgressUpdate.current = now;
          const snapshot = transferSnapshot.current;
          if (snapshot.runId && event.payload.run_id !== snapshot.runId) return;
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
          const snapshot = transferSnapshot.current;
          if (snapshot.runId && event.payload.run_id !== snapshot.runId) return;
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
          const snapshot = transferSnapshot.current;
          if (snapshot.runId && event.payload.run_id !== snapshot.runId) return;
          const payload = event.payload;
          setTransferState((prev) => ({
            ...prev,
            status: "Complete",
            sent: payload.bytes,
            total: payload.bytes
          }));
          const duration =
            snapshot.state.elapsed ||
            (snapshot.startedAt ? (Date.now() - snapshot.startedAt) / 1000 : 0);
          const speed = duration > 0 ? payload.bytes / duration : 0;
          if (snapshot.source && snapshot.dest) {
            const record: TransferRecord = {
              timestamp: Math.floor(Date.now() / 1000),
              source_path: snapshot.source,
              dest_path: snapshot.dest,
              file_count: payload.files,
              total_bytes: payload.bytes,
              duration_secs: duration,
              speed_bps: speed,
              success: true,
              via_queue: snapshot.viaQueue
            };
            setHistoryData((prev) => ({
              records: [...prev.records, record].slice(-100)
            }));
            invoke("history_add", { record }).catch(() => {
              setClientLogs((prev) => [
                "Failed to save history entry.",
                ...prev
              ]);
            });
          }
          if (snapshot.dest && configSnapshot.current.chmodAfterUpload) {
            invoke("manage_chmod", {
              ip: configSnapshot.current.ip,
              path: snapshot.dest
            }).catch(() => {
              // ignore chmod failures in UI
            });
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
        }
      );
      const unlistenError = await listen<TransferErrorEvent>(
        "transfer_error",
        (event) => {
          if (!mounted) return;
          const snapshot = transferSnapshot.current;
          if (snapshot.runId && event.payload.run_id !== snapshot.runId) return;
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
              via_queue: snapshot.viaQueue
            };
            setHistoryData((prev) => ({
              records: [...prev.records, record].slice(-100)
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
            updateQueueItemStatus(id, { Failed: event.payload.message }).then(
              () => {
                processNextQueueItem();
              }
            );
          } else {
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
          setManageBusy(true);
          const now = Date.now();
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
          if (event.payload.error) {
            setManageStatus(`${event.payload.op} failed: ${event.payload.error}`);
            setClientLogs((prev) =>
              [
                `${event.payload.op} failed: ${event.payload.error}`,
                ...prev
              ].slice(0, 100)
            );
          } else {
            setManageStatus(`${event.payload.op} complete`);
            const snapshot = manageSnapshot.current;
            if (snapshot.ip.trim()) {
              invoke<DirEntry[]>("manage_list", {
                ip: snapshot.ip,
                path: snapshot.path
                })
                .then((entries) => {
                  if (!mounted) return;
                  const sorted = sortEntries(entries, { key: "name", direction: "asc" });
                  setManageEntries(sorted);
                  setManageSelected(null);
                  setManageMeta(null);
                  setManageCoverUrl(null);
                  setManageStatus(`Loaded ${sorted.length} items`);
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

      return () => {
        if (clientLogFlush.current) {
          clearTimeout(clientLogFlush.current);
          clientLogFlush.current = null;
        }
        if (payloadLogFlush.current) {
          clearTimeout(payloadLogFlush.current);
          payloadLogFlush.current = null;
        }
        unlistenProgress();
        unlistenScan();
        unlistenComplete();
        unlistenError();
        unlistenLog();
        unlistenPayloadLog();
        unlistenPayloadDone();
        unlistenPayloadVersion();
        unlistenUpdateReady();
        unlistenUpdateError();
        unlistenManageProgress();
        unlistenManageDone();
        unlistenManageLog();
        unlistenChatMessage();
        unlistenChatStatus();
        unlistenChatAck();
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
  const transferSpeed =
    transferState.elapsed > 0 ? transferState.sent / transferState.elapsed : 0;

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

  const fetchPayloadStatus = async () => {
    if (!ip.trim() || !isConnected) return;
    try {
      const status = await invoke<PayloadStatusResponse>("payload_status", { ip });
      setPayloadFullStatus(status);
    } catch (err) {
      console.error("Failed to fetch payload status:", err);
      // Don't spam logs on every poll, only log once
      if (!payloadFullStatus) {
        setClientLogs((prev) => [`Payload status error: ${String(err)}`, ...prev].slice(0, 100));
      }
    }
  };

  const handleRefreshPayloadStatus = async () => {
    if (!ip.trim()) return;
    setPayloadStatusLoading(true);
    try {
      const status = await invoke<PayloadStatusResponse>("payload_status", { ip });
      setPayloadFullStatus(status);
    } catch (err) {
      setClientLogs((prev) => [`Payload status error: ${String(err)}`, ...prev].slice(0, 100));
    }
    setPayloadStatusLoading(false);
  };

  const handleQueueExtract = async (src: string, dst: string) => {
    if (!ip.trim()) return;
    try {
      const id = await invoke<number>("payload_queue_extract", { ip, src, dst });
      setClientLogs((prev) => [`Queued extraction (ID: ${id}): ${src}`, ...prev].slice(0, 100));
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

  // Auto-refresh payload status when on payload tab
  useEffect(() => {
    if (activeTab === "payload" && isConnected && ip.trim()) {
      fetchPayloadStatus();
      if (!payloadStatusInterval.current) {
        payloadStatusInterval.current = setInterval(fetchPayloadStatus, 5000);
      }
    } else {
      if (payloadStatusInterval.current) {
        clearInterval(payloadStatusInterval.current);
        payloadStatusInterval.current = null;
      }
    }
    return () => {
      if (payloadStatusInterval.current) {
        clearInterval(payloadStatusInterval.current);
        payloadStatusInterval.current = null;
      }
    };
  }, [activeTab, isConnected, ip]);

  const handleConnect = async () => {
    if (!ip.trim()) {
      setConnectionStatus("Missing IP");
      return false;
    }
    if (isConnecting) return false;
    setIsConnecting(true);
    setConnectionStatus("Connecting...");
    try {
      const portOpen = await invoke<boolean>("port_check", {
        ip,
        port: 9113
      });
      if (!portOpen) {
        setConnectionStatus("Port 9113 closed");
        setIsConnected(false);
        setIsConnecting(false);
        return false;
      }
      const locations = await invoke<StorageLocation[]>("storage_list", { ip });
      const available = locations.filter((loc) => loc.free_gb > 0);
      setStorageLocations(available);
      if (available.length > 0) {
        setStorageRoot(available[0].path);
        if (!managePath || managePath === "/data") {
          setManagePath(available[0].path);
        }
        setConnectionStatus("Connected");
        setManageStatus("Connected");
        setIsConnected(true);
        if (payloadAutoReload) {
          runPayloadReload();
        }
        return true;
      } else {
        setConnectionStatus("No storage");
        setIsConnected(false);
        return false;
      }
    } catch (err) {
      setConnectionStatus(`Error: ${String(err)}`);
      setIsConnected(false);
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
  };

  const handleScan = async () => {
    if (!sourcePath.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing source" }));
      return;
    }
    setTransferState((prev) => ({ ...prev, status: "Scanning" }));
    try {
      const runId = await invoke<number>("transfer_scan", {
        source_path: sourcePath
      });
      setActiveRunId(runId);
    } catch (err) {
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
    }
  };

  const handleOptimizeToggle = () => {
    setOptimizeUpload((prev) => {
      const next = !prev;
      if (next) {
        setAutoTune(true);
        setCompression("auto");
      }
      return next;
    });
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

  const handleBrowseArchive = async () => {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "Archive", extensions: ["zip", "7z", "rar"] }]
      });
      if (typeof selected === "string") {
        setSourcePath(selected);
        if (!subfolder) {
          const name = selected.split(/[/\\]/).filter(Boolean).pop() ?? "";
          const base = name.replace(/\.(zip|7z|rar)$/i, "");
          if (base) {
            setSubfolder(base);
          }
        }
      }
    } catch (err) {
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
    }
  };

  const handlePayloadBrowse = async () => {
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "Payload", extensions: ["elf", "bin"] }]
      });
      if (typeof selected === "string") {
        setPayloadLocalPath(selected);
      }
    } catch (err) {
      setPayloadStatus(`Error: ${String(err)}`);
    }
  };

  const runPayloadReload = async (modeOverride?: "local" | "current" | "latest") => {
    if (payloadBusy || !ip.trim()) return;
    const mode = modeOverride ?? payloadReloadMode;
    if (mode === "local" && !payloadLocalPath.trim()) {
      setPayloadStatus(tr("payload_local_required"));
      return;
    }
    if (mode === "local" && payloadProbe && !payloadProbe.ok) {
      setPayloadStatus(payloadProbe.message);
      return;
    }
    const portOpen = await invoke<boolean>("port_check", {
      ip,
      port: 9021
    });
    if (!portOpen) {
      setPayloadStatus("Port 9021 closed");
      return;
    }
    setPayloadBusy(true);
    setPayloadStatus(tr("payload_sending"));
    try {
      if (mode === "local") {
        await invoke("payload_send", { ip, path: payloadLocalPath });
      } else {
        await invoke("payload_download_and_send", { ip, fetch: mode });
      }
    } catch (err) {
      setPayloadBusy(false);
      setPayloadStatus(`Error: ${String(err)}`);
    }
  };

  const handlePayloadSend = async () => {
    await runPayloadReload();
  };

  const handlePayloadCheck = async () => {
    try {
      setPayloadStatus("Checking...");
      await invoke("payload_check", { ip });
    } catch (err) {
      setPayloadStatus(`Error: ${String(err)}`);
    }
  };

  const handlePayloadDownload = async (kind: "current" | "latest") => {
    await runPayloadReload(kind);
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
      const assetName = await invoke<string>("update_current_asset_name");
      const asset = updateInfo.assets.find((item) => item.name === assetName);
      if (!asset) {
        setUpdateDownloadStatus(`Asset not found: ${assetName}`);
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
    if (!ip.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing IP" }));
      return;
    }
    if (!sourcePath.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing source" }));
      return;
    }
    if (!finalDestPath.trim()) {
      setTransferState((prev) => ({ ...prev, status: "Missing destination" }));
      return;
    }
    if (!(await ensureConnected())) {
      setTransferState((prev) => ({ ...prev, status: "Not connected" }));
      return;
    }

    try {
      const exists = await invoke<boolean>("transfer_check_dest", {
        ip,
        destPath: finalDestPath
      });
      if (exists && !overrideOnConflict) {
        setTransferState((prev) => ({ ...prev, status: tr("destination_exists") }));
        return;
      }
      const runId = await invoke<number>("transfer_start", {
        req: {
          ip,
          source_path: sourcePath,
          dest_path: finalDestPath,
          use_temp: useTemp,
          connections,
          resume_mode: resumeMode,
          compression,
          bandwidth_limit_mbps: bandwidthLimit,
          auto_tune_connections: autoTune,
          optimize_upload: optimizeUpload,
          rar_extract_mode: rarExtractMode,
          payload_version: payloadVersion,
          storage_root: storageRoot,
          required_size: transferState.total || null
        }
      });
      setActiveRunId(runId);
      setActiveTransferSource(sourcePath);
      setActiveTransferDest(finalDestPath);
      setActiveTransferViaQueue(false);
      setTransferStartedAt(Date.now());
    } catch (err) {
      setTransferState((prev) => ({
        ...prev,
        status: `Error: ${String(err)}`
      }));
    }
  };

  const saveQueueData = async (data: QueueData) => {
    setQueueData(data);
    try {
      await invoke("queue_save", { data });
    } catch (err) {
      setClientLogs((prev) => [
        `Failed to save queue: ${String(err)}`,
        ...prev
      ]);
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

  const handleAddToQueue = async () => {
    if (!sourcePath.trim()) return;
    const nextId = queueData.next_id || 1;
    const subfolderName = subfolder || sourcePath.split(/[/\\]/).filter(Boolean).pop() || "App";
    const presetIndex = presetOptions.indexOf(preset);
    const item: QueueItem = {
      id: nextId,
      source_path: sourcePath,
      subfolder_name: subfolderName,
      preset_index: presetIndex === -1 ? 0 : presetIndex,
      custom_preset_path: customPreset,
      storage_base: storageRoot,
      dest_path: finalDestPath,
      status: "Pending",
      size_bytes: transferState.total || null
    };
    const normalizeKey = (value: string) =>
      value.replace(/[/\\]+/g, "/").trim().toLowerCase();
    const itemKey = [
      normalizeKey(item.source_path),
      normalizeKey(item.subfolder_name || ""),
      String(item.preset_index),
      normalizeKey(item.custom_preset_path || ""),
      normalizeKey(item.storage_base || ""),
      normalizeKey(item.dest_path || "")
    ].join("|");
    const duplicate = queueData.items.some((existing) => {
      const existingKey = [
        normalizeKey(existing.source_path),
        normalizeKey(existing.subfolder_name || ""),
        String(existing.preset_index),
        normalizeKey(existing.custom_preset_path || ""),
        normalizeKey(existing.storage_base || ""),
        normalizeKey(existing.dest_path || "")
      ].join("|");
      return existingKey === itemKey;
    });
    if (duplicate) {
      setClientLogs((prev) => [tr("queue_duplicate"), ...prev]);
      return;
    }
    const nextQueue: QueueData = {
      items: [...queueData.items, item],
      next_id: nextId + 1
    };
    await saveQueueData(nextQueue);
  };

  const updateQueueItemStatus = async (id: number, status: QueueStatus) => {
    const nextQueue: QueueData = {
      ...queueSnapshot.current.data,
      items: queueSnapshot.current.data.items.map((item) =>
        item.id === id ? { ...item, status } : item
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

  const handleClearCompletedQueue = async () => {
    const nextQueue: QueueData = {
      ...queueData,
      items: queueData.items.filter(
        (item) => item.status !== "Completed" && typeof item.status !== "object"
      )
    };
    await saveQueueData(nextQueue);
  };

  const handleClearQueue = async () => {
    const nextQueue: QueueData = {
      ...queueData,
      items: []
    };
    setCurrentQueueItemId(null);
    await saveQueueData(nextQueue);
  };

  const handleResumeFromHistory = (record: TransferRecord) => {
    setSourcePath(record.source_path);
    const name = record.source_path.split(/[/\\]/).filter(Boolean).pop() || "";
    setSubfolder(name);
    setActiveTab("transfer");
    setClientLogs((prev) => [`Loaded from history: ${record.source_path}`, ...prev]);
  };

  const startQueueItem = async (item: QueueItem) => {
    const base = item.storage_base || storageRoot;
    const dest = buildDestPathForItem(base, item);
    if (!overrideOnConflict) {
      const exists = await invoke<boolean>("transfer_check_dest", {
        ip,
        destPath: dest
      });
      if (exists) {
        await updateQueueItemStatus(item.id, { Failed: tr("destination_exists") });
        setCurrentQueueItemId(null);
        return;
      }
    }
    await updateQueueItemStatus(item.id, "InProgress");
    setCurrentQueueItemId(item.id);
    setActiveTransferSource(item.source_path);
    setActiveTransferDest(dest);
    setActiveTransferViaQueue(true);
    setTransferStartedAt(Date.now());
    try {
      const runId = await invoke<number>("transfer_start", {
        req: {
          ip,
          source_path: item.source_path,
          dest_path: dest,
          use_temp: useTemp,
          connections,
          resume_mode: resumeMode,
          compression,
          bandwidth_limit_mbps: bandwidthLimit,
          auto_tune_connections: autoTune,
          optimize_upload: optimizeUpload,
          rar_extract_mode: rarExtractMode,
          payload_version: payloadVersion,
          storage_root: base,
          required_size: item.size_bytes || null
        }
      });
      setActiveRunId(runId);
    } catch (err) {
      await updateQueueItemStatus(item.id, { Failed: String(err) });
      setCurrentQueueItemId(null);
    }
  };

  const processNextQueueItem = async () => {
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
    if (currentQueueItemId) return;
    await processNextQueueItem();
  };

  const handleCancel = async () => {
    try {
      await invoke("transfer_cancel");
      setTransferState((prev) => ({ ...prev, status: "Cancelled" }));
      if (currentQueueItemId) {
        updateQueueItemStatus(currentQueueItemId, { Failed: "Cancelled" }).then(
          () => {
            setCurrentQueueItemId(null);
          }
        );
      }
      // Reset transfer state to re-enable UI
      setActiveRunId(null);
      setActiveTransferSource("");
      setActiveTransferDest("");
      setActiveTransferViaQueue(false);
      setTransferStartedAt(null);
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
    setOverrideOnConflict(false);
    setCompression("auto");
    setResumeMode("none");
    setConnections(4);
    setBandwidthLimit(0);
    setOptimizeUpload(false);
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

  const loadManageEntries = async (path: string) => {
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    setManageStatus("Loading...");
    try {
      const entries = await invoke<DirEntry[]>("manage_list", { ip, path });
      const sorted = sortEntries(entries, manageSort);
      setManageEntries(sorted);
      setManageSelected(null);
      setManageMeta(null);
      setManageCoverUrl(null);
      setManageStatus(`Loaded ${sorted.length} items`);
    } catch (err) {
      setManageStatus(`Error: ${String(err)}`);
    }
  };

  const handleManageRefresh = async (pathOverride?: string) => {
    const override = typeof pathOverride === "string" ? pathOverride : undefined;
    const targetPath = override ?? managePath;
    if (override && override !== managePath) {
      manageRefreshSkip.current = targetPath;
      setManagePath(targetPath);
    }
    await loadManageEntries(targetPath);
  };

  const handleManageUp = () => {
    if (managePath === "/") return;
    const parts = managePath.split("/").filter(Boolean);
    parts.pop();
    const next = `/${parts.join("/")}`;
    setManagePath(next || "/");
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
    setManageDestAction(action);
    setManageDestOpen(true);
    const nextPath = storageRoot || managePath || "/data";
    setManageDestPath(nextPath);
    setManageDestSelected(null);
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
    const dstPath = joinRemote(destBase, manageSelectedEntry.name);

    setManageDestOpen(false);
    setManageBusy(true);
    setManageStatus(`${manageDestAction}...`);
    try {
      if (manageDestAction === "Move") {
        await invoke("manage_move", { ip, src_path: srcPath, dst_path: dstPath });
      } else if (manageDestAction === "Copy") {
        await invoke("manage_copy", { ip, src_path: srcPath, dst_path: dstPath });
      } else if (manageDestAction === "Extract") {
        await invoke("manage_extract", { ip, src_path: srcPath, dst_path: dstPath });
      }
    } catch (err) {
      setManageBusy(false);
      setManageStatus(`Error: ${String(err)}`);
    } finally {
      setManageDestAction(null);
    }
  };

  const handleManageCancel = async () => {
    try {
      await invoke("manage_cancel");
      setManageStatus("Cancelling...");
    } catch (err) {
      setClientLogs((prev) => [
        `Failed to cancel: ${String(err)}`,
        ...prev
      ]);
    }
  };

  const handleManageUploadFiles = async () => {
    if (!ip.trim()) {
      setManageStatus("Not connected");
      return;
    }
    const selected = await open({ multiple: true });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    setManageBusy(true);
    setManageStatus("Uploading...");
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
    const selected = await open({ directory: true, multiple: true });
    if (!selected) return;
    const paths = Array.isArray(selected) ? selected : [selected];
    setManageBusy(true);
    setManageStatus("Uploading...");
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
      setManageStatus("Downloading...");
      if (manageSelectedEntry.entry_type === "dir") {
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
        await invoke("manage_download_dir", {
          ip,
          path: srcPath,
      dest_path: finalDestPath,
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

  const status = useMemo(
    () => ({
      connection: connectionStatus,
      payload: payloadStatus,
      transfer: transferState.status,
      storage: storageRoot
    }),
    [connectionStatus, payloadStatus, storageRoot, transferState.status]
  );
  const manageActionLabel = manageDestAction
    ? manageDestAction === "Move"
      ? tr("move")
      : manageDestAction === "Copy"
      ? tr("copy")
      : tr("extract")
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
          className="header-main"
          onDoubleClick={handleToggleMaximize}
          data-tauri-drag-region
        >
          <div className="brand">
            <div className="brand-logo-wrap">
              <img className="brand-logo" src="/logo.png" alt="PS5Upload" />
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
            onClick={() => window.open("https://ko-fi.com/B0B81S0WUA")}
            title={tr("buy_coffee")}
          >
            <span className="icon-coffee">♥</span> {tr("buy_coffee")}
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
          <label className="lang-switch" aria-label={tr("language")}>
            <span>{tr("language")}</span>
            <select
              value={language}
              onChange={(event) => setLanguage(event.target.value)}
            >
              <option value="en">English</option>
              <option value="zh-CN">简体中文</option>
              <option value="zh-TW">繁體中文</option>
              <option value="fr">Français</option>
              <option value="es">Español</option>
              <option value="ar">العربية</option>
            </select>
          </label>
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
              className="btn primary"
              onClick={handleConnect}
              disabled={isConnecting}
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
              className="btn primary"
              onClick={handlePayloadSend}
              disabled={payloadBusy}
            >
              {tr("payload_send")}
            </button>
            <button className="btn" onClick={handlePayloadCheck} disabled={payloadBusy}>
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
          <div className="pill status-pill">
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
                    {loc.path} ({loc.free_gb.toFixed(1)} GB free)
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
                onClick={() => window.open(updateInfo.html_url)}
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
              {currentAssetName && (
                <button
                  className="btn ghost"
                  onClick={() => {
                    const asset = updateInfo.assets.find(
                      (item) => item.name === currentAssetName
                    );
                    if (asset) {
                      handleUpdateDownload(asset);
                    } else {
                      setUpdateDownloadStatus(
                        `Asset not found: ${currentAssetName}`
                      );
                    }
                  }}
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
              <div className="card">
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
                  <button className="btn" onClick={handleScan}>
                    {tr("scan")}
                  </button>
                  <button className="btn" onClick={handleAddToQueue}>
                    {tr("add_to_queue")}
                  </button>
                  <button
                    className={`btn ${optimizeUpload ? "primary" : ""}`}
                    onClick={handleOptimizeToggle}
                  >
                    {tr("optimize")}
                  </button>
                </div>
                <p className="muted small">{tr("scan_help")}</p>
                <p className="muted small">{tr("optimize_help")}</p>
                {optimizeUpload && (
                  <p className="muted small">{tr("optimize_lock_note")}</p>
                )}
                {gameMeta && (
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
                )}
                <label className="field">
                  <span>{tr("bandwidth_limit")}</span>
                  <input
                    type="number"
                    min={0}
                    value={bandwidthLimit}
                    onChange={(event) => setBandwidthLimit(Number(event.target.value))}
                  />
                </label>
                <label className="field inline">
                  <span>{tr("auto_tune")}</span>
                  <input
                    type="checkbox"
                    checked={autoTune}
                    disabled={optimizeUpload}
                    onChange={(event) => setAutoTune(event.target.checked)}
                  />
                </label>
                <p className="muted small">{tr("auto_tune_desc")}</p>
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
                <label className="field">
                  <span>{tr("compression")}</span>
                  <select
                    value={compression}
                    disabled={optimizeUpload}
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
                  <span>{tr("connections")}</span>
                  <input
                    type="number"
                    min={1}
                    max={10}
                    value={connections}
                    disabled={optimizeUpload}
                    onChange={(event) =>
                      setConnections(
                        Math.min(10, Math.max(1, Number(event.target.value)))
                      )
                    }
                  />
                </label>
                <p className="muted small">{tr("connections_note")}</p>
                <p className="muted small">{tr("connections_note_extra")}</p>
                <label className="field">
                  <span>{tr("resume_mode")}</span>
                  <select
                    value={resumeMode}
                    onChange={(event) =>
                      setResumeMode(event.target.value as ResumeOption)
                    }
                  >
                    <option value="none">{tr("resume_off")}</option>
                    <option value="size">{tr("resume_fast")}</option>
                    <option value="size_mtime">{tr("resume_medium")}</option>
                    <option value="sha256">{tr("resume_slow")}</option>
                  </select>
                </label>
                {resumeMode !== "none" && (
                  <>
                    <p className="muted small">{tr("resume_note")}</p>
                    <p className="muted small">{tr("resume_note_change")}</p>
                  </>
                )}
                <label className="field">
                  <span>{tr("rar_extract")}</span>
                  <select
                    value={rarExtractMode}
                    onChange={(event) =>
                      setRarExtractMode(event.target.value as RarExtractMode)
                    }
                  >
                    <option value="normal">{tr("rar_normal")}</option>
                    <option value="safe">{tr("rar_safe")}</option>
                    <option value="turbo">{tr("rar_turbo")}</option>
                  </select>
                </label>
                <p className="muted small">{tr("rar_note")}</p>
                <p className="muted small">
                  {rarExtractMode === "safe"
                    ? tr("rar_note_safe")
                    : rarExtractMode === "turbo"
                    ? tr("rar_note_turbo")
                    : tr("rar_note_normal")}
                </p>
                <label className="field inline">
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
                        onClick={() => setFinalPathMode("auto")}
                      >
                        {tr("use_auto")}
                      </button>
                    )}
                  </div>
                  <p className="muted small">{tr("final_path_note")}</p>
                </label>
                <label className="field inline">
                  <span>{tr("override_conflict")}</span>
                  <input
                    type="checkbox"
                    checked={overrideOnConflict}
                    onChange={(event) => setOverrideOnConflict(event.target.checked)}
                  />
                </label>
              </div>

              <div className="card wide">
                <header className="card-title">
                  <span className="card-title-icon">▶</span>
                  {tr("transfer_control")}
                  {activeRunId && (
                    <span
                      className="status-dot active"
                      title={tr("transfer_in_progress")}
                    />
                  )}
                </header>
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
                  <span>
                    {transferState.files} {tr("files")}
                  </span>
                  <span>{transferSpeed > 0 ? `${formatBytes(transferSpeed)}/s` : "—"}</span>
                  <span>{transferState.status}</span>
                </div>
                {transferState.currentFile && (
                  <div className="pill">{transferState.currentFile}</div>
                )}
                <div className="split">
                  <button
                    className="btn primary"
                    onClick={handleUpload}
                    disabled={!!activeRunId || !sourcePath.trim()}
                  >
                    ↑ {tr("upload")}
                  </button>
                  <button
                    className="btn primary"
                    onClick={handleUploadQueue}
                    disabled={
                      !!activeRunId ||
                      queueData.items.filter((i) => i.status === "Pending").length === 0
                    }
                  >
                    ↑ {tr("upload_queue")} (
                    {queueData.items.filter((i) => i.status === "Pending").length})
                  </button>
                  <button
                    className="btn danger"
                    onClick={handleCancel}
                    disabled={!activeRunId}
                  >
                    {tr("stop")}
                  </button>
                  <button
                    className="btn transfer-reset"
                    onClick={handleResetTransfer}
                    disabled={!!activeRunId}
                  >
                    {tr("reset")}
                  </button>
                </div>
                <p className="muted small" style={{ marginTop: 8 }}>
                  {activeRunId
                    ? tr("transfer_in_progress")
                    : sourcePath.trim()
                    ? tr("ready_to_upload")
                    : tr("select_source")}
                </p>
              </div>

              <div className="card wide">
                <header className="card-title">
                  <span className="card-title-icon">▣</span>
                  {tr("upload_queue")}
                  {queueData.items.length > 0 && (
                    <span className="badge">{queueData.items.length}</span>
                  )}
                </header>
                {queueData.items.length === 0 ? (
                  <p className="muted">{tr("queue_empty")}</p>
                ) : (
                  <div className="stack">
                    {queueData.items.map((item) => {
                      const statusIcon = item.status === "Pending" ? "○"
                        : item.status === "InProgress" ? "●"
                        : item.status === "Completed" ? "✓"
                        : "✗";
                      const statusColor = item.status === "Pending" ? "#888"
                        : item.status === "InProgress" ? "#0066cc"
                        : item.status === "Completed" ? "#64c864"
                        : "#c86464";
                      return (
                        <div className="queue-item" key={item.id}>
                          <div>
                            <strong>
                              <span style={{ color: statusColor, marginRight: 6 }}>{statusIcon}</span>
                              {item.subfolder_name}
                              {item.size_bytes != null && (
                                <span className="muted small" style={{ marginLeft: 8 }}>
                                  {formatBytes(item.size_bytes)}
                                </span>
                              )}
                            </strong>
                            <div className="muted small">
                              {buildDestPathForItem(
                                item.storage_base || storageRoot,
                                item
                              )}
                            </div>
                          </div>
                          <div>
                            {item.status === "Pending" && (
                              <button
                                className="btn ghost small"
                                onClick={() => handleRemoveQueueItem(item.id)}
                              >
                                ×
                              </button>
                            )}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
                <div className="split">
                  <button className="btn" onClick={handleClearCompletedQueue}>
                    {tr("clear_completed")}
                  </button>
                  <button className="btn" onClick={handleClearQueue}>
                    {tr("clear_queue")}
                  </button>
                </div>
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
                  <div className="stack">
                    <div className="stats-row">
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
                    <button
                      className="btn"
                      onClick={handleRefreshPayloadStatus}
                      disabled={payloadStatusLoading}
                    >
                      {payloadStatusLoading ? tr("loading") : tr("refresh")}
                    </button>
                  </div>
                ) : (
                  <div className="stack">
                    <p className="muted">{tr("loading")}...</p>
                    <button
                      className="btn"
                      onClick={handleRefreshPayloadStatus}
                      disabled={payloadStatusLoading}
                    >
                      {tr("refresh")}
                    </button>
                  </div>
                )}
              </div>

              <div className="card wide">
                <header className="card-title">
                  <span className="card-title-icon">≣</span>
                  {tr("extraction_queue")}
                </header>
                {!isConnected ? (
                  <p className="muted">{tr("not_connected")}</p>
                ) : !payloadFullStatus ? (
                  <div className="stack">
                    <p className="muted">{tr("loading")}...</p>
                    <button
                      className="btn"
                      onClick={handleRefreshPayloadStatus}
                      disabled={payloadStatusLoading}
                    >
                      {tr("refresh")}
                    </button>
                  </div>
                ) : payloadFullStatus.items.length === 0 ? (
                  <div className="stack">
                    <p className="muted">{tr("extraction_queue_empty")}</p>
                    <button
                      className="btn"
                      onClick={handleRefreshPayloadStatus}
                      disabled={payloadStatusLoading}
                    >
                      {tr("refresh")}
                    </button>
                  </div>
                ) : (
                  <div className="stack">
                    {payloadFullStatus.items.map((item) => (
                      <div
                        key={item.id}
                        className={`queue-item ${item.status === "running" ? "active" : ""} ${item.status === "complete" ? "completed" : ""} ${item.status === "failed" ? "failed" : ""}`}
                      >
                        <div className="queue-item-header">
                          <strong>{item.archive_name}</strong>
                          <span className={`chip ${item.status === "running" ? "warn" : item.status === "complete" ? "ok" : item.status === "failed" ? "error" : ""}`}>
                            {item.status === "running" ? tr("extracting") :
                             item.status === "complete" ? tr("complete") :
                             item.status === "failed" ? tr("failed") :
                             tr("pending")}
                          </span>
                        </div>
                        {item.status === "running" && (
                          <div className="progress-info">
                            <div className="progress-bar">
                              <div
                                className="progress-fill"
                                style={{ width: `${item.percent}%` }}
                              />
                            </div>
                            <div className="muted small">
                              {item.percent}% - {item.files_extracted} {tr("files")} - {formatBytes(item.processed_bytes)} / {formatBytes(item.total_bytes)}
                            </div>
                          </div>
                        )}
                        {item.status === "complete" && (
                          <div className="muted small">
                            {item.files_extracted} {tr("files")} - {formatBytes(item.total_bytes)}
                          </div>
                        )}
                        {item.status === "failed" && item.error && (
                          <div className="small" style={{ color: "#c86464" }}>
                            {tr("error")}: {item.error}
                          </div>
                        )}
                        {(item.status === "pending" || item.status === "running") && (
                          <button
                            className="btn ghost small"
                            onClick={() => handleQueueCancel(item.id)}
                          >
                            {tr("cancel")}
                          </button>
                        )}
                      </div>
                    ))}
                    <div className="split">
                      <button className="btn" onClick={handleQueueClear}>
                        {tr("clear_completed")}
                      </button>
                    </div>
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
                {manageStatus && <p className="muted small">{manageStatus}</p>}
                <div className="file-list">
                  <div className="file-item header">
                    <span
                      role="button"
                      className={`file-header ${manageSort.key === "name" ? "active" : ""}`}
                      onClick={() =>
                        setManageSort((prev) => ({
                          key: "name",
                          direction: prev.key === "name" && prev.direction === "asc" ? "desc" : "asc"
                        }))
                      }
                    >
                      {tr("name")}
                      {manageSort.key === "name" && (manageSort.direction === "asc" ? " ↑" : " ↓")}
                    </span>
                    <span className="file-header">
                      {tr("type")}
                    </span>
                    <span
                      role="button"
                      className={`file-header meta ${manageSort.key === "size" ? "active" : ""}`}
                      onClick={() =>
                        setManageSort((prev) => ({
                          key: "size",
                          direction: prev.key === "size" && prev.direction === "desc" ? "asc" : "desc"
                        }))
                      }
                    >
                      {tr("size")}
                      {manageSort.key === "size" && (manageSort.direction === "asc" ? " ↑" : " ↓")}
                    </span>
                    <span
                      role="button"
                      className={`file-header meta ${manageSort.key === "modified" ? "active" : ""}`}
                      onClick={() =>
                        setManageSort((prev) => ({
                          key: "modified",
                          direction: prev.key === "modified" && prev.direction === "desc" ? "asc" : "desc"
                        }))
                      }
                    >
                      {tr("modified")}
                      {manageSort.key === "modified" && (manageSort.direction === "asc" ? " ↑" : " ↓")}
                    </span>
                  </div>
                  {manageEntries.length === 0 ? (
                    <div className="empty-state">
                      <span className="empty-state-icon">○</span>
                      <span className="empty-state-text">{tr("no_files")}</span>
                    </div>
                  ) : (
                    manageEntries.map((entry, index) => {
                      if (!entry) return null;
                      const entryTypeRaw = typeof entry.entry_type === "string"
                        ? entry.entry_type
                        : typeof (entry as { type?: unknown }).type === "string"
                        ? String((entry as { type?: unknown }).type)
                        : "";
                      const entryType = entryTypeRaw.toLowerCase();
                      const isDir =
                        entryType === "dir" ||
                        entryType === "folder" ||
                        entryType === "directory";
                      const nameRaw = typeof entry.name === "string" ? entry.name : "";
                      const lowerName = nameRaw.toLowerCase();
                      const baseIcon = isDir ? "▸" :
                        lowerName.endsWith(".rar") || lowerName.endsWith(".zip") ? "▣" :
                        lowerName.endsWith(".pkg") ? "●" :
                        lowerName.endsWith(".json") || lowerName.endsWith(".txt") ? "≡" :
                        lowerName.endsWith(".png") || lowerName.endsWith(".jpg") ? "◧" :
                        "○";
                      const isSelected = manageSelected === index;
                      const icon = isSelected ? "●" : baseIcon;
                      return (
                        <div
                          className={`file-item ${isSelected ? "selected" : ""}`}
                          key={`${nameRaw}-${entryTypeRaw}-${index}`}
                          onClick={() => {
                            setManageSelected(index);
                            if (!isDir) {
                              manageClickRef.current = { index, time: Date.now() };
                              return;
                            }
                            const now = Date.now();
                            const last = manageClickRef.current;
                            if (last.index === index && now - last.time < 350) {
                              const nextPath = joinRemote(managePath, nameRaw);
                              handleManageRefresh(nextPath);
                              manageClickRef.current = { index: null, time: 0 };
                              return;
                            }
                            manageClickRef.current = { index, time: now };
                          }}
                        >
                          <span className="file-name">
                            <span className={`file-icon ${isSelected ? "active" : ""}`}>
                              {icon}
                            </span>
                            {nameRaw}
                          </span>
                          <span className="file-meta type">
                            {isDir ? tr("folder") : tr("file")}
                          </span>
                          <span className="file-meta">
                            {isDir ? "—" : formatBytes(entry.size)}
                          </span>
                          <span className="file-meta">{formatTimestamp(entry.mtime)}</span>
                        </div>
                      );
                    })
                  )}
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
                        {manageSelectedEntry.entry_type === "dir" ? "▸" : "○"}
                      </span>
                      <div className="selected-item-info">
                        <div className="selected-item-name">{manageSelectedEntry.name}</div>
                        <div className="selected-item-type">
                          {manageSelectedEntry.entry_type === "dir"
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

                  <div className="action-group">
                    <div className="action-group-title">↑ {tr("upload_title")}</div>
                    <div className="action-buttons">
                      <button
                        className="btn"
                        onClick={handleManageUploadFiles}
                        disabled={manageBusy}
                      >
                        {tr("files")}
                      </button>
                      <button
                        className="btn"
                        onClick={handleManageUploadFolder}
                        disabled={manageBusy}
                      >
                        {tr("folder")}
                      </button>
                    </div>
                  </div>

                  <div className="action-group">
                    <div className="action-group-title">↓ {tr("transfer")}</div>
                    <div className="action-buttons">
                      <button
                        className="btn primary"
                        onClick={handleManageDownload}
                        disabled={!manageSelectedEntry || manageBusy}
                      >
                        ↓ {tr("download")}
                      </button>
                      <button
                        className="btn"
                        onClick={() => handleOpenDestPicker("Move")}
                        disabled={!manageSelectedEntry || manageBusy}
                      >
                        → {tr("move")}
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
                        disabled={!manageSelectedEntry || manageBusy}
                      >
                        {tr("extract")}
                      </button>
                    </div>
                    <p className="muted small">{tr("extract_note")}</p>
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

                  {manageProgress.op && (
                    <div className="stack" style={{ marginTop: "8px" }}>
                      <div className="progress">
                        <div
                          className="progress-fill"
                          style={{
                            width:
                              manageProgress.total > 0
                                ? `${Math.min(
                                    100,
                                    (manageProgress.processed /
                                      manageProgress.total) *
                                      100
                                  )}%`
                                : "0%"
                          }}
                        />
                      </div>
                  <div className="progress-meta">
                    <span>
                      {manageProgress.op}{" "}
                      {formatBytes(manageProgress.processed)} /{" "}
                      {formatBytes(manageProgress.total)}
                    </span>
                    <span>
                      {manageProgress.speed_bps > 0
                        ? `${formatBytes(manageProgress.speed_bps)}/s`
                        : "—"}
                    </span>
                    <span>
                      {manageProgress.total > 0
                        ? `${Math.round(
                            (manageProgress.processed / manageProgress.total) *
                                  100
                              )}%`
                            : "0%"}
                        </span>
                      </div>
                      {manageProgress.currentFile && (
                        <div className="pill">{manageProgress.currentFile}</div>
                      )}
                      <button className="btn danger" onClick={handleManageCancel}>
                        {tr("stop")}
                      </button>
                    </div>
                  )}

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
              <div className="card wide chat-card">
                <header className="card-title">
                  <span className="card-title-icon">◇</span>
                  {tr("community")}
                </header>
                <div className="chip-row">
                  <span
                    className={`chip chat-status ${
                      isChatConnected ? "ok" : isChatDisconnected ? "warn" : ""
                    }`}
                  >
                    {chatStatus}
                  </span>
                </div>
                <div className="stats-row">
                  <span className="pill">
                    {tr("sent")}: {chatStats.sent}
                  </span>
                  <span className="pill">
                    {tr("received")}: {chatStats.received}
                  </span>
                  <span className="pill">
                    {tr("acked")}: {chatStats.acked}
                  </span>
                  <span className="pill">
                    {tr("rejected")}: {chatStats.rejected}
                  </span>
                </div>
                <label className="field">
                  <span>{tr("display_name")}</span>
                  <input
                    value={chatDisplayName}
                    onChange={(event) => setChatDisplayName(event.target.value)}
                    placeholder={tr("display_name")}
                  />
                </label>
                <div className="chat-window">
                  {chatMessages.length === 0 ? (
                    <p className="muted">{tr("no_messages")}</p>
                  ) : (
                    chatMessages.map((msg, index) => (
                      <div
                        key={`${msg.time}-${msg.sender}-${index}`}
                        className={`chat-line ${msg.local ? "local" : ""}`}
                      >
                        <span className="chat-time">[{msg.time}]</span>{" "}
                        <strong>{msg.local ? tr("you") : msg.sender}:</strong>{" "}
                        {msg.text}
                      </div>
                    ))
                  )}
                  <div ref={chatEndRef} />
                </div>
                <div className="inline-field chat-input">
                  <input
                    placeholder={tr("type_message")}
                    value={chatInput}
                    onChange={(event) => setChatInput(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") {
                        handleChatSend();
                      }
                    }}
                  />
                  <button className="btn primary" onClick={handleChatSend}>
                    {tr("send")}
                  </button>
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
                  invoke("history_clear").then(() =>
                    setHistoryData({ records: [] })
                  );
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
                      <div>
                        <strong style={{ color: record.success ? "#64c864" : "#c86464" }}>
                          {record.success ? "✓" : "✗"}{" "}
                          {record.source_path.split(/[/\\]/).filter(Boolean).pop()}
                        </strong>
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
                      <div className="stack-sm">
                        <span>
                          {record.file_count} {tr("files")}
                        </span>
                        <span>
                          {formatBytes(record.total_bytes)} · {formatBytes(record.speed_bps)}/s
                        </span>
                        <span>{formatDuration(record.duration_secs)}</span>
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
              <button
                className="btn"
                onClick={() => {
                  if (logTab === "client") {
                    setClientLogs([]);
                  } else {
                    setPayloadLogs([]);
                  }
                }}
              >
                {tr("clear_logs")}
              </button>
              <div className="log-window">
                {(logTab === "client" ? clientLogs : payloadLogs).length === 0 ? (
                  <p>{tr("no_logs")}</p>
                ) : (
                  (logTab === "client" ? clientLogs : payloadLogs)
                    .slice(0, 50)
                    .map((entry, index) => <p key={`${index}`}>{entry}</p>)
                )}
              </div>
            </>
          )}
        </section>
      </aside>

      <footer className="status-bar shell">
        <span>{status.transfer}</span>
        <span>
          <a href="https://x.com/phantomptr">Created by PhantomPtr</a> |{" "}
          <a href="https://github.com/phantomptr/ps5upload">Source Code</a>
        </span>
      </footer>

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
                  <span>{entry.entry_type}</span>
                  <span>-</span>
                  <span>{formatTimestamp(entry.mtime)}</span>
                </div>
              ))}
            </div>
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
    </div>
  );
}
