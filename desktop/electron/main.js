const { app, BrowserWindow, ipcMain, dialog } = require('electron');
const path = require('path');
const fs = require('fs');
const os = require('os');
const net = require('net');
const https = require('https');
const http = require('http');
const crypto = require('crypto');
const { promisify } = require('util');
const { pipeline } = require('stream');
const streamPipeline = promisify(pipeline);
const tryRequire = (moduleName) => {
  try {
    return require(moduleName);
  } catch {
    return null;
  }
};
const lz4 = tryRequire('lz4');
const fzstd = tryRequire('fzstd');
const lzma = tryRequire('lzma-native');

// Constants
const TRANSFER_PORT = 9113;
const PAYLOAD_PORT = 9021;
const CONNECTION_TIMEOUT_MS = 30000;
const READ_TIMEOUT_MS = 120000;
const PACK_BUFFER_SIZE = 16 * 1024 * 1024; // 16MB
const SEND_CHUNK_SIZE = 4 * 1024 * 1024; // 4MB
const MAGIC_FTX1 = 0x31585446;
const VERSION = '1.2.2';

const FrameType = {
  Pack: 4,
  PackLz4: 8,
  PackZstd: 9,
  PackLzma: 10,
  Finish: 6,
};


// Paths
const getAppDataDir = () => {
  const homeDir = os.homedir();
  return path.join(homeDir, '.ps5upload');
};

const ensureDir = (dir) => {
  if (!fs.existsSync(dir)) {
    fs.mkdirSync(dir, { recursive: true });
  }
};

const getConfigPath = () => path.join(getAppDataDir(), 'ps5upload.ini');
const getHistoryPath = () => path.join(getAppDataDir(), 'ps5upload_history.json');
const getQueuePath = () => path.join(getAppDataDir(), 'ps5upload_queue.json');
const getProfilesPath = () => path.join(getAppDataDir(), 'ps5upload_profiles.ini');
const getLogsDir = () => path.join(getAppDataDir(), 'logs');

// State
let mainWindow = null;
const state = {
  transferCancel: false,
  transferActive: false,
  transferRunId: 0,
  transferStatus: { run_id: 0, status: 'Idle', sent: 0, total: 0, files: 0, elapsed_secs: 0, current_file: '' },
  payloadPollEnabled: false,
  payloadIp: '',
  payloadAutoReloadEnabled: false,
  payloadAutoReloadMode: 'current',
  payloadAutoReloadPath: '',
  payloadStatus: null,
  connectionPollEnabled: false,
  connectionAutoEnabled: false,
  connectionIp: '',
  connectionStatus: { is_connected: false, status: 'Disconnected', storage_locations: [] },
  managePollEnabled: false,
  manageIp: '',
  managePath: '/data',
  manageActive: false,
  manageCancel: false,
  manageListCache: { path: '', entries: [], error: null, updated_at_ms: 0 },
  saveLogs: false,
  uiLogEnabled: true,
  chatSender: null,
};

// Pollers
let payloadPoller = null;
let connectionPoller = null;
let managePoller = null;
let payloadAutoReloader = null;

// Determine the environment
const isDev = process.env.NODE_ENV !== 'production' || !app.isPackaged;

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1440,
    height: 960,
    frame: false,
    transparent: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  if (isDev) {
    mainWindow.loadURL('http://localhost:1420');
    mainWindow.webContents.openDevTools();
  } else {
    mainWindow.loadFile(path.join(__dirname, '../dist/index.html'));
  }

  mainWindow.on('closed', () => {
    mainWindow = null;
  });
}

// Emit event to renderer
function emit(channel, data) {
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.webContents.send(channel, data);
  }
}

// Log writing
function writeLogLine(category, message) {
  if (!state.saveLogs) return;
  try {
    const logsDir = getLogsDir();
    ensureDir(logsDir);
    const date = new Date().toISOString().split('T')[0];
    const logFile = path.join(logsDir, `${category}_${date}.log`);
    const timestamp = new Date().toISOString();
    fs.appendFileSync(logFile, `[${timestamp}] ${message}\n`);
  } catch (e) {
    // Ignore log errors
  }
}

// Rate Limiter Class
class RateLimiter {
  constructor(limitBps) {
    this.limitBps = limitBps; // bytes per second
    this.lastByteTime = process.hrtime.bigint();
    this.accruedDelay = 0n; // nanoseconds
  }

  throttle(bytes) {
    if (!this.limitBps || this.limitBps <= 0) {
      return;
    }

    const now = process.hrtime.bigint();
    const elapsed = now - this.lastByteTime;
    this.lastByteTime = now;

    this.accruedDelay -= elapsed;

    const delayNeeded = (BigInt(bytes) * 1_000_000_000n) / BigInt(this.limitBps);
    this.accruedDelay += delayNeeded;

    if (this.accruedDelay > 0) {
      // Convert nanoseconds to milliseconds, at least 1ms to avoid busy-waiting
      const sleepMs = Number(this.accruedDelay / 1_000_000n);
      if (sleepMs > 0) {
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, sleepMs);
      }
      this.accruedDelay = 0n; // Reset accrued delay after sleeping
    }
  }
}

// Robust socket write
function writeAllRetry(socket, data, cancel) {
  return new Promise((resolve, reject) => {
    let offset = 0;
    const writeLoop = () => {
      if (cancel.value) {
        socket.destroy();
        return reject(new Error('Upload cancelled by user'));
      }

      const bytesWritten = socket.write(data.slice(offset));
      if (bytesWritten === false) { // Buffer full, wait for 'drain'
        socket.once('drain', writeLoop);
      } else {
        offset += bytesWritten;
        if (offset < data.length) {
          setImmediate(writeLoop); // Continue writing remaining data
        } else {
          resolve(); // All data written
        }
      }
    };
    writeLoop();
  });
}

// ==================== TCP Protocol ====================

function createSocketWithTimeout(ip, port, timeout = CONNECTION_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    const socket = new net.Socket();
    socket.setTimeout(timeout);

    socket.on('timeout', () => {
      socket.destroy();
      reject(new Error('Connection timed out'));
    });

    socket.on('error', (err) => {
      reject(err);
    });

    socket.connect(port, ip, () => {
      socket.setTimeout(0);
      resolve(socket);
    });
  });
}

async function sendSimpleCommand(ip, port, cmd) {
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      socket.destroy();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      data = Buffer.concat([data, chunk]);
      if (data.includes(Buffer.from('\n'))) {
        socket.destroy();
        resolve(data.toString('utf8').trim());
      }
    });

    socket.on('error', (err) => reject(err));
    socket.on('close', () => {
      if (data.length > 0) {
        resolve(data.toString('utf8').trim());
      }
    });

    socket.write(cmd);
  });
}

async function listStorage(ip, port) {
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      socket.destroy();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      data = Buffer.concat([data, chunk]);
      // Check for end marker "\n]\n"
      const str = data.toString('utf8');
      if (str.includes('\n]\n') || str.endsWith('\n]')) {
        socket.destroy();
        try {
          const jsonEnd = str.lastIndexOf(']');
          const jsonStr = str.substring(0, jsonEnd + 1);
          const locations = JSON.parse(jsonStr);
          resolve(locations);
        } catch (e) {
          reject(new Error('Invalid JSON response'));
        }
      }
    });

    socket.on('error', (err) => reject(err));
    socket.on('close', () => {
      if (data.length > 0) {
        try {
          const str = data.toString('utf8');
          const jsonEnd = str.lastIndexOf(']');
          const jsonStr = str.substring(0, jsonEnd + 1);
          resolve(JSON.parse(jsonStr));
        } catch (e) {
          reject(new Error('Invalid JSON response'));
        }
      }
    });

    socket.write('LIST_STORAGE\n');
  });
}

async function listDir(ip, port, dirPath) {
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      socket.destroy();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      data = Buffer.concat([data, chunk]);
      const str = data.toString('utf8');
      if (str.includes('\n]\n') || str.endsWith('\n]')) {
        socket.destroy();
        try {
          const jsonEnd = str.lastIndexOf(']');
          const jsonStr = str.substring(0, jsonEnd + 1);
          const entries = JSON.parse(jsonStr);
          resolve(entries);
        } catch (e) {
          reject(new Error('Invalid JSON response'));
        }
      }
    });

    socket.on('error', (err) => reject(err));
    socket.on('close', () => {
      if (data.length > 0) {
        try {
          const str = data.toString('utf8');
          const jsonEnd = str.lastIndexOf(']');
          const jsonStr = str.substring(0, jsonEnd + 1);
          resolve(JSON.parse(jsonStr));
        } catch (e) {
          reject(new Error('Invalid JSON response'));
        }
      }
    });

    socket.write(`LIST_DIR ${dirPath}\n`);
  });
}

async function checkDir(ip, port, dirPath) {
  try {
    const response = await sendSimpleCommand(ip, port, `CHECK_DIR ${dirPath}\n`);
    return response === 'EXISTS';
  } catch {
    return false;
  }
}

async function deletePath(ip, port, filePath) {
  const response = await sendSimpleCommand(ip, port, `DELETE ${filePath}\n`);
  if (!response.startsWith('OK')) {
    throw new Error(`Delete failed: ${response}`);
  }
}

async function movePath(ip, port, src, dst) {
  const response = await sendSimpleCommand(ip, port, `MOVE ${src}\t${dst}\n`);
  if (!response.startsWith('OK')) {
    throw new Error(`Move failed: ${response}`);
  }
}

async function createPath(ip, port, dirPath) {
  const response = await sendSimpleCommand(ip, port, `CREATE_PATH ${dirPath}\n`);
  if (!response.startsWith('SUCCESS')) {
    throw new Error(`Create folder failed: ${response}`);
  }
}

async function chmod777(ip, port, filePath) {
  const response = await sendSimpleCommand(ip, port, `CHMOD777 ${filePath}\n`);
  if (!response.startsWith('OK')) {
    throw new Error(`Chmod failed: ${response}`);
  }
}

async function getPayloadVersion(ip, port) {
  const response = await sendSimpleCommand(ip, port, 'VERSION\n');
  if (response.startsWith('VERSION ')) {
    return response.substring(8).trim();
  }
  throw new Error(`Unexpected response: ${response}`);
}

async function getPayloadStatus(ip, port) {
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let headerParsed = false;
    let jsonSize = 0;

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      socket.destroy();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      data = Buffer.concat([data, chunk]);

      if (!headerParsed) {
        const str = data.toString('utf8');
        const newlineIdx = str.indexOf('\n');
        if (newlineIdx !== -1) {
          const header = str.substring(0, newlineIdx).trim();
          if (header.startsWith('ERROR')) {
            socket.destroy();
            reject(new Error(`Payload status error: ${header}`));
            return;
          }
          if (!header.startsWith('STATUS ')) {
            socket.destroy();
            reject(new Error(`Unexpected response: ${header}`));
            return;
          }
          jsonSize = parseInt(header.substring(7).trim(), 10);
          headerParsed = true;
          data = data.slice(newlineIdx + 1);
        }
      }

      if (headerParsed && data.length >= jsonSize) {
        socket.destroy();
        try {
          const jsonBuf = data.slice(0, jsonSize);
          const status = JSON.parse(jsonBuf.toString('utf8'));
          resolve(status);
        } catch (e) {
          reject(new Error('Invalid JSON response'));
        }
      }
    });

    socket.on('error', (err) => reject(err));

    socket.write('PAYLOAD_STATUS\n');
  });
}

async function queueExtract(ip, port, src, dst) {
  const response = await sendSimpleCommand(ip, port, `QUEUE_EXTRACT ${src}\t${dst}\n`);
  if (response.startsWith('OK ')) {
    return parseInt(response.substring(3).trim(), 10);
  }
  throw new Error(`Queue extract failed: ${response}`);
}

async function queueCancel(ip, port, id) {
  const response = await sendSimpleCommand(ip, port, `QUEUE_CANCEL ${id}\n`);
  if (!response.startsWith('OK')) {
    throw new Error(`Queue cancel failed: ${response}`);
  }
}

async function queueClear(ip, port) {
  const response = await sendSimpleCommand(ip, port, 'QUEUE_CLEAR\n');
  if (!response.startsWith('OK')) {
    throw new Error(`Queue clear failed: ${response}`);
  }
}

async function uploadV2Init(ip, port, destPath, useTemp) {
  const socket = await createSocketWithTimeout(ip, port);
  const mode = useTemp ? 'TEMP' : 'DIRECT';

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      socket.destroy();
      reject(new Error('Read timed out'));
    });

    const onData = (chunk) => {
      data = Buffer.concat([data, chunk]);
      const response = data.toString('utf8').trim();
      if (response === 'READY') {
        socket.removeListener('data', onData);
        socket.setTimeout(0);
        resolve(socket);
      } else if (response.length > 0 && !response.startsWith('READY')) {
        socket.destroy();
        reject(new Error(`Server rejected V2 upload: ${response}`));
      }
    };

    socket.on('data', onData);
    socket.on('error', (err) => reject(err));

    socket.write(`UPLOAD_V2 ${destPath} ${mode}\n`);
  });
}

async function getSpace(ip, port, path) {
  const response = await sendSimpleCommand(ip, port, `GET_SPACE ${path}\n`);
  if (response.startsWith('OK ')) {
    const parts = response.split(/\s+/);
    if (parts.length >= 3) {
      return { free: BigInt(parts[1]), total: BigInt(parts[2]) };
    }
  }
  throw new Error(`Failed to get space: ${response}`);
}

// ==================== Config Management ====================

const defaultConfig = {
  address: '192.168.0.100',
  storage: '/data',
  connections: 1,
  use_temp: false,
  auto_connect: false,
  theme: 'dark',
  compression: 'none',
  bandwidth_limit_mbps: 0,
  update_channel: 'stable',
  download_compression: 'none',
  chmod_after_upload: false,
  override_on_conflict: false,
  resume_mode: 'none',
  language: 'en',
  auto_tune_connections: true,
  auto_check_payload: false,
  payload_auto_reload: false,
  payload_reload_mode: 'current',
  payload_local_path: '',
  optimize_upload: false,
  chat_display_name: '',
  rar_extract_mode: 'turbo',
  window_width: 1440,
  window_height: 960,
  window_x: -1,
  window_y: -1,
};

function loadConfig() {
  const configPath = getConfigPath();
  if (!fs.existsSync(configPath)) {
    return { ...defaultConfig };
  }

  try {
    const content = fs.readFileSync(configPath, 'utf8');
    const config = { ...defaultConfig };

    for (const line of content.split('\n')) {
      const idx = line.indexOf('=');
      if (idx === -1) continue;
      const key = line.substring(0, idx).trim();
      const value = line.substring(idx + 1).trim();

      switch (key) {
        case 'address': config.address = value; break;
        case 'storage': config.storage = value; break;
        case 'connections': config.connections = Math.max(1, parseInt(value, 10) || 1); break;
        case 'use_temp': config.use_temp = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
        case 'auto_connect': config.auto_connect = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
        case 'theme': config.theme = value === 'light' ? 'light' : 'dark'; break;
        case 'compression': config.compression = ['lz4', 'zstd', 'lzma', 'auto'].includes(value) ? value : 'none'; break;
        case 'bandwidth_limit_mbps': config.bandwidth_limit_mbps = Math.max(0, parseFloat(value) || 0); break;
        case 'update_channel': config.update_channel = value === 'all' ? 'all' : 'stable'; break;
        case 'download_compression': config.download_compression = ['lz4', 'zstd', 'lzma', 'auto'].includes(value) ? value : 'none'; break;
        case 'chmod_after_upload': config.chmod_after_upload = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
        case 'override_on_conflict': config.override_on_conflict = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
        case 'resume_mode': config.resume_mode = ['size', 'size_mtime', 'sha256'].includes(value) ? value : 'none'; break;
        case 'language': config.language = ['zh-CN', 'zh-TW', 'fr', 'es', 'ar'].includes(value) ? value : 'en'; break;
        case 'auto_tune_connections': config.auto_tune_connections = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
        case 'auto_check_payload': config.auto_check_payload = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
        case 'payload_auto_reload': config.payload_auto_reload = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
        case 'payload_reload_mode': config.payload_reload_mode = ['local', 'current', 'latest'].includes(value) ? value : 'current'; break;
        case 'payload_local_path': config.payload_local_path = value; break;
        case 'optimize_upload': config.optimize_upload = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
        case 'chat_display_name': config.chat_display_name = value; break;
        case 'rar_extract_mode': config.rar_extract_mode = ['normal', 'safe', 'turbo'].includes(value) ? value : 'turbo'; break;
        case 'window_width': config.window_width = parseInt(value, 10) || 1440; break;
        case 'window_height': config.window_height = parseInt(value, 10) || 960; break;
        case 'window_x': config.window_x = parseInt(value, 10) || -1; break;
        case 'window_y': config.window_y = parseInt(value, 10) || -1; break;
      }
    }

    if (config.auto_check_payload && !config.payload_auto_reload) {
      config.payload_auto_reload = true;
    }

    return config;
  } catch (e) {
    return { ...defaultConfig };
  }
}

function saveConfig(config) {
  ensureDir(getAppDataDir());
  const lines = [
    `address=${config.address}`,
    `storage=${config.storage}`,
    `connections=${config.connections}`,
    `use_temp=${config.use_temp}`,
    `auto_connect=${config.auto_connect}`,
    `theme=${config.theme}`,
    `compression=${config.compression}`,
    `bandwidth_limit_mbps=${config.bandwidth_limit_mbps}`,
    `update_channel=${config.update_channel}`,
    `download_compression=${config.download_compression}`,
    `chmod_after_upload=${config.chmod_after_upload}`,
    `override_on_conflict=${config.override_on_conflict}`,
    `resume_mode=${config.resume_mode}`,
    `language=${config.language}`,
    `auto_tune_connections=${config.auto_tune_connections}`,
    `auto_check_payload=${config.auto_check_payload}`,
    `payload_auto_reload=${config.payload_auto_reload}`,
    `payload_reload_mode=${config.payload_reload_mode}`,
    `payload_local_path=${config.payload_local_path}`,
    `optimize_upload=${config.optimize_upload}`,
    `chat_display_name=${config.chat_display_name}`,
    `rar_extract_mode=${config.rar_extract_mode}`,
    `window_width=${config.window_width || 1440}`,
    `window_height=${config.window_height || 960}`,
    `window_x=${config.window_x || -1}`,
    `window_y=${config.window_y || -1}`,
  ];
  fs.writeFileSync(getConfigPath(), lines.join('\n') + '\n');
}

// ==================== History Management ====================

function loadHistory() {
  const historyPath = getHistoryPath();
  if (!fs.existsSync(historyPath)) {
    return { records: [] };
  }
  try {
    return JSON.parse(fs.readFileSync(historyPath, 'utf8'));
  } catch {
    return { records: [] };
  }
}

function saveHistory(data) {
  ensureDir(getAppDataDir());
  fs.writeFileSync(getHistoryPath(), JSON.stringify(data, null, 2));
}

function addHistoryRecord(record) {
  const data = loadHistory();
  data.records.push(record);
  saveHistory(data);
}

function clearHistory() {
  saveHistory({ records: [] });
}

// ==================== Queue Management ====================

function loadQueue() {
  const queuePath = getQueuePath();
  if (!fs.existsSync(queuePath)) {
    return { items: [], next_id: 1 };
  }
  try {
    return JSON.parse(fs.readFileSync(queuePath, 'utf8'));
  } catch {
    return { items: [], next_id: 1 };
  }
}

function saveQueue(data) {
  ensureDir(getAppDataDir());
  fs.writeFileSync(getQueuePath(), JSON.stringify(data, null, 2));
}

// ==================== Profiles Management ====================

function loadProfiles() {
  const profilesPath = getProfilesPath();
  if (!fs.existsSync(profilesPath)) {
    return { profiles: [], default_profile: null };
  }

  try {
    const content = fs.readFileSync(profilesPath, 'utf8');
    const profiles = [];
    let currentProfile = null;
    let defaultProfile = null;

    for (const line of content.split('\n')) {
      const trimmed = line.trim();
      if (trimmed.startsWith('[') && trimmed.endsWith(']')) {
        if (currentProfile) profiles.push(currentProfile);
        const name = trimmed.slice(1, -1);
        currentProfile = {
          name,
          address: '',
          storage: '',
          preset_index: 0,
          custom_preset_path: '',
          connections: 1,
          use_temp: false,
          auto_tune_connections: true,
          chat_display_name: '',
        };
      } else if (currentProfile && trimmed.includes('=')) {
        const idx = trimmed.indexOf('=');
        const key = trimmed.substring(0, idx).trim();
        const value = trimmed.substring(idx + 1).trim();
        switch (key) {
          case 'address': currentProfile.address = value; break;
          case 'storage': currentProfile.storage = value; break;
          case 'preset_index': currentProfile.preset_index = parseInt(value, 10) || 0; break;
          case 'custom_preset_path': currentProfile.custom_preset_path = value; break;
          case 'connections': currentProfile.connections = parseInt(value, 10) || 1; break;
          case 'use_temp': currentProfile.use_temp = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
          case 'auto_tune_connections': currentProfile.auto_tune_connections = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
          case 'chat_display_name': currentProfile.chat_display_name = value; break;
          case 'default': if (value === 'true') defaultProfile = currentProfile.name; break;
        }
      }
    }
    if (currentProfile) profiles.push(currentProfile);

    return { profiles, default_profile: defaultProfile };
  } catch {
    return { profiles: [], default_profile: null };
  }
}

function saveProfiles(data) {
  ensureDir(getAppDataDir());
  const lines = [];
  for (const profile of data.profiles) {
    lines.push(`[${profile.name}]`);
    lines.push(`address=${profile.address}`);
    lines.push(`storage=${profile.storage}`);
    lines.push(`preset_index=${profile.preset_index}`);
    lines.push(`custom_preset_path=${profile.custom_preset_path}`);
    lines.push(`connections=${profile.connections}`);
    lines.push(`use_temp=${profile.use_temp}`);
    lines.push(`auto_tune_connections=${profile.auto_tune_connections}`);
    lines.push(`chat_display_name=${profile.chat_display_name}`);
    if (data.default_profile === profile.name) {
      lines.push('default=true');
    }
    lines.push('');
  }
  fs.writeFileSync(getProfilesPath(), lines.join('\n'));
}

// ==================== File Collection ====================

function collectFiles(basePath, cancel = { value: false }, progressCallback = null) {
  const files = [];
  let totalSize = 0n;

  const stat = fs.statSync(basePath);
  if (stat.isFile()) {
    files.push({
      rel_path: path.basename(basePath),
      abs_path: basePath,
      size: Number(stat.size),
      mtime: Math.floor(stat.mtimeMs / 1000),
    });
    if (progressCallback) progressCallback(1, Number(stat.size));
    return { files, cancelled: false };
  }

  const walk = (dir, prefix = '') => {
    if (cancel.value) return;
    const entries = fs.readdirSync(dir, { withFileTypes: true });
    for (const entry of entries) {
      if (cancel.value) return;
      const fullPath = path.join(dir, entry.name);
      const relPath = prefix ? `${prefix}/${entry.name}` : entry.name;

      if (entry.isDirectory()) {
        walk(fullPath, relPath);
      } else if (entry.isFile()) {
        const stat = fs.statSync(fullPath);
        files.push({
          rel_path: relPath.replace(/\\/g, '/'),
          abs_path: fullPath,
          size: Number(stat.size),
          mtime: Math.floor(stat.mtimeMs / 1000),
        });
        totalSize += BigInt(stat.size);

        if (progressCallback && files.length % 1000 === 0) {
          progressCallback(files.length, Number(totalSize));
        }
      }
    }
  };

  walk(basePath);
  if (progressCallback) progressCallback(files.length, Number(totalSize));

  return { files, cancelled: cancel.value };
}

// ==================== Transfer ====================

async function sendFrameHeader(socket, frameType, length, cancel) {
  const header = Buffer.alloc(16);
  header.writeUInt32LE(MAGIC_FTX1, 0);
  header.writeUInt32LE(frameType, 4);
  header.writeBigUInt64LE(BigInt(length), 8);
  await writeAllRetry(socket, header, cancel);
}

async function sendFilesV2(files, socket, options = {}) {
  const { cancel = { value: false }, progress = () => {}, log = () => {}, compression = 'none', rateLimitBps = null } = options;

  let totalSentBytes = 0n;
  let totalSentFiles = 0;
  const startTime = Date.now();
  let limiter = new RateLimiter(rateLimitBps);

  let packBuffer = Buffer.alloc(4); // Record count placeholder
  let packBytesAdded = 0n;
  let packFilesAdded = 0;
  let recordCount = 0;

  const flushPack = async (lastFile = false) => {
    if (recordCount === 0 && !lastFile) return;

    // Write record count
    packBuffer.writeUInt32LE(recordCount, 0);

    let frameType = FrameType.Pack;
    let payload = packBuffer;

    if (compression !== 'none') {
      log(`Compressing pack (${compression})...`);
      let compressed;
      try {
        if (compression === 'lz4') {
          if (!lz4) {
            throw new Error('lz4 module unavailable');
          }
          compressed = lz4.encode(packBuffer);
          frameType = FrameType.PackLz4;
        } else if (compression === 'zstd') {
          if (!fzstd) {
            throw new Error('fzstd module unavailable');
          }
          compressed = fzstd.compress(packBuffer);
          frameType = FrameType.PackZstd;
        } else if (compression === 'lzma') {
          if (!lzma) {
            throw new Error('lzma-native module unavailable');
          }
          compressed = await lzma.compress(packBuffer);
          frameType = FrameType.PackLzma;
        }
      } catch (e) {
        log(`Compression failed (${compression}): ${e.message}. Sending uncompressed.`);
        // Fallback to uncompressed
        compressed = null;
        frameType = FrameType.Pack;
      }

      if (compressed && compressed.length < packBuffer.length) {
        // Prepend original size for ZSTD/LZMA
        if (compression === 'zstd' || compression === 'lzma') {
          const originalSizeBuf = Buffer.alloc(4);
          originalSizeBuf.writeUInt32LE(packBuffer.length, 0);
          payload = Buffer.concat([originalSizeBuf, compressed]);
        } else {
          payload = compressed;
        }
      } else {
        log(`Compressed size was larger or equal for ${compression}. Sending uncompressed.`);
        frameType = FrameType.Pack;
      }
    }

    await sendFrameHeader(socket, frameType, payload.length, cancel);
    await writeAllRetry(socket, payload, cancel);
    limiter.throttle(payload.length); // Apply rate limit to compressed/actual sent bytes

    totalSentBytes += packBytesAdded;
    totalSentFiles += packFilesAdded;

    const elapsed = (Date.now() - startTime) / 1000;
    progress(Number(totalSentBytes), totalSentFiles, elapsed, null); // currentFile will be updated separately

    packBuffer = Buffer.alloc(4);
    packBytesAdded = 0n;
    packFilesAdded = 0;
    recordCount = 0;
  };

  const addRecord = (relPathBytes, data) => {
    const recordHeader = Buffer.alloc(2 + relPathBytes.length + 8);
    recordHeader.writeUInt16LE(relPathBytes.length, 0);
    relPathBytes.copy(recordHeader, 2);
    recordHeader.writeBigUInt64LE(BigInt(data.length), 2 + relPathBytes.length);

    packBuffer = Buffer.concat([packBuffer, recordHeader, data]);
    packBytesAdded += BigInt(data.length);
    recordCount++;
  };

  for (const file of files) {
    if (cancel.value) {
      throw new Error('Upload cancelled by user');
    }

    log(`Packing: ${file.rel_path}`);

    const relPathBytes = Buffer.from(file.rel_path, 'utf8');
    let sawData = false;
    const stream = fs.createReadStream(file.abs_path, { highWaterMark: SEND_CHUNK_SIZE });

    for await (const chunk of stream) {
      if (cancel.value) {
        stream.destroy();
        throw new Error('Upload cancelled by user');
      }

      sawData = true;
      let offset = 0;

      while (offset < chunk.length) {
        const overhead = 2 + relPathBytes.length + 8;
        const remaining = PACK_BUFFER_SIZE - packBuffer.length;

        if (remaining <= overhead) {
          await flushPack();
          continue;
        }

        const maxData = remaining - overhead;
        const sliceLen = Math.min(maxData, chunk.length - offset);
        const dataSlice = sliceLen === chunk.length ? chunk : chunk.slice(offset, offset + sliceLen);

        addRecord(relPathBytes, dataSlice);
        offset += sliceLen;

        const elapsed = (Date.now() - startTime) / 1000;
        progress(Number(totalSentBytes) + Number(packBytesAdded), totalSentFiles + packFilesAdded, elapsed, file.rel_path);
      }
    }

    if (!sawData) {
      const overhead = 2 + relPathBytes.length + 8;
      if (PACK_BUFFER_SIZE - packBuffer.length <= overhead) {
        await flushPack();
      }
      addRecord(relPathBytes, Buffer.alloc(0));
    }

    packFilesAdded++;

    const elapsed = (Date.now() - startTime) / 1000;
    progress(Number(totalSentBytes) + Number(packBytesAdded), totalSentFiles + packFilesAdded, elapsed, file.rel_path);
  }

  // Flush remaining
  if (recordCount > 0) {
    await flushPack(true);
  }

  // Send finish frame
  await sendFrameHeader(socket, FrameType.Finish, 0, cancel);
  limiter.throttle(16); // Account for finish frame header

  return { files: totalSentFiles, bytes: Number(totalSentBytes) };
}

async function readUploadResponse(socket, cancel = { value: false }) {
  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    const timeout = setTimeout(() => {
      socket.destroy();
      reject(new Error('Read timeout'));
    }, 60000);

    socket.on('data', (chunk) => {
      data = Buffer.concat([data, chunk]);
      const str = data.toString('utf8');
      if (str.includes('\n')) {
        clearTimeout(timeout);
        resolve(str.trim());
      }
    });

    socket.on('error', (err) => {
      clearTimeout(timeout);
      reject(err);
    });

    socket.on('close', () => {
      clearTimeout(timeout);
      if (data.length > 0) {
        resolve(data.toString('utf8').trim());
      } else {
        reject(new Error('Connection closed'));
      }
    });
  });
}

function parseUploadResponse(response) {
  if (response.startsWith('OK ')) {
    const parts = response.split(/\s+/);
    if (parts.length >= 3) {
      return { files: parseInt(parts[1], 10), bytes: parseInt(parts[2], 10) };
    }
    return { files: 0, bytes: 0 };
  }
  throw new Error(response);
}

// ==================== Update Checking ====================

async function fetchUrl(url) {
  return new Promise((resolve, reject) => {
    const protocol = url.startsWith('https') ? https : http;
    const req = protocol.get(url, {
      headers: { 'User-Agent': 'ps5upload-desktop' }
    }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        fetchUrl(res.headers.location).then(resolve).catch(reject);
        return;
      }

      let data = '';
      res.on('data', (chunk) => { data += chunk; });
      res.on('end', () => resolve(data));
    });

    req.on('error', reject);
    req.setTimeout(30000, () => {
      req.destroy();
      reject(new Error('Timeout'));
    });
  });
}

async function fetchLatestRelease(includePrerelease = false) {
  const apiUrl = 'https://api.github.com/repos/phantomptr/ps5upload/releases';
  const data = await fetchUrl(apiUrl);
  const releases = JSON.parse(data);

  for (const release of releases) {
    if (!includePrerelease && release.prerelease) continue;
    return release;
  }

  throw new Error('No release found');
}

async function fetchReleaseByTag(tag) {
  const apiUrl = `https://api.github.com/repos/phantomptr/ps5upload/releases/tags/${tag}`;
  const data = await fetchUrl(apiUrl);
  return JSON.parse(data);
}

async function downloadAsset(url, destPath) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(destPath);
    const protocol = url.startsWith('https') ? https : http;

    const makeRequest = (requestUrl) => {
      const req = protocol.get(requestUrl, {
        headers: { 'User-Agent': 'ps5upload-desktop' }
      }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          makeRequest(res.headers.location);
          return;
        }

        if (res.statusCode !== 200) {
          file.close();
          fs.unlinkSync(destPath);
          reject(new Error(`HTTP ${res.statusCode}`));
          return;
        }

        res.pipe(file);
        file.on('finish', () => {
          file.close();
          resolve();
        });
      });

      req.on('error', (err) => {
        file.close();
        fs.unlinkSync(destPath);
        reject(err);
      });
    };

    makeRequest(url);
  });
}

// ==================== Payload ====================

function payloadPathIsElf(filepath) {
  const ext = path.extname(filepath).toLowerCase();
  return ext === '.elf' || ext === '.bin';
}

async function sendPayloadFile(ip, filepath) {
  if (!payloadPathIsElf(filepath)) {
    throw new Error('Payload must be a .elf or .bin file.');
  }

  const fileContent = fs.readFileSync(filepath);
  const socket = await createSocketWithTimeout(ip, PAYLOAD_PORT);

  return new Promise((resolve, reject) => {
    socket.write(fileContent, (err) => {
      if (err) {
        socket.destroy();
        reject(err);
        return;
      }

      socket.end();

      // Drain any response
      socket.on('data', () => {});
      socket.on('close', () => {
        resolve(fileContent.length);
      });
      socket.on('error', () => {
        resolve(fileContent.length);
      });
    });
  });
}

function probePayloadFile(filepath) {
  if (!payloadPathIsElf(filepath)) {
    return { is_ps5upload: false, message: 'Payload must be a .elf or .bin file.' };
  }

  const nameMatch = filepath.toLowerCase().includes('ps5upload');
  const content = fs.readFileSync(filepath, { encoding: null }).slice(0, 512 * 1024);
  const signatureMatch = content.includes(Buffer.from('ps5upload')) || content.includes(Buffer.from('PS5UPLOAD'));

  if (nameMatch || signatureMatch) {
    return { is_ps5upload: true, message: 'PS5Upload payload detected.' };
  }
  return { is_ps5upload: false, message: 'No PS5Upload signature found. Use only if you trust this payload.' };
}

// ==================== Pollers ====================

function startPayloadPoller() {
  if (payloadPoller) return;

  payloadPoller = setInterval(async () => {
    if (!state.payloadPollEnabled || !state.payloadIp) return;

    try {
      const status = await getPayloadStatus(state.payloadIp, TRANSFER_PORT);
      state.payloadStatus = { status, error: null, updated_at_ms: Date.now() };
      emit('payload_status_update', state.payloadStatus);
    } catch (err) {
      state.payloadStatus = { status: null, error: err.message, updated_at_ms: Date.now() };
      emit('payload_status_update', state.payloadStatus);
    }
  }, 5000);
}

function startConnectionPoller() {
  if (connectionPoller) return;

  connectionPoller = setInterval(async () => {
    if (!state.connectionPollEnabled || !state.connectionAutoEnabled || !state.connectionIp) return;

    try {
      const portOpen = await checkPort(state.connectionIp, TRANSFER_PORT);
      if (!portOpen) {
        state.connectionStatus = { is_connected: false, status: `Port ${TRANSFER_PORT} closed`, storage_locations: [] };
        emit('connection_status_update', state.connectionStatus);
        return;
      }

      const storage = await listStorage(state.connectionIp, TRANSFER_PORT);
      const available = storage.filter(loc => loc.free_gb > 0);

      if (available.length === 0) {
        state.connectionStatus = { is_connected: false, status: 'No storage', storage_locations: [] };
      } else {
        state.connectionStatus = { is_connected: true, status: 'Connected', storage_locations: available };
      }
      emit('connection_status_update', state.connectionStatus);
    } catch (err) {
      state.connectionStatus = { is_connected: false, status: `Error: ${err.message}`, storage_locations: [] };
      emit('connection_status_update', state.connectionStatus);
    }
  }, 5000);
}

function startManagePoller() {
  if (managePoller) return;

  managePoller = setInterval(async () => {
    if (!state.managePollEnabled || !state.manageIp || !state.managePath) return;

    try {
      const entries = await listDir(state.manageIp, TRANSFER_PORT, state.managePath);
      state.manageListCache = { path: state.managePath, entries, error: null, updated_at_ms: Date.now() };
      emit('manage_list_update', state.manageListCache);
    } catch (err) {
      state.manageListCache = { path: state.managePath, entries: [], error: err.message, updated_at_ms: Date.now() };
      emit('manage_list_update', state.manageListCache);
    }
  }, 3000);
}

async function checkPort(ip, port) {
  return new Promise((resolve) => {
    const socket = new net.Socket();
    socket.setTimeout(2000);

    socket.on('connect', () => {
      socket.destroy();
      resolve(true);
    });

    socket.on('timeout', () => {
      socket.destroy();
      resolve(false);
    });

    socket.on('error', () => {
      resolve(false);
    });

    socket.connect(port, ip);
  });
}

// ==================== IPC Handlers ====================

function registerIpcHandlers() {
  // App
  ipcMain.handle('app_version', () => VERSION);

  // Window controls
  ipcMain.handle('window_minimize', () => mainWindow?.minimize());
  ipcMain.handle('window_maximize', () => mainWindow?.isMaximized() ? mainWindow.unmaximize() : mainWindow?.maximize());
  ipcMain.handle('window_close', () => mainWindow?.close());

  // Dialogs
  ipcMain.handle('dialog_open', async (_, options) => {
    const result = await dialog.showOpenDialog(mainWindow, options);
    return result.canceled ? null : result.filePaths;
  });

  ipcMain.handle('dialog_save', async (_, options) => {
    const result = await dialog.showSaveDialog(mainWindow, options);
    return result.canceled ? null : result.filePath;
  });

  // Config
  ipcMain.handle('config_load', () => loadConfig());
  ipcMain.handle('config_save', (_, config) => { saveConfig(config); return true; });
  ipcMain.handle('config_update', (_, config) => { saveConfig(config); return true; });

  // Profiles
  ipcMain.handle('profiles_load', () => loadProfiles());
  ipcMain.handle('profiles_save', (_, data) => { saveProfiles(data); return true; });
  ipcMain.handle('profiles_update', (_, data) => { saveProfiles(data); return true; });

  // Queue
  ipcMain.handle('queue_load', () => loadQueue());
  ipcMain.handle('queue_save', (_, data) => { saveQueue(data); return true; });
  ipcMain.handle('queue_update', (_, data) => { saveQueue(data); return true; });

  // History
  ipcMain.handle('history_load', () => loadHistory());
  ipcMain.handle('history_add', (_, record) => { addHistoryRecord(record); return true; });
  ipcMain.handle('history_clear', () => { clearHistory(); return true; });

  // Logging
  ipcMain.handle('set_save_logs', (_, enabled) => { state.saveLogs = enabled; return true; });
  ipcMain.handle('set_ui_log_enabled', (_, enabled) => { state.uiLogEnabled = enabled; return true; });

  // Storage
  ipcMain.handle('storage_list', async (_, ip) => {
    return listStorage(ip, TRANSFER_PORT);
  });

  // Port check
  ipcMain.handle('port_check', async (_, ip, port) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return checkPort(ip.trim(), port);
  });

  // Connection
  ipcMain.handle('connection_set_ip', (_, ip) => { state.connectionIp = ip.trim(); return true; });
  ipcMain.handle('connection_polling_set', (_, enabled) => { state.connectionPollEnabled = enabled; return true; });
  ipcMain.handle('connection_auto_set', (_, enabled) => { state.connectionAutoEnabled = enabled; return true; });
  ipcMain.handle('connection_snapshot', () => state.connectionStatus);
  ipcMain.handle('connection_connect', async (_, ip) => {
    if (!ip || !ip.trim()) {
      return { is_connected: false, status: 'Missing IP', storage_locations: [] };
    }

    try {
      const portOpen = await checkPort(ip, TRANSFER_PORT);
      if (!portOpen) {
        const result = { is_connected: false, status: `Port ${TRANSFER_PORT} closed`, storage_locations: [] };
        state.connectionStatus = result;
        emit('connection_status_update', result);
        return result;
      }

      const storage = await listStorage(ip, TRANSFER_PORT);
      const available = storage.filter(loc => loc.free_gb > 0);

      let result;
      if (available.length === 0) {
        result = { is_connected: false, status: 'No storage', storage_locations: [] };
      } else {
        result = { is_connected: true, status: 'Connected', storage_locations: available };
      }
      state.connectionStatus = result;
      emit('connection_status_update', result);
      return result;
    } catch (err) {
      const result = { is_connected: false, status: `Error: ${err.message}`, storage_locations: [] };
      state.connectionStatus = result;
      emit('connection_status_update', result);
      return result;
    }
  });

  // Payload
  ipcMain.handle('payload_send', async (_, ip, filepath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!filepath || !filepath.trim()) throw new Error('Select a payload (.elf/.bin) file first.');

    emit('payload_busy', { busy: true });
    try {
      emit('payload_log', { message: `Sending payload to ${ip}:${PAYLOAD_PORT}...` });
      emit('payload_log', { message: `Payload path: ${filepath}` });

      const bytes = await sendPayloadFile(ip, filepath);
      emit('payload_log', { message: 'Payload sent successfully.' });
      emit('payload_done', { bytes, error: null });
    } catch (err) {
      emit('payload_log', { message: `Payload failed: ${err.message}` });
      emit('payload_done', { bytes: null, error: err.message });
      throw err;
    } finally {
      emit('payload_busy', { busy: false });
    }
    return true;
  });

  ipcMain.handle('payload_download_and_send', async (_, ip, fetch) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');

    emit('payload_busy', { busy: true });

    try {
      const logLabel = fetch === 'current' ? `Downloading payload v${VERSION}...` : 'Downloading latest payload...';
      emit('payload_log', { message: logLabel });

      let release;
      if (fetch === 'current') {
        try {
          release = await fetchReleaseByTag(`v${VERSION}`);
        } catch {
          emit('payload_log', { message: `Tag v${VERSION} not found, falling back to latest release.` });
          release = await fetchLatestRelease(false);
        }
      } else {
        release = await fetchLatestRelease(false);
      }

      const asset = release.assets.find(a => a.name === 'ps5upload.elf');
      if (!asset) throw new Error('Payload asset not found');

      const tmpPath = path.join(os.tmpdir(), `ps5upload_${fetch}.elf`);
      await downloadAsset(asset.browser_download_url, tmpPath);

      emit('payload_log', { message: `Payload downloaded: ${tmpPath}` });

      const bytes = await sendPayloadFile(ip, tmpPath);
      emit('payload_done', { bytes, error: null });
    } catch (err) {
      emit('payload_done', { bytes: null, error: err.message });
      throw err;
    } finally {
      emit('payload_busy', { busy: false });
    }
    return true;
  });

  ipcMain.handle('payload_check', async (_, ip) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');

    try {
      const version = await getPayloadVersion(ip, TRANSFER_PORT);
      emit('payload_version', { version, error: null });
    } catch (err) {
      emit('payload_version', { version: null, error: err.message });
      throw err;
    }
    return true;
  });

  ipcMain.handle('payload_probe', (_, filepath) => {
    if (!filepath || !filepath.trim()) throw new Error('Select a payload (.elf/.bin) file first.');
    return probePayloadFile(filepath);
  });

  ipcMain.handle('payload_status', async (_, ip) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return getPayloadStatus(ip, TRANSFER_PORT);
  });

  ipcMain.handle('payload_status_snapshot', () => state.payloadStatus || { status: null, error: null, updated_at_ms: 0 });

  ipcMain.handle('payload_status_refresh', async (_, ip) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');

    try {
      const status = await getPayloadStatus(ip, TRANSFER_PORT);
      state.payloadStatus = { status, error: null, updated_at_ms: Date.now() };
    } catch (err) {
      state.payloadStatus = { status: null, error: err.message, updated_at_ms: Date.now() };
    }
    emit('payload_status_update', state.payloadStatus);
    return state.payloadStatus;
  });

  ipcMain.handle('payload_polling_set', (_, enabled) => { state.payloadPollEnabled = enabled; return true; });
  ipcMain.handle('payload_set_ip', (_, ip) => { state.payloadIp = ip.trim(); return true; });
  ipcMain.handle('payload_auto_reload_set', (_, enabled, mode, localPath) => {
    state.payloadAutoReloadEnabled = enabled;
    state.payloadAutoReloadMode = mode;
    state.payloadAutoReloadPath = localPath;
    return true;
  });

  ipcMain.handle('payload_queue_extract', async (_, ip, src, dst) => {
    try {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!src || !src.trim()) throw new Error('Source path is required.');
    if (!dst || !dst.trim()) throw new Error('Destination path is required.');
    return queueExtract(ip, TRANSFER_PORT, src, dst);
  } catch (err) {
    throw err;
  }
  });

  ipcMain.handle('payload_queue_cancel', async (_, ip, id) => {
    try {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return queueCancel(ip, TRANSFER_PORT, id);
  } catch (err) {
    throw err;
  }
  });

  ipcMain.handle('payload_queue_clear', async (_, ip) => {
    try {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return queueClear(ip, TRANSFER_PORT);
  } catch (err) {
    throw err;
  }
  });

  // Manage
  ipcMain.handle('manage_list', async (_, ip, dirPath) => {
    return listDir(ip, TRANSFER_PORT, dirPath);
  });

  ipcMain.handle('manage_list_snapshot', () => state.manageListCache);

  ipcMain.handle('manage_list_refresh', async (_, ip, dirPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!dirPath || !dirPath.trim()) throw new Error('Enter a path.');

    try {
      const entries = await listDir(ip, TRANSFER_PORT, dirPath);
      state.manageListCache = { path: dirPath, entries, error: null, updated_at_ms: Date.now() };
    } catch (err) {
      state.manageListCache = { path: dirPath, entries: [], error: err.message, updated_at_ms: Date.now() };
    }
    emit('manage_list_update', state.manageListCache);
    return state.manageListCache;
  });

  ipcMain.handle('manage_polling_set', (_, enabled) => { state.managePollEnabled = enabled; return true; });
  ipcMain.handle('manage_set_ip', (_, ip) => { state.manageIp = ip.trim(); return true; });
  ipcMain.handle('manage_set_path', (_, dirPath) => { state.managePath = dirPath.trim(); return true; });
  ipcMain.handle('manage_cancel', () => { state.manageCancel = true; return true; });

  ipcMain.handle('manage_delete', async (_, ip, filepath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!filepath || !filepath.trim()) throw new Error('Select a path to delete.');

    emit('manage_log', { message: `Delete ${filepath}` });
    try {
      await deletePath(ip, TRANSFER_PORT, filepath);
      emit('manage_done', { op: 'Delete', bytes: null, error: null });
    } catch (err) {
      emit('manage_done', { op: 'Delete', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_rename', async (_, ip, srcPath, dstPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!srcPath || !srcPath.trim() || !dstPath || !dstPath.trim()) throw new Error('Source and destination are required.');

    emit('manage_log', { message: `Rename ${srcPath} -> ${dstPath}` });
    try {
      await movePath(ip, TRANSFER_PORT, srcPath, dstPath);
      emit('manage_done', { op: 'Rename', bytes: null, error: null });
    } catch (err) {
      emit('manage_done', { op: 'Rename', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_create_dir', async (_, ip, dirPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!dirPath || !dirPath.trim()) throw new Error('Folder path is required.');

    emit('manage_log', { message: `Create folder ${dirPath}` });
    try {
      await createPath(ip, TRANSFER_PORT, dirPath);
      emit('manage_done', { op: 'Create', bytes: null, error: null });
    } catch (err) {
      emit('manage_done', { op: 'Create', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_chmod', async (_, ip, filepath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!filepath || !filepath.trim()) throw new Error('Select a path.');

    emit('manage_log', { message: `chmod 777 ${filepath}` });
    try {
      await chmod777(ip, TRANSFER_PORT, filepath);
      emit('manage_done', { op: 'chmod', bytes: null, error: null });
    } catch (err) {
      emit('manage_done', { op: 'chmod', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_move', async (_, ip, srcPath, dstPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!srcPath || !srcPath.trim() || !dstPath || !dstPath.trim()) throw new Error('Source and destination are required.');

    emit('manage_log', { message: `Move ${srcPath} -> ${dstPath}` });
    try {
      await movePath(ip, TRANSFER_PORT, srcPath, dstPath);
      emit('manage_done', { op: 'Move', bytes: null, error: null });
    } catch (err) {
      emit('manage_done', { op: 'Move', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_copy', async (_, ip, srcPath, dstPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!srcPath || !srcPath.trim() || !dstPath || !dstPath.trim()) throw new Error('Source and destination are required.');

    emit('manage_log', { message: `Copy ${srcPath} -> ${dstPath}` });

    // Copy is done via COPY command
    try {
      const response = await sendSimpleCommand(ip, TRANSFER_PORT, `COPY ${srcPath}\t${dstPath}\n`);
      if (!response.startsWith('OK')) {
        throw new Error(`Copy failed: ${response}`);
      }
      emit('manage_done', { op: 'Copy', bytes: null, error: null });
    } catch (err) {
      emit('manage_done', { op: 'Copy', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_extract', async (_, ip, srcPath, dstPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!srcPath || !srcPath.trim() || !dstPath || !dstPath.trim()) throw new Error('Source and destination are required.');

    emit('manage_log', { message: `Extract ${srcPath} -> ${dstPath}` });

    try {
      const response = await sendSimpleCommand(ip, TRANSFER_PORT, `EXTRACT_ARCHIVE ${srcPath}\t${dstPath}\n`);
      if (!response.startsWith('OK')) {
        throw new Error(`Extract failed: ${response}`);
      }
      emit('manage_done', { op: 'Extract', bytes: null, error: null });
    } catch (err) {
      emit('manage_done', { op: 'Extract', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_download_file', async (_, ip, filepath, destPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!filepath || !filepath.trim() || !destPath || !destPath.trim()) throw new Error('Source and destination are required.');

    emit('manage_log', { message: `Download ${filepath}` });

    try {
      const socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
      socket.write(`DOWNLOAD ${filepath}\n`);

      let headerReceived = false;
      let totalSize = 0;
      let received = 0;
      let headerBuf = Buffer.alloc(0);

      const fileStream = fs.createWriteStream(destPath);

      await new Promise((resolve, reject) => {
        socket.on('data', (chunk) => {
          if (!headerReceived) {
            headerBuf = Buffer.concat([headerBuf, chunk]);
            const newlineIdx = headerBuf.indexOf('\n');
            if (newlineIdx !== -1) {
              const header = headerBuf.slice(0, newlineIdx).toString('utf8').trim();
              if (!header.startsWith('OK ')) {
                socket.destroy();
                reject(new Error(`Download failed: ${header}`));
                return;
              }
              totalSize = parseInt(header.substring(3).trim(), 10);
              headerReceived = true;
              emit('manage_progress', { op: 'Download', processed: 0, total: totalSize, current_file: filepath });

              const remaining = headerBuf.slice(newlineIdx + 1);
              if (remaining.length > 0) {
                fileStream.write(remaining);
                received += remaining.length;
              }
            }
          } else {
            fileStream.write(chunk);
            received += chunk.length;
            emit('manage_progress', { op: 'Download', processed: received, total: totalSize, current_file: null });
          }

          if (headerReceived && received >= totalSize) {
            fileStream.end();
            socket.destroy();
            resolve();
          }
        });

        socket.on('error', (err) => {
          fileStream.end();
          reject(err);
        });

        socket.on('close', () => {
          fileStream.end();
          if (received >= totalSize) {
            resolve();
          }
        });
      });

      emit('manage_done', { op: 'Download', bytes: received, error: null });
    } catch (err) {
      emit('manage_done', { op: 'Download', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_download_dir', async (_, ip, dirPath, destPath, compression) => {
    // Simplified version - download files individually
    emit('manage_log', { message: `Download ${dirPath}` });
    emit('manage_done', { op: 'Download', bytes: null, error: 'Directory download not yet implemented' });
    return true;
  });

  ipcMain.handle('manage_upload', async (_, ip, destRoot, paths) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!destRoot || !destRoot.trim()) throw new Error('Destination path is required.');
    if (!paths || paths.length === 0) throw new Error('Select at least one file or folder.');

    emit('manage_log', { message: 'Upload started.' });

    try {
      let totalBytes = 0;

      for (const srcPath of paths) {
        const stat = fs.statSync(srcPath);
        let files;
        let dest;

        if (stat.isDirectory()) {
          const folderName = path.basename(srcPath);
          dest = `${destRoot.replace(/\/$/, '')}/${folderName}`;
          const result = collectFiles(srcPath);
          files = result.files;
        } else {
          dest = destRoot;
          files = [{
            rel_path: path.basename(srcPath),
            abs_path: srcPath,
            size: stat.size,
            mtime: Math.floor(stat.mtimeMs / 1000),
          }];
        }

        const batchBytes = files.reduce((sum, f) => sum + f.size, 0);
        totalBytes += batchBytes;

        emit('manage_progress', { op: 'Upload', processed: 0, total: totalBytes, current_file: null });

        const socket = await uploadV2Init(ip, TRANSFER_PORT, dest, false);
        await sendFilesV2(files, socket, {
          progress: (sent, filesSent, elapsed) => {
            emit('manage_progress', { op: 'Upload', processed: sent, total: totalBytes, current_file: null });
          },
          log: (msg) => emit('manage_log', { message: msg }),
        });

        const response = await readUploadResponse(socket);
        parseUploadResponse(response);
      }

      emit('manage_done', { op: 'Upload', bytes: totalBytes, error: null });
    } catch (err) {
      emit('manage_done', { op: 'Upload', bytes: null, error: err.message });
    }
    return true;
  });

  // Upload RAR for server-side extraction
  ipcMain.handle('manage_upload_rar', async (_, ip, rarPath, destPath, mode) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!rarPath || !rarPath.trim()) throw new Error('RAR file path is required.');
    if (!destPath || !destPath.trim()) throw new Error('Destination path is required.');

    emit('manage_log', { message: `Uploading RAR ${rarPath} for extraction to ${destPath}` });

    try {
      const fileSize = (await fs.promises.stat(rarPath)).size;

      let triedFallback = false;
      let modeToTry = mode;
      let socket;

      while (true) {
        if (state.manageCancel) {
          throw new Error('Upload cancelled');
        }

        socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
        const cmd = `UPLOAD_RAR_${modeToTry.toUpperCase()} ${destPath} ${fileSize}\n`;
        socket.write(cmd);

        let response = '';
        try {
          response = await new Promise((resolve, reject) => {
            let data = Buffer.alloc(0);
            const timeout = setTimeout(() => {
              socket.destroy();
              reject(new Error('Read timeout'));
            }, READ_TIMEOUT_MS);

            socket.on('data', (chunk) => {
              data = Buffer.concat([data, chunk]);
              if (data.includes(Buffer.from('\n'))) {
                clearTimeout(timeout);
                resolve(data.toString('utf8').trim());
              }
            });
            socket.on('error', reject);
            socket.on('close', () => resolve('')); // Handle unexpected close
          });
        } catch (err) {
          socket.destroy();
          throw err;
        }

        if (response === 'READY') {
          break; // Server is ready, proceed with upload
        }

        socket.destroy();

        if (!triedFallback && modeToTry !== 'normal' && response.includes('Unknown command')) {
          triedFallback = true;
          modeToTry = 'normal';
          emit('manage_log', { message: 'RAR mode unsupported by payload. Retrying with Normal.' });
          continue; // Try again with normal mode
        }

        throw new Error(`Server rejected RAR upload: ${response}`);
      }

      // --- Send RAR file data ---
      const fileStream = fs.createReadStream(rarPath);
      let sentBytes = 0;
      const progressInterval = setInterval(() => {
        emit('manage_progress', { op: 'Extract', processed: sentBytes, total: fileSize, current_file: path.basename(rarPath) });
      }, 1000);

      try {
        await new Promise((resolve, reject) => {
          fileStream.on('data', (chunk) => {
            if (state.manageCancel) {
              fileStream.destroy();
              socket.destroy();
              return reject(new Error('Upload cancelled'));
            }
            if (!socket.write(chunk)) {
              fileStream.pause();
              socket.once('drain', () => {
                fileStream.resume();
              });
            }
            sentBytes += chunk.length;
          });

          fileStream.on('end', () => {
            socket.end(); // Signal end of file data
            resolve();
          });

          fileStream.on('error', (err) => {
            socket.destroy();
            reject(err);
          });

          socket.on('error', (err) => {
            fileStream.destroy();
            reject(err);
          });
        });
      } finally {
        clearInterval(progressInterval);
      }

      // --- Monitor extraction progress ---
      const extractResult = await new Promise((resolve, reject) => {
        let lineBuffer = '';
        const timeout = setTimeout(() => {
          socket.destroy();
          reject(new Error('Extraction timed out (no progress for 10m)'));
        }, 600000); // 10 minutes timeout

        socket.on('data', (chunk) => {
          clearTimeout(timeout);
          lineBuffer += chunk.toString('utf8');
          let newlineIndex;
          while ((newlineIndex = lineBuffer.indexOf('\n')) !== -1) {
            const line = lineBuffer.substring(0, newlineIndex).trim();
            lineBuffer = lineBuffer.substring(newlineIndex + 1);

            if (line.startsWith('SUCCESS ')) {
              const parts = line.split(' ');
              const files = parseInt(parts[1], 10) || 0;
              const bytes = parseInt(parts[2], 10) || 0;
              resolve({ files, bytes });
              return;
            } else if (line.startsWith('ERROR: ')) {
              reject(new Error(`RAR extraction failed: ${line}`));
              return;
            } else if (line.startsWith('EXTRACT_PROGRESS ')) {
              const parts = line.split(' ');
              const processed = parseInt(parts[2], 10) || 0;
              const total = parseInt(parts[3], 10) || 0;
              let currentFile = null;
              if (parts.length > 4) { // parts[0] is "EXTRACT_PROGRESS", parts[1] is maybe a token, parts[2] is processed, parts[3] is total
                currentFile = parts.slice(4).join(' '); // Remaining parts form the filename
              }
              emit('manage_progress', { op: 'Extract', processed, total, current_file: currentFile });
            } else if (line.startsWith('EXTRACTING ')) {
              const rest = line.substring('EXTRACTING '.length);
              const spaceIndex = rest.indexOf(' ');
              if (spaceIndex !== -1) {
                const count = rest.substring(0, spaceIndex);
                const filename = rest.substring(spaceIndex + 1);
                emit('manage_log', { message: `Extracting (${count}): ${filename}` });
              }
            }
          }
          timeout.refresh(); // Reset timeout on data
        });

        socket.on('error', reject);
        socket.on('close', () => {
          reject(new Error('Connection closed unexpectedly during extraction monitoring.'));
        });
      });

      emit('manage_done', { op: 'Extract', bytes: extractResult.bytes, error: null });
      return true;
    } catch (err) {
      emit('manage_done', { op: 'Extract', bytes: null, error: err.message });
      throw err;
    }
  });

  // Transfer
  ipcMain.handle('transfer_check_dest', async (_, ip, destPath) => {
    return checkDir(ip, TRANSFER_PORT, destPath);
  });

  ipcMain.handle('transfer_scan', async (_, sourcePath) => {
    state.transferRunId++;
    const runId = state.transferRunId;
    state.transferCancel = false;
    state.transferStatus = { run_id: runId, status: 'Scanning', sent: 0, total: 0, files: 0, elapsed_secs: 0, current_file: '' };

    setImmediate(() => {
      const result = collectFiles(sourcePath, { value: state.transferCancel }, (filesFound, totalSize) => {
        state.transferStatus = { ...state.transferStatus, files: filesFound, total: totalSize };
      });

      const totalSize = result.files.reduce((sum, f) => sum + f.size, 0);
      state.transferStatus = { ...state.transferStatus, files: result.files.length, total: totalSize };
    });

    return runId;
  });

  ipcMain.handle('transfer_cancel', () => {
    state.transferCancel = true;
    state.transferStatus = { ...state.transferStatus, status: 'Cancelled' };
    return true;
  });

  ipcMain.handle('transfer_status', () => state.transferStatus);

  ipcMain.handle('transfer_start', async (_, req) => {
    if (!req.ip || !req.ip.trim()) throw new Error('PS5 IP address is required');
    if (!req.source_path || !req.source_path.trim()) throw new Error('Source path is required');
    if (!req.dest_path || !req.dest_path.trim()) throw new Error('Destination path is required');

    if (state.transferActive) throw new Error('Transfer already running');

    state.transferRunId++;
    const runId = state.transferRunId;
    state.transferCancel = false;
    state.transferActive = true;
    state.transferStatus = { run_id: runId, status: 'Starting', sent: 0, total: req.required_size || 0, files: 0, elapsed_secs: 0, current_file: '' };

    const emitLog = (message) => {
      if (state.saveLogs) writeLogLine('transfer', `[${runId}] ${message}`);
      if (state.uiLogEnabled) emit('transfer_log', { run_id: runId, message });
    };

    setImmediate(async () => {
      const startTime = Date.now();

      try {
        emitLog('Scanning files...');
        const result = collectFiles(req.source_path, { value: state.transferCancel }, (filesFound, totalSize) => {
          state.transferStatus = { ...state.transferStatus, status: 'Scanning', files: filesFound, total: totalSize };
        });

        if (result.cancelled) {
          throw new Error('Cancelled');
        }

        if (result.files.length === 0) {
          throw new Error('No files found to upload');
        }

        const totalSize = result.files.reduce((sum, f) => sum + f.size, 0);
        emitLog(`Starting transfer: ${(totalSize / (1024 * 1024 * 1024)).toFixed(2)} GB using ${req.connections} connection(s)`);

        state.transferStatus = { ...state.transferStatus, status: 'Uploading', files: result.files.length, total: totalSize };

        const socket = await uploadV2Init(req.ip, TRANSFER_PORT, req.dest_path, req.use_temp);
        const rateLimitBps = req.bandwidth_limit_mbps ? req.bandwidth_limit_mbps * 1024 * 1024 / 8 : null; // Convert Mbps to Bps

        const uploadResult = await sendFilesV2(result.files, socket, {
          cancel: { value: state.transferCancel },
          progress: (sent, filesSent, elapsed, currentFile) => {
            state.transferStatus = {
              run_id: runId,
              status: 'Uploading',
              sent,
              total: totalSize,
              files: filesSent,
              elapsed_secs: elapsed,
              current_file: currentFile || '',
            };
          },
          log: emitLog,
          compression: req.compression,
          rateLimitBps: rateLimitBps,
        });

        const response = await readUploadResponse(socket);
        const parsed = parseUploadResponse(response);

        const elapsed = (Date.now() - startTime) / 1000;
        state.transferStatus = { run_id: runId, status: 'Complete', sent: totalSize, total: totalSize, files: parsed.files, elapsed_secs: elapsed, current_file: '' };

        emit('transfer_complete', { run_id: runId, files: parsed.files, bytes: parsed.bytes });
      } catch (err) {
        emit('transfer_error', { run_id: runId, message: err.message });
        state.transferStatus = { ...state.transferStatus, status: `Error: ${err.message}` };
      } finally {
        state.transferActive = false;
      }
    });

    return runId;
  });

  // Updates
  ipcMain.handle('update_check', async (_, includePrerelease) => {
    return fetchLatestRelease(includePrerelease);
  });

  ipcMain.handle('update_check_tag', async (_, tag) => {
    return fetchReleaseByTag(tag);
  });

  ipcMain.handle('update_download_asset', async (_, url, destPath) => {
    await downloadAsset(url, destPath);
    return { path: destPath };
  });

  ipcMain.handle('update_current_asset_name', () => {
    const platform = process.platform;
    const arch = process.arch;

    if (platform === 'win32') {
      return 'ps5upload-windows.zip';
    } else if (platform === 'darwin') {
      return arch === 'arm64' ? 'ps5upload-macos-arm64.zip' : 'ps5upload-macos-x64.zip';
    } else {
      return 'ps5upload-linux.zip';
    }
  });

  ipcMain.handle('update_prepare_self', async (_, assetUrl) => {
    // Simplified - just emit ready for now
    emit('update_ready', {});
    return true;
  });

  ipcMain.handle('update_apply_self', () => {
    // Would need proper update logic
    app.quit();
    return true;
  });

  // Chat (simplified - no actual WebSocket implementation)
  ipcMain.handle('chat_info', () => {
    return { room_id: '', enabled: false };
  });

  ipcMain.handle('chat_generate_name', () => {
    return `User${Math.floor(Math.random() * 10000)}`;
  });

  ipcMain.handle('chat_start', () => {
    return { room_id: '', enabled: false };
  });

  ipcMain.handle('chat_send', (_, name, text) => {
    // No-op for now
    return true;
  });

  // Game meta (simplified)
  ipcMain.handle('game_meta_load', async (_, sourcePath) => {
    return { meta: null, cover: null };
  });

  ipcMain.handle('manage_rar_metadata', async (_, ip, filepath) => {
    return { meta: null, cover: null };
  });
}

// App lifecycle
app.whenReady().then(() => {
  registerIpcHandlers();
  createWindow();
  startPayloadPoller();
  startConnectionPoller();
  startManagePoller();

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('window-all-closed', () => {
  if (payloadPoller) clearInterval(payloadPoller);
  if (connectionPoller) clearInterval(connectionPoller);
  if (managePoller) clearInterval(managePoller);

  if (process.platform !== 'darwin') app.quit();
});
