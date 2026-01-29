const { app, BrowserWindow, ipcMain, dialog, nativeImage, shell, powerSaveBlocker } = require('electron');
const path = require('path');
const fs = require('fs');
const os = require('os');
const net = require('net');
const dgram = require('dgram');
const https = require('https');
const http = require('http');
const crypto = require('crypto');
const { promisify } = require('util');
const { execFile } = require('child_process');
const { pipeline, PassThrough } = require('stream');
const { once } = require('events');
const streamPipeline = promisify(pipeline);
const execFileAsync = promisify(execFile);
if (process.platform === 'linux') {
  app.commandLine.appendSwitch('no-sandbox');
  app.commandLine.appendSwitch('disable-setuid-sandbox');
}
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
const tar = tryRequire('tar');

// Constants
const TRANSFER_PORT = 9113;
const PAYLOAD_PORT = 9021;
const CONNECTION_TIMEOUT_MS = 30000;
const READ_TIMEOUT_MS = 120000;
const PAYLOAD_STATUS_CONNECT_TIMEOUT_MS = 5000;
const PAYLOAD_STATUS_READ_TIMEOUT_MS = 10000;
const PACK_BUFFER_SIZE = 48 * 1024 * 1024; // 48MB
const PACK_BUFFER_MIN = 4 * 1024 * 1024; // 4MB
const SEND_CHUNK_SIZE = 4 * 1024 * 1024; // 4MB
const SEND_CHUNK_MIN = 512 * 1024; // 512KB
const ADAPTIVE_POLL_MS = 2000;
const TINY_FILE_AVG_BYTES = 64 * 1024; // 64KB
const SMALL_FILE_AVG_BYTES = 256 * 1024; // 256KB
const RESUME_HASH_LARGE_BYTES = 1024 * 1024 * 1024; // 1GB
const RESUME_HASH_MED_BYTES = 128 * 1024 * 1024; // 128MB
const WRITE_CHUNK_SIZE = 512 * 1024; // 512KB
const MAGIC_FTX1 = 0x31585446;

let sleepBlockerId = null;
const VERSION = '1.3.7';
const IS_WINDOWS = process.platform === 'win32';

function beginManageOperation(op) {
  state.manageDoneEmitted = false;
  state.manageActiveOp = op;
}

function emitManageDone(payload) {
  if (state.manageDoneEmitted) return;
  state.manageDoneEmitted = true;
  emit('manage_done', payload);
}

const FrameType = {
  Pack: 4,
  PackLz4: 8,
  PackZstd: 9,
  PackLzma: 10,
  Finish: 6,
  Error: 7,
};

const ARCHIVE_EXTENSIONS = new Set(['.rar']);
const SEVEN_Z_COMMANDS = ['7z', '7za', '7zz'];
const COVER_CANDIDATES = [
  'icon0.png',
  'icon0.jpg',
  'icon0.jpeg',
  'icon.png',
  'cover.png',
  'cover.jpg',
  'tile0.png',
];

async function tryExecFile(command, args, options = {}) {
  try {
    return await execFileAsync(command, args, {
      maxBuffer: 50 * 1024 * 1024,
      ...options,
    });
  } catch (err) {
    if (err && err.code === 'ENOENT') return null;
    if (err && err.stdout) return { stdout: err.stdout, stderr: err.stderr || '' };
    return null;
  }
}

async function listArchiveEntries(archivePath) {
  for (const cmd of SEVEN_Z_COMMANDS) {
    const result = await tryExecFile(cmd, ['l', '-slt', archivePath], { encoding: 'utf8' });
    if (!result || !result.stdout) continue;
    const entries = [];
    const lines = result.stdout.split(/\r?\n/);
    for (const line of lines) {
      const match = line.match(/^Path = (.+)$/);
      if (match) {
        entries.push(match[1].trim());
      }
    }
    if (entries.length) return entries;
  }
  return null;
}

async function extractArchiveFile(archivePath, entryPath) {
  for (const cmd of SEVEN_Z_COMMANDS) {
    const result = await tryExecFile(
      cmd,
      ['e', '-so', '-y', '-bd', archivePath, entryPath],
      { encoding: 'buffer' }
    );
    if (result && result.stdout && result.stdout.length > 0) {
      return result.stdout;
    }
  }
  return null;
}

function normalizeArchiveEntry(entry) {
  return entry.replace(/\\/g, '/').replace(/^\.\/+/, '');
}

function findArchiveParamEntry(entries) {
  const normalized = entries.map(normalizeArchiveEntry);
  const sceParam = normalized.find((e) => e.toLowerCase().endsWith('/sce_sys/param.json') || e.toLowerCase() === 'sce_sys/param.json');
  if (sceParam) return sceParam;
  const directParam = normalized.find((e) => e.toLowerCase().endsWith('/param.json') || e.toLowerCase() === 'param.json');
  return directParam || null;
}

function findArchiveCoverEntry(entries, paramEntry) {
  const normalized = entries.map(normalizeArchiveEntry);
  let preferredBase = null;
  if (paramEntry) {
    const lower = paramEntry.toLowerCase();
    if (lower.includes('/sce_sys/param.json')) {
      preferredBase = paramEntry.slice(0, lower.lastIndexOf('/sce_sys/param.json')) + '/sce_sys';
    } else if (lower.endsWith('/param.json')) {
      preferredBase = paramEntry.slice(0, lower.lastIndexOf('/param.json'));
    }
  }
  if (preferredBase) {
    for (const candidate of COVER_CANDIDATES) {
      const target = `${preferredBase}/${candidate}`.toLowerCase();
      const entry = normalized.find((e) => e.toLowerCase() === target);
      if (entry) return entry;
    }
  }
  for (const candidate of COVER_CANDIDATES) {
    const entry = normalized.find(
      (e) => e.toLowerCase() === candidate || e.toLowerCase().endsWith(`/${candidate}`)
    );
    if (entry) return entry;
  }
  return null;
}

async function uploadRarForExtraction(ip, rarPath, destPath, mode, opts = {}) {
  const {
    cancel = { value: false },
    onProgress = () => {},
    onLog = () => {},
    overrideOnConflict = true,
    tempRoot = ''
  } = opts;
  const fileSize = (await fs.promises.stat(rarPath)).size;

  try {
    await createPath(ip, TRANSFER_PORT, destPath);
  } catch (err) {
    const message = err && err.message ? err.message : String(err);
    throw new Error(`Create destination failed: ${message}`);
  }

  let triedFallback = false;
  let modeToTry = mode;
  let socket;

  while (true) {
    if (cancel.value) throw new Error('Upload cancelled');

    socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
    tuneUploadSocket(socket);
    if (String(modeToTry || '').toLowerCase() === 'turbo') {
      onLog('Turbo mode: progress updates are reduced; totals may be unavailable.');
    }
    const flag = overrideOnConflict ? '' : ' NOOVERWRITE';
    const cleanedTempRoot = typeof tempRoot === 'string' ? tempRoot.trim() : '';
    if (cleanedTempRoot && /\s/.test(cleanedTempRoot)) {
      socket.destroy();
      throw new Error('Temp storage path must not contain spaces.');
    }
    const tmpToken = cleanedTempRoot ? ` TMP=${cleanedTempRoot}` : '';
    const cmd = `UPLOAD_RAR_${String(modeToTry || 'normal').toUpperCase()} ${destPath} ${fileSize}${tmpToken}${flag}\n`;
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
        socket.on('close', () => resolve(''));
      });
    } catch (err) {
      socket.destroy();
      throw err;
    }

    if (response === 'READY') {
      break;
    }

    socket.destroy();

    if (!triedFallback && modeToTry !== 'normal' && response.includes('Unknown command')) {
      triedFallback = true;
      modeToTry = 'normal';
      onLog('RAR mode unsupported by payload. Retrying with Normal.');
      continue;
    }
    if (!overrideOnConflict && (response.includes('Invalid file size') || response.includes('Invalid UPLOAD_RAR format'))) {
      throw new Error('Payload does not support archive conflict checks. Update payload or enable Override.');
    }

    throw new Error(`Server rejected RAR upload: ${response}`);
  }

  const fileStream = fs.createReadStream(rarPath);
  let sentBytes = 0;
  const progressInterval = setInterval(() => {
    onProgress('Upload', sentBytes, fileSize, path.basename(rarPath));
  }, 1000);

  try {
    await new Promise((resolve, reject) => {
      fileStream.on('data', (chunk) => {
        if (cancel.value) {
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
        socket.end();
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

  onLog('Upload complete. Waiting for extraction response...');

  const extractResult = await new Promise((resolve, reject) => {
    let settled = false;
    let lineBuffer = '';
    const timeout = setTimeout(() => {
      socket.destroy();
      if (!settled) {
        settled = true;
        reject(new Error('Extraction timed out (no progress for 10m)'));
      }
    }, 600000);

    socket.on('data', (chunk) => {
      clearTimeout(timeout);
      lineBuffer += chunk.toString('utf8');
      let newlineIndex;
      while ((newlineIndex = lineBuffer.indexOf('\n')) !== -1) {
        const line = lineBuffer.substring(0, newlineIndex).trim();
        lineBuffer = lineBuffer.substring(newlineIndex + 1);

        if (line.startsWith('QUEUED ')) {
          const id = parseInt(line.split(' ')[1], 10) || 0;
          onLog(`Extraction queued (ID ${id}).`);
          socket.destroy();
          if (!settled) {
            settled = true;
            resolve({ queuedId: id, files: 0, bytes: 0 });
          }
          return;
        }
        if (line.startsWith('SUCCESS ')) {
          const parts = line.split(' ');
          const files = parseInt(parts[1], 10) || 0;
          const bytes = parseInt(parts[2], 10) || 0;
          socket.destroy();
          if (!settled) {
            settled = true;
            resolve({ files, bytes });
          }
          return;
        } else if (line.startsWith('ERROR: ')) {
          if (!settled) {
            settled = true;
            reject(new Error(`RAR extraction failed: ${line}`));
          }
          return;
        } else if (line.startsWith('EXTRACT_PROGRESS ')) {
          const parts = line.split(' ');
          const processed = parseInt(parts[2], 10) || 0;
          const total = parseInt(parts[3], 10) || 0;
          let currentFile = null;
          if (parts.length > 4) {
            currentFile = parts.slice(4).join(' ');
          }
          onProgress('Extract', processed, total, currentFile);
        } else if (line.startsWith('EXTRACTING ')) {
          const rest = line.substring('EXTRACTING '.length);
          const spaceIndex = rest.indexOf(' ');
          if (spaceIndex !== -1) {
            const count = rest.substring(0, spaceIndex);
            const filename = rest.substring(spaceIndex + 1);
            onLog(`Extracting (${count}): ${filename}`);
          }
        }
      }
      timeout.refresh();
    });

    socket.on('error', (err) => {
      if (!settled) {
        settled = true;
        reject(err);
      }
    });
    socket.on('close', () => {
      if (!settled) {
        settled = true;
        reject(new Error('Connection closed unexpectedly during extraction monitoring.'));
      }
    });
  });

  return { fileSize, ...extractResult };
}

function getTitleFromParam(param) {
  if (param && typeof param.titleName === 'string') {
    return param.titleName;
  }
  const localized = param && param.localizedParameters;
  if (!localized || typeof localized !== 'object') {
    return null;
  }
  let region = typeof localized.defaultLanguage === 'string' ? localized.defaultLanguage.trim() : '';
  if (!region) region = 'en-US';
  const normalized = region.replace('_', '-');
  const direct = localized[normalized] && localized[normalized].titleName;
  if (typeof direct === 'string') return direct;
  const fallback = localized['en-US'] && localized['en-US'].titleName;
  if (typeof fallback === 'string') return fallback;
  return null;
}

function parseGameMetaFromParam(param) {
  if (!param || typeof param !== 'object') return null;
  const title = getTitleFromParam(param) || 'Unknown';
  const title_id = typeof param.titleId === 'string' ? param.titleId : '';
  const content_id = typeof param.contentId === 'string' ? param.contentId : '';
  const version = typeof param.contentVersion === 'string' ? param.contentVersion : '';
  return { title, title_id, content_id, version };
}

function readJsonFile(pathname) {
  try {
    return JSON.parse(fs.readFileSync(pathname, 'utf8'));
  } catch {
    return null;
  }
}

function findParamPathForPath(sourcePath) {
  if (!sourcePath || !sourcePath.trim()) return null;
  let stat;
  try {
    stat = fs.statSync(sourcePath);
  } catch {
    return null;
  }
  if (stat.isDirectory()) {
    const sce = path.join(sourcePath, 'sce_sys', 'param.json');
    if (fs.existsSync(sce)) return sce;
    const direct = path.join(sourcePath, 'param.json');
    if (fs.existsSync(direct)) return direct;
    return null;
  }
  const parent = path.dirname(sourcePath);
  const stem = path.parse(sourcePath).name;
  if (stem) {
    const candidate = path.join(parent, stem);
    const sce = path.join(candidate, 'sce_sys', 'param.json');
    if (fs.existsSync(sce)) return sce;
    const direct = path.join(candidate, 'param.json');
    if (fs.existsSync(direct)) return direct;
  }
  return null;
}

function toRgbaPixels(image) {
  const { width, height } = image.getSize();
  const bgra = image.toBitmap();
  const rgba = Buffer.alloc(bgra.length);
  for (let i = 0; i < bgra.length; i += 4) {
    rgba[i] = bgra[i + 2];
    rgba[i + 1] = bgra[i + 1];
    rgba[i + 2] = bgra[i];
    rgba[i + 3] = bgra[i + 3];
  }
  return { pixels: Array.from(rgba), width, height };
}

function loadCoverImageFromPath(pathname, maxDim) {
  let image = nativeImage.createFromPath(pathname);
  if (image.isEmpty()) return null;
  const size = image.getSize();
  const maxSide = Math.max(size.width, size.height);
  if (maxSide > maxDim) {
    const scale = maxDim / maxSide;
    const width = Math.max(1, Math.round(size.width * scale));
    const height = Math.max(1, Math.round(size.height * scale));
    image = image.resize({ width, height, quality: 'best' });
  }
  return toRgbaPixels(image);
}

function loadCoverImageFromBytes(bytes, maxDim) {
  let image = nativeImage.createFromBuffer(bytes);
  if (image.isEmpty()) return null;
  const size = image.getSize();
  const maxSide = Math.max(size.width, size.height);
  if (maxSide > maxDim) {
    const scale = maxDim / maxSide;
    const width = Math.max(1, Math.round(size.width * scale));
    const height = Math.max(1, Math.round(size.height * scale));
    image = image.resize({ width, height, quality: 'best' });
  }
  return toRgbaPixels(image);
}

function findCoverPathForParam(paramPath) {
  const candidates = COVER_CANDIDATES;
  const isSceSys = path.basename(path.dirname(paramPath)).toLowerCase() === 'sce_sys';
  if (isSceSys) {
    const sceDir = path.dirname(paramPath);
    for (const name of candidates) {
      const c = path.join(sceDir, name);
      if (fs.existsSync(c)) return c;
    }
    const root = path.dirname(sceDir);
    for (const name of candidates) {
      const c = path.join(root, name);
      if (fs.existsSync(c)) return c;
    }
    return null;
  }
  for (const name of candidates) {
    const c = path.join(path.dirname(paramPath), 'sce_sys', name);
    if (fs.existsSync(c)) return c;
  }
  for (const name of candidates) {
    const c = path.join(path.dirname(paramPath), name);
    if (fs.existsSync(c)) return c;
  }
  return null;
}

async function loadGameMetaFromArchive(archivePath) {
  const entries = await listArchiveEntries(archivePath);
  if (!entries) return null;
  const paramEntry = findArchiveParamEntry(entries);
  if (!paramEntry) return null;
  const paramBytes = await extractArchiveFile(archivePath, paramEntry);
  if (!paramBytes) return null;
  let param;
  try {
    param = JSON.parse(paramBytes.toString('utf8'));
  } catch {
    return null;
  }
  const meta = parseGameMetaFromParam(param);
  if (!meta) return null;
  const coverEntry = findArchiveCoverEntry(entries, paramEntry);
  let cover = null;
  if (coverEntry) {
    const coverBytes = await extractArchiveFile(archivePath, coverEntry);
    if (coverBytes) {
      cover = loadCoverImageFromBytes(coverBytes, 160);
    }
  }
  return { meta, cover };
}

async function loadGameMetaForPath(sourcePath) {
  if (!sourcePath || !sourcePath.trim()) return { meta: null, cover: null };
  const ext = path.extname(sourcePath).toLowerCase();
  if (ARCHIVE_EXTENSIONS.has(ext)) {
    const archiveResult = await loadGameMetaFromArchive(sourcePath);
    if (archiveResult) return archiveResult;
    const fallbackParamPath = findParamPathForPath(sourcePath);
    if (fallbackParamPath) {
      const param = readJsonFile(fallbackParamPath);
      const meta = parseGameMetaFromParam(param);
      const coverPath = findCoverPathForParam(fallbackParamPath);
      const cover = coverPath ? loadCoverImageFromPath(coverPath, 160) : null;
      return { meta, cover };
    }
    return { meta: null, cover: null };
  }
  const paramPath = findParamPathForPath(sourcePath);
  if (!paramPath) return { meta: null, cover: null };
  const param = readJsonFile(paramPath);
  const meta = parseGameMetaFromParam(param);
  const coverPath = findCoverPathForParam(paramPath);
  const cover = coverPath ? loadCoverImageFromPath(coverPath, 160) : null;
  return { meta, cover };
}


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
  transferMeta: { requested_optimize: null, auto_tune_connections: null, effective_optimize: null, effective_compression: null },
  transferLastUpdate: 0,
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
  manageActiveOp: null,
  manageDoneEmitted: false,
  manageCancel: false,
  manageSocket: null,
  manageListCache: { path: '', entries: [], error: null, updated_at_ms: 0 },
  saveLogs: false,
  uiLogEnabled: true,
  chatSender: null,
};

let chatSocket = null;
let chatRoomId = 'LAN';
let chatEnabled = false;
const chatInstanceId = crypto.randomUUID();
const CHAT_PORT = 9234;

// Pollers
let payloadPoller = null;
let connectionPoller = null;
let managePoller = null;
let payloadAutoReloader = null;
let payloadAutoReloadInFlight = false;
let payloadAutoReloadLastAttempt = 0;

// Determine the environment
const isDev = !app.isPackaged;

const mainLogPath = () => path.join(app.getPath('userData'), 'ps5upload_main.log');
const logMain = (message, data = null) => {
  const stamp = new Date().toISOString();
  const payload = data ? ` ${JSON.stringify(data)}` : '';
  try {
    fs.appendFileSync(mainLogPath(), `[${stamp}] ${message}${payload}\n`);
  } catch {
    // ignore logging failures
  }
  console.error(message, data || '');
};

const resolveFaqPath = () => {
  if (isDev) {
    return path.join(__dirname, '..', '..', 'FAQ.md');
  }
  return path.join(process.resourcesPath, 'FAQ.md');
};

process.on('uncaughtException', (err) => {
  logMain('Uncaught exception', { message: err?.message, stack: err?.stack });
});

process.on('unhandledRejection', (err) => {
  logMain('Unhandled rejection', { message: err?.message, stack: err?.stack });
});

app.on('render-process-gone', (_event, webContents, details) => {
  logMain('Render process gone', { details, url: webContents?.getURL?.() });
});

app.on('child-process-gone', (_event, details) => {
  logMain('Child process gone', { details });
});

if (process.env.ELECTRON_DISABLE_GPU === '1') {
  app.disableHardwareAcceleration();
  app.commandLine.appendSwitch('disable-gpu');
}

app.commandLine.appendSwitch(
  'disable-features',
  'AutofillServerCommunication,AutofillEnable'
);

if (!isDev) {
  app.commandLine.appendSwitch('disable-devtools');
}

// Ensure ICU data is resolvable for packaged builds.
try {
  app.commandLine.appendSwitch('icu-data-dir', process.resourcesPath);
} catch {
  // ignore if not supported
}

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1440,
    height: 960,
    minWidth: 1024,
    minHeight: 720,
    frame: false,
    transparent: !IS_WINDOWS,
    backgroundColor: IS_WINDOWS ? '#0f172a' : '#00000000',
    resizable: true,
    maximizable: true,
    minimizable: true,
    fullscreenable: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.js'),
      contextIsolation: true,
      nodeIntegration: false,
      devTools: isDev,
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

function startChatSocket() {
  if (chatSocket) {
    return { room_id: chatRoomId, enabled: chatEnabled };
  }

  try {
    const socket = dgram.createSocket({ type: 'udp4', reuseAddr: true });
    chatSocket = socket;

    socket.on('error', (err) => {
      chatEnabled = false;
      emit('chat_status', { status: `Error: ${err.message}` });
    });

    socket.on('message', (msg) => {
      let payload = null;
      try {
        payload = JSON.parse(msg.toString('utf8'));
      } catch {
        return;
      }
      if (!payload || payload.instance_id === chatInstanceId) return;
      const time = payload.time || new Date().toLocaleTimeString();
      emit('chat_message', {
        time,
        sender: payload.name || 'User',
        text: payload.text || '',
        local: false,
      });
    });

    socket.bind(CHAT_PORT, () => {
      socket.setBroadcast(true);
      chatEnabled = true;
      emit('chat_status', { status: 'Connected' });
    });

    return { room_id: chatRoomId, enabled: true };
  } catch (err) {
    chatEnabled = false;
    emit('chat_status', { status: `Error: ${err.message}` });
    return { room_id: '', enabled: false };
  }
}

function stopChatSocket() {
  if (chatSocket) {
    try {
      chatSocket.close();
    } catch {
      // ignore
    }
    chatSocket = null;
  }
  chatEnabled = false;
  emit('chat_status', { status: 'Disconnected' });
  return { room_id: chatRoomId, enabled: false };
}

function sendChatMessage(name, text) {
  if (!chatSocket) {
    const info = startChatSocket();
    if (!info.enabled) {
      throw new Error('Chat unavailable');
    }
  }
  const payload = {
    instance_id: chatInstanceId,
    name,
    text,
    time: new Date().toLocaleTimeString(),
  };
  const buffer = Buffer.from(JSON.stringify(payload));
  chatSocket.send(buffer, 0, buffer.length, CHAT_PORT, '255.255.255.255');
  emit('chat_message', {
    time: payload.time,
    sender: name,
    text,
    local: true,
  });
  emit('chat_ack', { ok: true });
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

  setLimitBps(limitBps) {
    this.limitBps = limitBps;
  }

  async throttle(bytes) {
    if (!this.limitBps || this.limitBps <= 0) {
      return;
    }

    const now = process.hrtime.bigint();
    const elapsed = now - this.lastByteTime;
    this.lastByteTime = now;

    this.accruedDelay = this.accruedDelay > elapsed ? this.accruedDelay - elapsed : 0n;

    const delayNeeded = (BigInt(bytes) * 1_000_000_000n) / BigInt(this.limitBps);
    this.accruedDelay += delayNeeded;

    if (this.accruedDelay > 0) {
      const sleepMs = Number(this.accruedDelay / 1_000_000n);
      if (sleepMs > 0) {
        await new Promise(resolve => setTimeout(resolve, sleepMs));
      }
    }
  }
}

const sleepMs = (ms) => new Promise(resolve => setTimeout(resolve, ms));

const clamp = (value, min, max) => Math.min(max, Math.max(min, value));

const shouldDeprioritizeManage = () => state.transferActive;

async function manageDeprioritize() {
  if (shouldDeprioritizeManage()) {
    await sleepMs(5);
  }
}

function runManageTask(op, fn) {
  if (state.manageActive) {
    throw new Error('Another manage task is already running');
  }
  beginManageOperation(op);
  state.manageActive = true;
  state.manageCancel = false;
  setImmediate(async () => {
    try {
      await fn();
    } finally {
      state.manageActive = false;
      state.manageCancel = false;
      state.manageActiveOp = null;
    }
  });
  return true;
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

      while (offset < data.length) {
        const end = Math.min(offset + WRITE_CHUNK_SIZE, data.length);
        const chunk = data.slice(offset, end);
        const ok = socket.write(chunk);
        offset = end;
        if (!ok) { // Buffer full, wait for 'drain'
          socket.once('drain', writeLoop);
          return;
        }
      }
      resolve(); // All data written
    };
    writeLoop();
  });
}

// ==================== TCP Protocol ====================
const UPLOAD_SOCKET_BUFFER_SIZE = 8 * 1024 * 1024;

function tuneUploadSocket(socket) {
  if (!socket) return;
  socket.setNoDelay(true);
  socket.setKeepAlive(true, 1000);
  if (typeof socket.setSendBufferSize === 'function') {
    socket.setSendBufferSize(UPLOAD_SOCKET_BUFFER_SIZE);
  }
  if (typeof socket.setRecvBufferSize === 'function') {
    socket.setRecvBufferSize(UPLOAD_SOCKET_BUFFER_SIZE);
  }
}

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
  const MAX_RESPONSE_SIZE = 1024 * 1024; // 1MB limit
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;

    const cleanup = () => {
      socket.removeAllListeners();
      socket.destroy();
    };

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      if (resolved) return;
      data = Buffer.concat([data, chunk]);
      if (data.length > MAX_RESPONSE_SIZE) {
        resolved = true;
        cleanup();
        reject(new Error('Response too large'));
        return;
      }
      if (data.includes(Buffer.from('\n'))) {
        resolved = true;
        cleanup();
        resolve(data.toString('utf8').trim());
      }
    });

    socket.on('error', (err) => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(err);
    });

    socket.on('close', () => {
      if (resolved) return;
      resolved = true;
      if (data.length > 0) {
        resolve(data.toString('utf8').trim());
      }
    });

    socket.write(cmd);
  });
}

async function sendCommandWithPayload(ip, port, header, payload) {
  const socket = await createSocketWithTimeout(ip, port);
  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;

    const cleanup = () => {
      socket.removeAllListeners();
      socket.destroy();
    };

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      if (resolved) return;
      data = Buffer.concat([data, chunk]);
      if (data.includes(Buffer.from('\n'))) {
        resolved = true;
        cleanup();
        resolve(data.toString('utf8').trim());
      }
    });

    socket.on('error', (err) => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(err);
    });

    socket.write(header, () => {
      socket.write(payload);
    });
  });
}

async function sendCommandExpectPayload(ip, port, cmd) {
  const socket = await createSocketWithTimeout(ip, port);
  return new Promise((resolve, reject) => {
    let header = '';
    let body = Buffer.alloc(0);
    let expected = null;

    const cleanup = () => {
      socket.removeAllListeners();
      socket.destroy();
    };

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      cleanup();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      if (expected === null) {
        header += chunk.toString('utf8');
        const idx = header.indexOf('\n');
        if (idx === -1) return;
        const line = header.slice(0, idx).trim();
        const rest = header.slice(idx + 1);
        const parts = line.split(' ');
        if (parts[0] !== 'OK') {
          cleanup();
          reject(new Error(line || 'Invalid response'));
          return;
        }
        expected = parseInt(parts[1] || '0', 10) || 0;
        body = Buffer.from(rest, 'utf8');
        if (body.length >= expected) {
          cleanup();
          resolve(body.slice(0, expected).toString('utf8'));
        }
        return;
      }
      body = Buffer.concat([body, chunk]);
      if (body.length >= expected) {
        cleanup();
        resolve(body.slice(0, expected).toString('utf8'));
      }
    });

    socket.on('error', (err) => {
      cleanup();
      reject(err);
    });

    socket.write(cmd);
  });
}

async function listStorage(ip, port) {
  const MAX_RESPONSE_SIZE = 10 * 1024 * 1024; // 10MB limit for directory listings
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;

    const cleanup = () => {
      socket.removeAllListeners();
      socket.destroy();
    };

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      if (resolved) return;
      data = Buffer.concat([data, chunk]);
      if (data.length > MAX_RESPONSE_SIZE) {
        resolved = true;
        cleanup();
        reject(new Error('Response too large'));
        return;
      }
      const str = data.toString('utf8');
      if (str.includes('\n]\n') || str.endsWith('\n]')) {
        resolved = true;
        cleanup();
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

    socket.on('error', (err) => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(err);
    });

    socket.on('close', () => {
      if (resolved) return;
      resolved = true;
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
  const MAX_RESPONSE_SIZE = 10 * 1024 * 1024; // 10MB limit for directory listings
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;

    const cleanup = () => {
      socket.removeAllListeners();
      socket.destroy();
    };

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      if (resolved) return;
      data = Buffer.concat([data, chunk]);
      if (data.length > MAX_RESPONSE_SIZE) {
        resolved = true;
        cleanup();
        reject(new Error('Response too large'));
        return;
      }
      const str = data.toString('utf8');
      if (str.includes('\n]\n') || str.endsWith('\n]')) {
        resolved = true;
        cleanup();
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

    socket.on('error', (err) => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(err);
    });

    socket.on('close', () => {
      if (resolved) return;
      resolved = true;
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

async function hashFileLocal(filePath) {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash('sha256');
    const stream = fs.createReadStream(filePath);
    stream.on('data', chunk => hash.update(chunk));
    stream.on('error', reject);
    stream.on('end', () => resolve(hash.digest('hex')));
  });
}

function joinRemotePath(root, relPath) {
  if (!relPath) return root;
  if (root.endsWith('/')) return `${root}${relPath}`;
  return `${root}/${relPath}`;
}

async function mapWithConcurrency(items, limit, iterator) {
  const results = new Array(items.length);
  let nextIndex = 0;
  let active = 0;

  return new Promise((resolve, reject) => {
    const launch = () => {
      if (nextIndex >= items.length && active === 0) {
        resolve(results);
        return;
      }
      while (active < limit && nextIndex < items.length) {
        const idx = nextIndex++;
        active++;
        Promise.resolve(iterator(items[idx], idx))
          .then((result) => {
            results[idx] = result;
            active--;
            launch();
          })
          .catch(reject);
      }
    };
    launch();
  });
}

async function buildRemoteIndex(ip, port, destRoot, files, onProgress, onLog) {
  const dirSet = new Set();
  for (const file of files) {
    const rel = file.rel_path || '';
    let dir = path.posix.dirname(rel);
    if (dir === '.') dir = '';
    dirSet.add(dir);
  }
  const dirs = Array.from(dirSet);
  const total = dirs.length;
  let done = 0;

  const index = new Map();
  const listOne = async (dir) => {
    const remoteDir = joinRemotePath(destRoot, dir);
    try {
      const entries = await listDir(ip, port, remoteDir);
      for (const entry of entries || []) {
        if (!entry || entry.type !== 'file') continue;
        const relPath = dir ? `${dir}/${entry.name}` : entry.name;
        index.set(relPath.replace(/\\/g, '/'), {
          size: Number(entry.size) || 0,
          mtime: Number(entry.mtime) || 0
        });
      }
    } catch (err) {
      onLog?.(`Resume scan: failed to list ${remoteDir}: ${err.message || err}`);
    } finally {
      done += 1;
      if (onProgress) onProgress(done, total);
    }
  };

  const concurrency = 4;
  await mapWithConcurrency(dirs, concurrency, listOne);
  return index;
}

async function hashFileRemote(ip, port, filePath) {
  const response = await sendSimpleCommand(ip, port, `HASH_FILE ${filePath}\n`);
  if (response.startsWith('OK ')) {
    return response.substring(3).trim();
  }
  throw new Error(`Hash failed: ${response}`);
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
  const response = await sendSimpleCommand(ip, port, `DELETE_ASYNC ${filePath}\n`);
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
  try {
    const socket = await createSocketWithTimeout(ip, port, PAYLOAD_STATUS_CONNECT_TIMEOUT_MS);

    return await new Promise((resolve, reject) => {
      let data = Buffer.alloc(0);
      let headerParsed = false;
      let jsonSize = 0;

      socket.setTimeout(PAYLOAD_STATUS_READ_TIMEOUT_MS);
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
  } catch (err) {
    throw err;
  }
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

async function queueClearAll(ip, port) {
  const response = await sendSimpleCommand(ip, port, 'QUEUE_CLEAR_ALL\n');
  if (!response.startsWith('OK')) {
    throw new Error(`Queue clear failed: ${response}`);
  }
}

async function queueClearFailed(ip, port) {
  const response = await sendSimpleCommand(ip, port, 'QUEUE_CLEAR_FAILED\n');
  if (!response.startsWith('OK')) {
    throw new Error(`Queue clear failed: ${response}`);
  }
}

async function payloadReset(ip, port) {
  const response = await sendSimpleCommand(ip, port, 'RESET\n');
  if (!response.startsWith('OK')) {
    throw new Error(`Payload reset failed: ${response}`);
  }
}

async function payloadClearTmp(ip, port) {
  const response = await sendSimpleCommand(ip, port, 'CLEAR_TMP\n');
  if (!response.startsWith('OK')) {
    throw new Error(`Clear tmp failed: ${response}`);
  }
  return response.trim();
}

async function payloadMaintenance(ip, port) {
  const response = await sendSimpleCommand(ip, port, 'MAINTENANCE\n');
  if (response.startsWith('BUSY')) {
    return response.trim();
  }
  if (!response.startsWith('OK')) {
    throw new Error(`Maintenance failed: ${response}`);
  }
  return response.trim();
}

async function queueReorder(ip, port, ids) {
  if (!Array.isArray(ids) || ids.length === 0) {
    throw new Error('Queue reorder failed: empty list');
  }
  const list = ids.join(',');
  const response = await sendSimpleCommand(ip, port, `QUEUE_REORDER ${list}\n`);
  if (!response.startsWith('OK')) {
    throw new Error(`Queue reorder failed: ${response}`);
  }
}

async function queueProcess(ip, port) {
  const response = await sendSimpleCommand(ip, port, 'QUEUE_PROCESS\n');
  if (!response.startsWith('OK')) {
    throw new Error(`Queue start failed: ${response}`);
  }
}

async function queuePause(ip, port, id) {
  const response = await sendSimpleCommand(ip, port, `QUEUE_PAUSE ${id}\n`);
  if (!response.startsWith('OK')) {
    throw new Error(`Queue pause failed: ${response}`);
  }
}

async function queueRetry(ip, port, id) {
  const response = await sendSimpleCommand(ip, port, `QUEUE_RETRY ${id}\n`);
  if (!response.startsWith('OK')) {
    throw new Error(`Queue retry failed: ${response}`);
  }
}

async function syncInfo(ip, port) {
  const response = await sendCommandExpectPayload(ip, port, 'SYNC_INFO\n');
  return JSON.parse(response || '{}');
}

async function uploadQueueSync(ip, port, payload) {
  const header = `UPLOAD_QUEUE_SYNC ${Buffer.byteLength(payload)}\n`;
  const response = await sendCommandWithPayload(ip, port, header, payload);
  if (!response.startsWith('OK')) {
    throw new Error(`Upload queue sync failed: ${response}`);
  }
}

async function uploadQueueGet(ip, port) {
  return sendCommandExpectPayload(ip, port, 'UPLOAD_QUEUE_GET\n');
}

async function historySync(ip, port, payload) {
  const header = `HISTORY_SYNC ${Buffer.byteLength(payload)}\n`;
  const response = await sendCommandWithPayload(ip, port, header, payload);
  if (!response.startsWith('OK')) {
    throw new Error(`History sync failed: ${response}`);
  }
}

async function historyGet(ip, port) {
  return sendCommandExpectPayload(ip, port, 'HISTORY_GET\n');
}

async function uploadV2Init(ip, port, destPath, useTemp, opts = {}) {
  const socket = await createSocketWithTimeout(ip, port);
  const mode = useTemp ? 'TEMP' : 'DIRECT';
  tuneUploadSocket(socket);
  const flags = [];
  const {
    optimize_upload = false,
    chmod_after_upload = false,
  } = opts;
  if (optimize_upload || chmod_after_upload) {
    flags.push('NOCHMOD');
  }
  if (chmod_after_upload) {
    flags.push('CHMOD_END');
  }
  const flagStr = flags.length ? ` ${flags.join(' ')}` : '';

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

    socket.write(`UPLOAD_V2 ${destPath} ${mode}${flagStr}\n`);
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
  override_on_conflict: true,
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
        case 'resume_mode': {
          if (value === 'size_mtime') {
            config.resume_mode = 'size';
            break;
          }
          const allowed = ['size', 'hash_large', 'hash_medium', 'sha256'];
          config.resume_mode = allowed.includes(value) ? value : 'none';
          break;
        }
        case 'language': config.language = ['zh-CN', 'zh-TW', 'fr', 'es', 'ar', 'vi', 'hi', 'bn', 'pt-BR', 'ru', 'ja', 'tr', 'id', 'th', 'ko', 'de', 'it'].includes(value) ? value : 'en'; break;
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
    return { records: [], rev: 0, updated_at: 0 };
  }
  try {
    const data = JSON.parse(fs.readFileSync(historyPath, 'utf8'));
    return {
      records: Array.isArray(data.records) ? data.records : [],
      rev: typeof data.rev === 'number' ? data.rev : 0,
      updated_at: data.updated_at || 0
    };
  } catch {
    return { records: [], rev: 0, updated_at: 0 };
  }
}

function saveHistory(data) {
  ensureDir(getAppDataDir());
  const current = loadHistory();
  const rev = typeof data.rev === 'number' ? data.rev : (current.rev || 0) + 1;
  const payload = {
    records: Array.isArray(data.records) ? data.records : [],
    rev,
    updated_at: data.updated_at || Date.now()
  };
  fs.writeFileSync(getHistoryPath(), JSON.stringify(payload, null, 2));
}

function addHistoryRecord(record) {
  const data = loadHistory();
  data.records.push(record);
  data.updated_at = Date.now();
  saveHistory(data);
}

function clearHistory() {
  saveHistory({ records: [], updated_at: Date.now() });
}

// ==================== Queue Management ====================

function loadQueue() {
  const queuePath = getQueuePath();
  if (!fs.existsSync(queuePath)) {
    return { items: [], next_id: 1, rev: 0, updated_at: 0 };
  }
  try {
    const data = JSON.parse(fs.readFileSync(queuePath, 'utf8'));
    return {
      items: Array.isArray(data.items) ? data.items : [],
      next_id: data.next_id || 1,
      rev: typeof data.rev === 'number' ? data.rev : 0,
      updated_at: data.updated_at || 0
    };
  } catch {
    return { items: [], next_id: 1, rev: 0, updated_at: 0 };
  }
}

function saveQueue(data) {
  ensureDir(getAppDataDir());
  const current = loadQueue();
  const rev = typeof data.rev === 'number' ? data.rev : (current.rev || 0) + 1;
  const payload = {
    items: Array.isArray(data.items) ? data.items : [],
    next_id: data.next_id || 1,
    rev,
    updated_at: data.updated_at || Date.now()
  };
  fs.writeFileSync(getQueuePath(), JSON.stringify(payload, null, 2));
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

async function collectFiles(basePath, cancel = { value: false }, progressCallback = null) {
  const files = [];
  let totalSize = 0n;

  const stat = await fs.promises.stat(basePath);
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

  const walk = async (dir, prefix = '') => {
    if (cancel.value) return;
    const entries = await fs.promises.readdir(dir, { withFileTypes: true });
    for (const entry of entries) {
      if (cancel.value) return;
      const fullPath = path.join(dir, entry.name);
      const relPath = prefix ? `${prefix}/${entry.name}` : entry.name;

      if (entry.isDirectory()) {
        await walk(fullPath, relPath);
      } else if (entry.isFile()) {
        const stat = await fs.promises.stat(fullPath);
        files.push({
          rel_path: relPath.replace(/\\/g, '/'),
          abs_path: fullPath,
          size: Number(stat.size),
          mtime: Math.floor(stat.mtimeMs / 1000),
        });
        totalSize += BigInt(stat.size);

        if (progressCallback && files.length % 100 === 0) { // Update more frequently
          progressCallback(files.length, Number(totalSize));
        }
      }
    }
  };

  await walk(basePath);
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

function createAdaptiveUploadTuner({ ip, basePackLimit, baseChunkSize, userRateLimitBps, log }) {
  const state = {
    packLimit: basePackLimit,
    chunkSize: baseChunkSize,
    paceMs: 0,
    rateLimitBps: userRateLimitBps,
    stableTicks: 0,
    lastBackpressureEvents: 0,
    lastBackpressureWaitMs: 0,
    lastMode: '',
  };

  let timer = null;

  const applyStatus = (status) => {
    if (!status || !status.transfer) return;
    const transfer = status.transfer || {};
    const queueCount = Number(transfer.queue_count || 0);
    const packInUse = Number(transfer.pack_in_use || 0);
    const backpressureEvents = Number(transfer.backpressure_events || 0);
    const backpressureWaitMs = Number(transfer.backpressure_wait_ms || 0);
    const deltaEvents = backpressureEvents - state.lastBackpressureEvents;
    const deltaWait = backpressureWaitMs - state.lastBackpressureWaitMs;
    state.lastBackpressureEvents = backpressureEvents;
    state.lastBackpressureWaitMs = backpressureWaitMs;

    let pressure = 0;
    if (queueCount >= 256) pressure += 2;
    else if (queueCount >= 64) pressure += 1;
    if (packInUse >= 64) pressure += 1;
    if (deltaEvents > 0 || deltaWait > 200) pressure += 1;
    if (typeof transfer.last_progress === 'number' && transfer.last_progress > 0) {
      const ageSec = Math.max(0, Date.now() / 1000 - transfer.last_progress);
      if (ageSec > 5) pressure += 1;
    }

    if (pressure >= 2) {
      state.packLimit = clamp(Math.floor(state.packLimit * 0.5), PACK_BUFFER_MIN, basePackLimit);
      state.paceMs = clamp(state.paceMs + 5, 5, 50);
      state.stableTicks = 0;
    } else if (pressure === 1) {
      state.paceMs = clamp(Math.max(state.paceMs, 5), 5, 30);
      state.packLimit = clamp(state.packLimit, PACK_BUFFER_MIN, basePackLimit);
      state.stableTicks = 0;
    } else {
      state.stableTicks += 1;
      if (state.stableTicks >= 2) {
        state.paceMs = clamp(state.paceMs - 5, 0, 20);
        state.packLimit = clamp(state.packLimit + 1 * 1024 * 1024, PACK_BUFFER_MIN, basePackLimit);
      }
    }

    const nextMode = `${state.packLimit}-${state.paceMs}`;
    if (nextMode !== state.lastMode) {
      log(
        `Adaptive tune: pack=${(state.packLimit / (1024 * 1024)).toFixed(1)} MB, pace=${state.paceMs}ms ` +
        `(queue=${queueCount}, pack=${packInUse}, backpressure+${Math.max(deltaEvents, 0)})`
      );
      state.lastMode = nextMode;
    }
  };

  const poll = async () => {
    if (!ip) return;
    try {
      const status = await getPayloadStatus(ip, TRANSFER_PORT);
      applyStatus(status);
    } catch (err) {
      // Ignore transient status errors
    }
  };

  const start = () => {
    timer = setInterval(poll, ADAPTIVE_POLL_MS);
    if (timer.unref) timer.unref();
    poll();
  };

  const stop = () => {
    if (timer) clearInterval(timer);
    timer = null;
  };

  return {
    start,
    stop,
    getPackLimit: () => state.packLimit,
    getPaceDelayMs: () => state.paceMs,
    getRateLimitBps: () => state.rateLimitBps,
    getChunkSize: () => state.chunkSize,
  };
}

async function sendFilesV2(files, socket, options = {}) {
  const {
    cancel = { value: false },
    progress = () => {},
    log = () => {},
    compression = 'none',
    rateLimitBps = null,
    deprioritize = false,
    packLimitBytes = PACK_BUFFER_SIZE,
    streamChunkBytes = SEND_CHUNK_SIZE,
    getPackLimit,
    getPaceDelayMs,
    getRateLimitBps,
  } = options;

  let totalSentBytes = 0n;
  let totalSentFiles = 0;
  const startTime = Date.now();
  let limiter = new RateLimiter(rateLimitBps);
  const packLimitFn = typeof getPackLimit === 'function'
    ? getPackLimit
    : () => packLimitBytes;
  const paceDelayFn = typeof getPaceDelayMs === 'function'
    ? getPaceDelayMs
    : () => 0;
  const rateLimitFn = typeof getRateLimitBps === 'function'
    ? getRateLimitBps
    : () => rateLimitBps;

  const applyAdaptiveRate = () => {
    const nextLimit = rateLimitFn();
    if (typeof limiter.setLimitBps === 'function') {
      limiter.setLimitBps(nextLimit);
    } else {
      limiter.limitBps = nextLimit;
    }
  };

  const maybePace = async () => {
    const delayMs = paceDelayFn();
    if (delayMs > 0) {
      await sleepMs(delayMs);
    }
  };

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

    applyAdaptiveRate();
    await sendFrameHeader(socket, frameType, payload.length, cancel);
    await writeAllRetry(socket, payload, cancel);
    limiter.throttle(payload.length); // Apply rate limit to compressed/actual sent bytes
    if (deprioritize) {
      await manageDeprioritize();
    }
    await maybePace();

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

  const logEachFile = files.length <= 10000;
  const logInterval = logEachFile ? 1 : Math.max(250, Math.floor(files.length / 200));

  for (let i = 0; i < files.length; i++) {
    const file = files[i];
    if (cancel.value) {
      throw new Error('Upload cancelled by user');
    }

    if (logEachFile || i % logInterval === 0) {
      log(logEachFile ? `Packing: ${file.rel_path}` : `Packing... ${i + 1}/${files.length}`);
    }

    const relPathBytes = Buffer.from(file.rel_path, 'utf8');
    let sawData = false;
    const stream = fs.createReadStream(file.abs_path, { highWaterMark: streamChunkBytes });

    for await (const chunk of stream) {
      if (cancel.value) {
        stream.destroy();
        throw new Error('Upload cancelled by user');
      }

      sawData = true;
      let offset = 0;

      while (offset < chunk.length) {
        const overhead = 2 + relPathBytes.length + 8;
        const packLimit = clamp(packLimitFn(), PACK_BUFFER_MIN, PACK_BUFFER_SIZE);
        const remaining = packLimit - packBuffer.length;

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
        if (deprioritize) {
          await manageDeprioritize();
        }
        await maybePace();
      }
    }

    if (!sawData) {
      const overhead = 2 + relPathBytes.length + 8;
      const packLimit = clamp(packLimitFn(), PACK_BUFFER_MIN, PACK_BUFFER_SIZE);
      if (packLimit - packBuffer.length <= overhead) {
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
  const MAX_RESPONSE_SIZE = 1024 * 1024; // 1MB limit
  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;

    const cleanup = () => {
      socket.removeListener('data', onData);
      socket.removeListener('error', onError);
      socket.removeListener('close', onClose);
      clearTimeout(timeout);
    };

    const timeout = setTimeout(() => {
      if (resolved) return;
      resolved = true;
      cleanup();
      socket.destroy();
      reject(new Error('Read timeout'));
    }, 60000);

    const onData = (chunk) => {
      if (resolved) return;
      data = Buffer.concat([data, chunk]);
      if (data.length > MAX_RESPONSE_SIZE) {
        resolved = true;
        cleanup();
        socket.destroy();
        reject(new Error('Response too large'));
        return;
      }
      const str = data.toString('utf8');
      if (str.includes('\n')) {
        resolved = true;
        cleanup();
        resolve(str.trim());
      }
    };

    const onError = (err) => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(err);
    };

    const onClose = () => {
      if (resolved) return;
      resolved = true;
      cleanup();
      if (data.length > 0) {
        resolve(data.toString('utf8').trim());
      } else {
        reject(new Error('Connection closed'));
      }
    };

    socket.on('data', onData);
    socket.on('error', onError);
    socket.on('close', onClose);
  });
}

function parseUploadResponse(response) {
  if (response.startsWith('OK ') || response.startsWith('SUCCESS ')) {
    const parts = response.split(/\s+/);
    if (parts.length >= 3) {
      return { files: parseInt(parts[1], 10), bytes: parseInt(parts[2], 10) };
    }
    return { files: 0, bytes: 0 };
  }
  throw new Error(response);
}

function createSocketReader(socket) {
  let buffer = Buffer.alloc(0);
  let ended = false;
  let error = null;
  const waiters = new Set();

  const notify = () => {
    for (const waiter of Array.from(waiters)) {
      waiter();
    }
  };

  const onData = (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    notify();
  };
  const onError = (err) => {
    error = err;
    notify();
  };
  const onClose = () => {
    ended = true;
    notify();
  };

  socket.on('data', onData);
  socket.on('error', onError);
  socket.on('close', onClose);
  socket.on('end', onClose);

  const awaitCondition = (predicate, timeoutMs) => new Promise((resolve, reject) => {
    if (error) return reject(error);
    if (predicate()) return resolve();
    if (ended) return reject(new Error('Connection closed'));

    const waiter = () => {
      if (error) {
        cleanup();
        reject(error);
        return;
      }
      if (predicate()) {
        cleanup();
        resolve();
        return;
      }
      if (ended) {
        cleanup();
        reject(new Error('Connection closed'));
      }
    };

    let timeout = null;
    if (timeoutMs) {
      timeout = setTimeout(() => {
        cleanup();
        reject(new Error('Read timeout'));
      }, timeoutMs);
    }

    const cleanup = () => {
      waiters.delete(waiter);
      if (timeout) clearTimeout(timeout);
    };

    waiters.add(waiter);
  });

  return {
    readExact: async (length, timeoutMs) => {
      if (length === 0) return Buffer.alloc(0);
      await awaitCondition(() => buffer.length >= length, timeoutMs);
      const out = buffer.slice(0, length);
      buffer = buffer.slice(length);
      return out;
    },
    readLine: async (timeoutMs, maxBytes = 1024 * 1024) => {
      await awaitCondition(() => buffer.indexOf(0x0a) !== -1 || buffer.length > maxBytes, timeoutMs);
      const idx = buffer.indexOf(0x0a);
      if (idx === -1) {
        throw new Error('Line too long');
      }
      const line = buffer.slice(0, idx).toString('utf8');
      buffer = buffer.slice(idx + 1);
      return line;
    },
    close: () => {
      socket.removeListener('data', onData);
      socket.removeListener('error', onError);
      socket.removeListener('close', onClose);
      socket.removeListener('end', onClose);
      waiters.clear();
    }
  };
}

async function readFtxFrame(reader, timeoutMs = READ_TIMEOUT_MS) {
  const header = await reader.readExact(16, timeoutMs);
  const magic = header.readUInt32LE(0);
  if (magic !== MAGIC_FTX1) {
    const err = new Error(`Invalid frame magic: ${magic.toString(16)}`);
    err.code = 'FTX_UNSUPPORTED';
    throw err;
  }
  const type = header.readUInt32LE(4);
  const len = Number(header.readBigUInt64LE(8));
  if (!Number.isFinite(len) || len < 0 || len > 64 * 1024 * 1024) {
    throw new Error(`Invalid frame length: ${len}`);
  }
  const body = len > 0 ? await reader.readExact(len, timeoutMs) : Buffer.alloc(0);
  return { type, body };
}

async function decodePackFrame(frameType, body) {
  if (frameType === FrameType.Pack) {
    return body;
  }
  if (frameType === FrameType.PackLz4) {
    if (!lz4) throw new Error('lz4 module unavailable');
    if (body.length < 4) throw new Error('Invalid LZ4 frame');
    const rawLen = body.readUInt32LE(0);
    const compressed = body.slice(4);
    const out = Buffer.alloc(rawLen);
    const decoded = lz4.decodeBlock(compressed, out);
    if (decoded < 0) throw new Error('LZ4 decode failed');
    return decoded === rawLen ? out : out.slice(0, decoded);
  }
  if (frameType === FrameType.PackZstd) {
    if (!fzstd) throw new Error('fzstd module unavailable');
    if (body.length < 4) throw new Error('Invalid ZSTD frame');
    const rawLen = body.readUInt32LE(0);
    const compressed = body.slice(4);
    const out = Buffer.alloc(rawLen);
    const result = fzstd.decompress(compressed, out);
    return Buffer.from(result.buffer, result.byteOffset, result.byteLength);
  }
  if (frameType === FrameType.PackLzma) {
    if (!lzma) throw new Error('lzma-native module unavailable');
    if (body.length < 17) throw new Error('Invalid LZMA frame');
    const props = body.slice(4, 9);
    const raw64 = body.slice(9, 17);
    const compressed = body.slice(17);
    const lzmaBuffer = Buffer.concat([props, raw64, compressed]);
    const result = await lzma.decompress(lzmaBuffer);
    return Buffer.isBuffer(result) ? result : Buffer.from(result);
  }
  throw new Error(`Unsupported frame type ${frameType}`);
}

function parsePackRecords(packBuffer, onRecord) {
  if (packBuffer.length < 4) {
    throw new Error('Invalid pack payload');
  }
  const records = packBuffer.readUInt32LE(0);
  let offset = 4;
  for (let i = 0; i < records; i++) {
    if (offset + 2 > packBuffer.length) throw new Error('Invalid pack record header');
    const pathLen = packBuffer.readUInt16LE(offset);
    offset += 2;
    if (offset + pathLen + 8 > packBuffer.length) throw new Error('Invalid pack record path');
    const relPath = packBuffer.slice(offset, offset + pathLen).toString('utf8');
    offset += pathLen;
    const dataLen = Number(packBuffer.readBigUInt64LE(offset));
    offset += 8;
    if (offset + dataLen > packBuffer.length) throw new Error('Invalid pack record data');
    const data = packBuffer.slice(offset, offset + dataLen);
    offset += dataLen;
    onRecord(relPath, data);
  }
}

function ensureDownloadPath(baseDir, relPath) {
  const root = path.resolve(baseDir);
  const target = path.resolve(baseDir, relPath);
  if (target !== root && !target.startsWith(root + path.sep)) {
    throw new Error('Invalid download path');
  }
  return target;
}

async function downloadFtx(ip, command, options) {
  const {
    mode,
    destPath,
    cancel = { value: false },
    progress = () => {},
    expectedRelPath = null,
  } = options;

  let socket = null;
  let reader = null;
  let currentPath = null;
  let currentRelPath = null;
  let currentStream = null;
  let received = 0;
  let lastActivity = Date.now();

  const closeStream = async () => {
    if (currentStream) {
      await new Promise((resolve) => currentStream.end(resolve));
      currentStream = null;
      currentRelPath = null;
      currentPath = null;
    }
  };

  const openStream = (relPath) => {
    if (mode === 'file') {
      if (expectedRelPath && relPath && relPath !== expectedRelPath) {
        throw new Error(`Unexpected file path: ${relPath}`);
      }
      if (currentPath !== destPath) {
        currentPath = destPath;
        currentRelPath = relPath;
        currentStream = fs.createWriteStream(destPath, { flags: 'w' });
      }
      return;
    }

    const target = ensureDownloadPath(destPath, relPath);
    if (currentPath !== target) {
      currentPath = target;
      currentRelPath = relPath;
      fs.mkdirSync(path.dirname(target), { recursive: true });
      currentStream = fs.createWriteStream(target, { flags: 'w' });
    }
  };

  try {
    socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
    socket.setTimeout(0);
    state.manageSocket = socket;
    reader = createSocketReader(socket);

    socket.write(command);
    const line = (await reader.readLine(30000)).trim();
    if (line.startsWith('ERROR')) {
      const err = new Error(line);
      if (line.includes('Unknown command')) err.code = 'FTX_UNSUPPORTED';
      throw err;
    }
    const match = line.match(/^READY\s+(\d+)(?:\s+COMP\s+\w+)?/i);
    if (!match) {
      const err = new Error(`Unexpected response: ${line}`);
      err.code = 'FTX_UNSUPPORTED';
      throw err;
    }
    const totalSize = Number(match[1]);
    if (mode === 'dir') {
      fs.mkdirSync(destPath, { recursive: true });
    }
    progress(0, totalSize, null);

    while (true) {
      if (cancel.value) {
        const err = new Error('Download cancelled');
        err.code = 'CANCELLED';
        throw err;
      }
      const timeout = received >= totalSize ? 5000 : READ_TIMEOUT_MS;
      let frame;
      try {
        frame = await readFtxFrame(reader, timeout);
      } catch (err) {
        if (
          received >= totalSize &&
          (err.message === 'Read timeout' || err.message === 'Connection closed')
        ) {
          break;
        }
        throw err;
      }
      if (frame.type === FrameType.Finish) {
        break;
      }
      if (frame.type === FrameType.Error) {
        throw new Error(frame.body.toString('utf8') || 'Download failed');
      }
      const pack = await decodePackFrame(frame.type, frame.body);
      const records = [];
      parsePackRecords(pack, (relPath, data) => records.push({ relPath, data }));
      for (const record of records) {
        if (!record.relPath) throw new Error('Invalid record path');
        if (record.relPath !== currentRelPath && currentStream) {
          await closeStream();
        }
        openStream(record.relPath);
        if (record.data.length > 0) {
          if (!currentStream.write(record.data)) {
            await new Promise((resolve) => currentStream.once('drain', resolve));
          }
        }
        received += record.data.length;
        lastActivity = Date.now();
        progress(received, totalSize, record.relPath);
      }
    }

    if (received < totalSize && Date.now() - lastActivity > 5000) {
      throw new Error(`Download incomplete: ${received}/${totalSize}`);
    }
    await closeStream();
    reader.close();
    return { bytes: received };
  } catch (err) {
    await closeStream();
    if (reader) reader.close();
    if (socket) socket.destroy();
    throw err;
  } finally {
    if (state.manageSocket === socket) {
      state.manageSocket = null;
    }
  }
}

// ==================== Update Checking ====================

async function fetchUrl(url, maxRedirects = 5) {
  if (maxRedirects <= 0) {
    throw new Error('Too many redirects');
  }
  return new Promise((resolve, reject) => {
    const protocol = url.startsWith('https') ? https : http;
    const req = protocol.get(url, {
      headers: { 'User-Agent': 'ps5upload-desktop' }
    }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        fetchUrl(res.headers.location, maxRedirects - 1).then(resolve).catch(reject);
        return;
      }

      let data = '';
      const MAX_SIZE = 10 * 1024 * 1024; // 10MB limit
      res.on('data', (chunk) => {
        data += chunk;
        if (data.length > MAX_SIZE) {
          req.destroy();
          reject(new Error('Response too large'));
        }
      });
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
  const MAX_REDIRECTS = 5;
  const DOWNLOAD_TIMEOUT = 300000; // 5 minutes

  return new Promise((resolve, reject) => {
    let file = fs.createWriteStream(destPath);
    let currentReq = null;
    let redirectCount = 0;
    let resolved = false;

    const cleanup = (deleteFile = false) => {
      if (currentReq) {
        currentReq.destroy();
        currentReq = null;
      }
      if (file) {
        file.close();
        file = null;
      }
      if (deleteFile) {
        try { fs.unlinkSync(destPath); } catch (_) {}
      }
    };

    const makeRequest = (requestUrl) => {
      if (resolved) return;

      if (++redirectCount > MAX_REDIRECTS) {
        resolved = true;
        cleanup(true);
        reject(new Error('Too many redirects'));
        return;
      }

      const reqProtocol = requestUrl.startsWith('https') ? https : http;
      currentReq = reqProtocol.get(requestUrl, {
        headers: { 'User-Agent': 'ps5upload-desktop' }
      }, (res) => {
        if (resolved) return;

        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          // Follow redirect - destroy current request first
          res.destroy();
          makeRequest(res.headers.location);
          return;
        }

        if (res.statusCode !== 200) {
          resolved = true;
          cleanup(true);
          reject(new Error(`HTTP ${res.statusCode}`));
          return;
        }

        res.pipe(file);
        file.on('finish', () => {
          if (resolved) return;
          resolved = true;
          cleanup(false);
          resolve();
        });

        file.on('error', (err) => {
          if (resolved) return;
          resolved = true;
          cleanup(true);
          reject(err);
        });
      });

      currentReq.on('error', (err) => {
        if (resolved) return;
        resolved = true;
        cleanup(true);
        reject(err);
      });

      currentReq.setTimeout(DOWNLOAD_TIMEOUT, () => {
        if (resolved) return;
        resolved = true;
        cleanup(true);
        reject(new Error('Download timeout'));
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
    socket.on('error', (err) => {
      reject(err);
      socket.destroy();
    });

    socket.write(fileContent, (err) => {
      if (err) {
        reject(err);
        socket.destroy();
        return;
      }
      
      // The data is now in the OS's buffer. Given that the user
      // has confirmed the payload is being received and executed,
      // we can resolve the promise here to provide a responsive UI.
      // This assumes the OS will handle the rest of the transfer reliably.
      resolve(fileContent.length);

      // We still need to end the socket to signal the end of the data.
      socket.end();
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

// Mutex flags to prevent overlapping poller calls
let payloadPollerRunning = false;
let connectionPollerRunning = false;
let managePollerRunning = false;

function startPayloadPoller() {
  if (payloadPoller) return;

  payloadPoller = setInterval(async () => {
    if (payloadPollerRunning) return;
    if (!state.payloadPollEnabled || !state.payloadIp) return;

    payloadPollerRunning = true;
    try {
      const status = await getPayloadStatus(state.payloadIp, TRANSFER_PORT);
      state.payloadStatus = { status, error: null, updated_at_ms: Date.now() };
      emit('payload_status_update', state.payloadStatus);
    } catch (err) {
      state.payloadStatus = { status: null, error: err.message, updated_at_ms: Date.now() };
      emit('payload_status_update', state.payloadStatus);
      tryAutoReloadPayload();
    } finally {
      payloadPollerRunning = false;
    }
  }, 1000);
}

async function tryAutoReloadPayload() {
  if (!state.payloadAutoReloadEnabled || !state.payloadIp) return;
  if (payloadAutoReloadInFlight) return;
  const now = Date.now();
  if (now - payloadAutoReloadLastAttempt < 15000) return;

  payloadAutoReloadInFlight = true;
  payloadAutoReloadLastAttempt = now;
  emit('payload_busy', { busy: true });

  try {
    try {
      await getPayloadVersion(state.payloadIp, TRANSFER_PORT);
      emit('payload_busy', { busy: false });
      return;
    } catch {
      // not running; continue to reload
    }

    const portOpen = await checkPort(state.payloadIp, PAYLOAD_PORT);
    if (!portOpen) {
      emit('payload_log', { message: `Auto reload failed: port ${PAYLOAD_PORT} closed.` });
      return;
    }

    emit('payload_log', { message: 'Auto reloading payload...' });
    if (state.payloadAutoReloadMode === 'local') {
      if (!state.payloadAutoReloadPath || !state.payloadAutoReloadPath.trim()) {
        emit('payload_log', { message: 'Auto reload failed: local payload path not set.' });
        return;
      }
      await sendPayloadFile(state.payloadIp, state.payloadAutoReloadPath);
    } else {
      const fetch = state.payloadAutoReloadMode === 'latest' ? 'latest' : 'current';
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
      if (!asset) {
        emit('payload_log', { message: 'Auto reload failed: payload asset not found.' });
        return;
      }

      const tmpPath = path.join(os.tmpdir(), `ps5upload_autoreload.elf`);
      await downloadAsset(asset.browser_download_url, tmpPath);
      await sendPayloadFile(state.payloadIp, tmpPath);
    }

    emit('payload_log', { message: 'Auto reload complete. Waiting for status...' });
    setTimeout(() => {
      if (state.payloadIp) {
        getPayloadVersion(state.payloadIp, TRANSFER_PORT)
          .then((version) => emit('payload_version', { version, error: null }))
          .catch((err) => emit('payload_version', { version: null, error: err.message }));
      }
    }, 3000);
  } catch (err) {
    emit('payload_log', { message: `Auto reload error: ${err.message || String(err)}` });
  } finally {
    emit('payload_busy', { busy: false });
    payloadAutoReloadInFlight = false;
  }
}

function startConnectionPoller() {
  if (connectionPoller) return;

  connectionPoller = setInterval(async () => {
    if (connectionPollerRunning) return;
    if (!state.connectionPollEnabled || !state.connectionAutoEnabled || !state.connectionIp) return;

    connectionPollerRunning = true;
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
    } finally {
      connectionPollerRunning = false;
    }
  }, 5000);
}

function startManagePoller() {
  if (managePoller) return;

  managePoller = setInterval(async () => {
    if (managePollerRunning) return;
    if (!state.managePollEnabled || !state.manageIp || !state.managePath) return;

    managePollerRunning = true;
    try {
      const entries = await listDir(state.manageIp, TRANSFER_PORT, state.managePath);
      state.manageListCache = { path: state.managePath, entries, error: null, updated_at_ms: Date.now() };
      emit('manage_list_update', state.manageListCache);
    } catch (err) {
      state.manageListCache = { path: state.managePath, entries: [], error: err.message, updated_at_ms: Date.now() };
      emit('manage_list_update', state.manageListCache);
    } finally {
      managePollerRunning = false;
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
  ipcMain.handle('app_platform', () => ({ platform: process.platform, arch: process.arch }));

  // Window controls
  ipcMain.handle('window_minimize', () => mainWindow?.minimize());
  ipcMain.handle('window_maximize', () => mainWindow?.isMaximized() ? mainWindow.unmaximize() : mainWindow?.maximize());
  ipcMain.handle('window_close', () => mainWindow?.close());
  ipcMain.handle('open_external', async (_, url) => {
    if (typeof url !== 'string' || url.trim().length === 0) {
      throw new Error('Invalid URL');
    }
    let parsed;
    try {
      parsed = new URL(url);
    } catch {
      throw new Error('Invalid URL');
    }
    if (!['http:', 'https:'].includes(parsed.protocol)) {
      throw new Error('Unsupported URL protocol');
    }
    await shell.openExternal(url);
    return true;
  });

  // Dialogs
  ipcMain.handle('dialog_open', async (_, options) => {
    const dialogOptions = { ...options };
    if (dialogOptions.directory) {
      dialogOptions.properties = dialogOptions.properties || [];
      dialogOptions.properties.push('openDirectory');
      delete dialogOptions.directory;
    }
    const result = await dialog.showOpenDialog(mainWindow, dialogOptions);
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
  ipcMain.handle('history_save', (_, data) => { saveHistory(data); return true; });
  ipcMain.handle('history_add', (_, record) => { addHistoryRecord(record); return true; });
  ipcMain.handle('history_clear', () => { clearHistory(); return true; });

  // Logging
  ipcMain.handle('set_save_logs', (_, enabled) => { state.saveLogs = enabled; return true; });
  ipcMain.handle('set_ui_log_enabled', (_, enabled) => { state.uiLogEnabled = enabled; return true; });
  ipcMain.handle('faq_load', () => {
    const faqPath = resolveFaqPath();
    try {
      return fs.readFileSync(faqPath, 'utf8');
    } catch (err) {
      logMain('FAQ load failed', { path: faqPath, message: err?.message || String(err) });
      throw new Error('FAQ not available.');
    }
  });

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

  ipcMain.handle('payload_queue_clear_all', async (_, ip) => {
    try {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return queueClearAll(ip, TRANSFER_PORT);
  } catch (err) {
    throw err;
  }
  });

  ipcMain.handle('payload_queue_clear_failed', async (_, ip) => {
    try {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return queueClearFailed(ip, TRANSFER_PORT);
  } catch (err) {
    throw err;
  }
  });

  ipcMain.handle('payload_reset', async (_, ip) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return payloadReset(ip, TRANSFER_PORT);
  });

  ipcMain.handle('payload_clear_tmp', async (_, ip) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return payloadClearTmp(ip, TRANSFER_PORT);
  });

  ipcMain.handle('payload_maintenance', async (_, ip) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return payloadMaintenance(ip, TRANSFER_PORT);
  });

  ipcMain.handle('payload_queue_reorder', async (_, ip, ids) => {
    try {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return queueReorder(ip, TRANSFER_PORT, ids);
  } catch (err) {
    throw err;
  }
  });

  ipcMain.handle('payload_queue_process', async (_, ip) => {
    try {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return queueProcess(ip, TRANSFER_PORT);
  } catch (err) {
    throw err;
  }
  });

  ipcMain.handle('payload_queue_pause', async (_, ip, id) => {
    try {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return queuePause(ip, TRANSFER_PORT, id);
  } catch (err) {
    throw err;
  }
  });

  ipcMain.handle('payload_queue_retry', async (_, ip, id) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return queueRetry(ip, TRANSFER_PORT, id);
  });

  ipcMain.handle('payload_sync_info', async (_, ip) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return syncInfo(ip, TRANSFER_PORT);
  });

  ipcMain.handle('payload_upload_queue_get', async (_, ip) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return uploadQueueGet(ip, TRANSFER_PORT);
  });

  ipcMain.handle('payload_upload_queue_sync', async (_, ip, payload) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return uploadQueueSync(ip, TRANSFER_PORT, payload || '');
  });

  ipcMain.handle('payload_history_get', async (_, ip) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return historyGet(ip, TRANSFER_PORT);
  });

  ipcMain.handle('payload_history_sync', async (_, ip, payload) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return historySync(ip, TRANSFER_PORT, payload || '');
  });

  ipcMain.handle('sleep_set', (_, enabled) => {
    if (enabled) {
      if (sleepBlockerId == null || !powerSaveBlocker.isStarted(sleepBlockerId)) {
        sleepBlockerId = powerSaveBlocker.start('prevent-display-sleep');
      }
      return { enabled: true };
    }
    if (sleepBlockerId != null && powerSaveBlocker.isStarted(sleepBlockerId)) {
      powerSaveBlocker.stop(sleepBlockerId);
    }
    sleepBlockerId = null;
    return { enabled: false };
  });

  ipcMain.handle('sleep_status', () => {
    if (sleepBlockerId != null && powerSaveBlocker.isStarted(sleepBlockerId)) {
      return { enabled: true };
    }
    return { enabled: false };
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
  ipcMain.handle('manage_cancel', () => {
    state.manageCancel = true;
    if (state.manageSocket) {
      try {
        state.manageSocket.destroy();
      } catch {
        // ignore
      }
    }
    if (state.manageActive && !state.manageDoneEmitted) {
      emitManageDone({
        op: state.manageActiveOp || 'Manage',
        bytes: null,
        error: 'Cancelled by user'
      });
      state.manageActive = false;
      state.manageActiveOp = null;
    }
    return true;
  });

  ipcMain.handle('manage_delete', async (_, ip, filepath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!filepath || !filepath.trim()) throw new Error('Select a path to delete.');

    return runManageTask('Delete', async () => {
      emit('manage_log', { message: `Delete ${filepath}` });
      let tick = 0;
      const timer = setInterval(() => {
        tick += 1;
        emit('manage_progress', { op: 'Delete', processed: tick, total: 0, current_file: filepath });
      }, 1000);
      emit('manage_progress', { op: 'Delete', processed: 1, total: 0, current_file: filepath });
      try {
        await deletePath(ip, TRANSFER_PORT, filepath);
        emitManageDone({ op: 'Delete', bytes: null, error: null });
      } catch (err) {
        emitManageDone({ op: 'Delete', bytes: null, error: err.message });
      } finally {
        clearInterval(timer);
      }
    });
  });

  ipcMain.handle('manage_rename', async (_, ip, srcPath, dstPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!srcPath || !srcPath.trim() || !dstPath || !dstPath.trim()) throw new Error('Source and destination are required.');

    beginManageOperation('Rename');
    emit('manage_log', { message: `Rename ${srcPath} -> ${dstPath}` });
    try {
      await movePath(ip, TRANSFER_PORT, srcPath, dstPath);
      emitManageDone({ op: 'Rename', bytes: null, error: null });
    } catch (err) {
      emitManageDone({ op: 'Rename', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_create_dir', async (_, ip, dirPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!dirPath || !dirPath.trim()) throw new Error('Folder path is required.');

    beginManageOperation('Create');
    emit('manage_log', { message: `Create folder ${dirPath}` });
    try {
      await createPath(ip, TRANSFER_PORT, dirPath);
      emitManageDone({ op: 'Create', bytes: null, error: null });
    } catch (err) {
      emitManageDone({ op: 'Create', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_chmod', async (_, ip, filepath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!filepath || !filepath.trim()) throw new Error('Select a path.');

    beginManageOperation('chmod');
    emit('manage_log', { message: `chmod 777 ${filepath}` });
    try {
      await chmod777(ip, TRANSFER_PORT, filepath);
      emitManageDone({ op: 'chmod', bytes: null, error: null });
    } catch (err) {
      emitManageDone({ op: 'chmod', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_move', async (_, ip, srcPath, dstPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!srcPath || !srcPath.trim() || !dstPath || !dstPath.trim()) throw new Error('Source and destination are required.');

    beginManageOperation('Move');
    emit('manage_log', { message: `Move ${srcPath} -> ${dstPath}` });
    try {
      await movePath(ip, TRANSFER_PORT, srcPath, dstPath);
      emitManageDone({ op: 'Move', bytes: null, error: null });
    } catch (err) {
      emitManageDone({ op: 'Move', bytes: null, error: err.message });
    }
    return true;
  });

  ipcMain.handle('manage_copy', async (_, ip, srcPath, dstPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!srcPath || !srcPath.trim() || !dstPath || !dstPath.trim()) throw new Error('Source and destination are required.');

    return runManageTask('Copy', async () => {
      emit('manage_log', { message: `Copy ${srcPath} -> ${dstPath}` });
      emit('manage_progress', { op: 'Copy', processed: 0, total: 0, current_file: null });
      try {
        await copyWithProgress(
          ip,
          srcPath,
          dstPath,
          { get value() { return state.manageCancel; } },
          (processed, total) => {
            emit('manage_progress', { op: 'Copy', processed, total, current_file: null });
          },
          (message) => emit('manage_log', { message })
        );
        emitManageDone({ op: 'Copy', bytes: null, error: null });
      } catch (err) {
        emitManageDone({ op: 'Copy', bytes: null, error: err.message });
        throw err;
      }
    });
  });

  ipcMain.handle('manage_extract', async (_, ip, srcPath, dstPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!srcPath || !srcPath.trim() || !dstPath || !dstPath.trim()) throw new Error('Source and destination are required.');

    return runManageTask('Extract', async () => {
      emit('manage_log', { message: `Extract ${srcPath} -> ${dstPath}` });
      try {
        await extractArchiveWithProgress(
          ip,
          srcPath,
          dstPath,
          { get value() { return state.manageCancel; } },
          (processed, total, currentFile) => {
            emit('manage_progress', { op: 'Extract', processed, total, current_file: currentFile });
          },
          (message) => emit('manage_log', { message })
        );
        emitManageDone({ op: 'Extract', bytes: null, error: null });
      } catch (err) {
        emitManageDone({ op: 'Extract', bytes: null, error: err.message });
      }
    });
  });

  ipcMain.handle('manage_download_file', async (_, ip, filepath, destPath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!filepath || !filepath.trim() || !destPath || !destPath.trim()) throw new Error('Source and destination are required.');

    return runManageTask('Download', async () => {
      emit('manage_log', { message: `Download ${filepath}` });
      let totalSize = 0;

      try {
        const result = await downloadFtx(ip, `DOWNLOAD_V2 ${filepath}\n`, {
          mode: 'file',
          destPath,
          expectedRelPath: path.basename(filepath),
          cancel: { get value() { return state.manageCancel; } },
          progress: (processed, total, currentFile) => {
            totalSize = total;
            emit('manage_progress', { op: 'Download', processed, total, current_file: currentFile });
          },
        });
        emit('manage_progress', { op: 'Download', processed: totalSize, total: totalSize, current_file: null });
        emitManageDone({ op: 'Download', bytes: result.bytes, error: null });
      } catch (err) {
        if (err && err.code === 'FTX_UNSUPPORTED') {
          try {
            const bytes = await downloadFileLegacy(ip, filepath, destPath);
            emitManageDone({ op: 'Download', bytes, error: null });
          } catch (legacyErr) {
            emitManageDone({ op: 'Download', bytes: null, error: legacyErr.message });
          }
          return;
        }
        // Clean up partial file on error
        try {
          fs.unlinkSync(destPath);
        } catch {
          // ignore cleanup errors
        }
        emitManageDone({ op: 'Download', bytes: null, error: err.message });
      }
    });
  });

  // Helper: recursively list all files in a remote directory
  async function listDirRecursive(ip, port, remotePath, basePath = '') {
    const entries = await listDir(ip, port, remotePath);
    let files = [];
    for (const entry of entries) {
      const entryPath = remotePath + '/' + entry.name;
      const relPath = basePath ? basePath + '/' + entry.name : entry.name;
      if (entry.type === 'd' || entry.is_dir) {
        const subFiles = await listDirRecursive(ip, port, entryPath, relPath);
        files = files.concat(subFiles);
      } else if (entry.type === 'f' || entry.is_file || entry.type === '-') {
        files.push({ remotePath: entryPath, relPath, size: entry.size || 0 });
      }
    }
    return files;
  }

  // Helper: download a single file with progress
  async function downloadSingleFile(ip, remotePath, localPath, onProgress) {
    const socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
    socket.setTimeout(0);

    return new Promise((resolve, reject) => {
      let headerBuf = Buffer.alloc(0);
      let headerDone = false;
      let totalSize = 0;
      let received = 0;
      let fileStream = null;
      let lastProgress = Date.now();
      let stallTimer = null;

      const cleanup = (err) => {
        if (stallTimer) {
          clearInterval(stallTimer);
          stallTimer = null;
        }
        socket.removeAllListeners();
        if (fileStream) {
          fileStream.end();
        }
        socket.destroy();
        if (err) {
          // Clean up partial file
          try { fs.unlinkSync(localPath); } catch (e) { /* ignore */ }
          reject(err);
        } else {
          resolve(received);
        }
      };

      socket.write(`DOWNLOAD_RAW ${remotePath}\n`);

      socket.on('data', (chunk) => {
        if (state.manageCancel) {
          cleanup(new Error('Download cancelled'));
          return;
        }
        if (!headerDone) {
          headerBuf = Buffer.concat([headerBuf, chunk]);
          const idx = headerBuf.indexOf('\n');
          if (idx === -1) return;
          const line = headerBuf.slice(0, idx).toString('utf8').trim();
          const remainder = headerBuf.slice(idx + 1);
          headerBuf = Buffer.alloc(0);
          if (line.startsWith('ERROR')) {
            cleanup(new Error(line));
            return;
          }
          const match = line.match(/^READY\s+(\d+)/i);
          if (!match) {
            cleanup(new Error(`Unexpected response: ${line}`));
            return;
          }
          totalSize = Number(match[1]);
          headerDone = true;
          fileStream = fs.createWriteStream(localPath, { flags: 'w' });
          if (remainder.length > 0) {
            received += remainder.length;
            fileStream.write(remainder);
          }
        } else {
          received += chunk.length;
          if (!fileStream.write(chunk)) {
            socket.pause();
            fileStream.once('drain', () => socket.resume());
          }
        }
        lastProgress = Date.now();
        if (onProgress) onProgress(received, totalSize);
      });

      socket.on('error', (err) => cleanup(err));
      socket.on('close', () => {
        if (headerDone && received >= totalSize) {
          cleanup();
        } else if (headerDone) {
          cleanup(new Error(`Incomplete: ${received}/${totalSize}`));
        } else {
          cleanup(new Error('Connection closed before response'));
        }
      });
      socket.on('end', () => {
        if (headerDone && received >= totalSize) {
          cleanup();
        }
      });

      stallTimer = setInterval(() => {
        if (Date.now() - lastProgress > 60000) {
          cleanup(new Error('Download stalled'));
        }
      }, 1000);
    });
  }

  async function downloadFileLegacy(ip, filepath, destPath) {
    let socket = null;
    let fileStream = null;
    let totalSize = 0;
    let received = 0;

    try {
      socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
      socket.setTimeout(0); // Disable timeout, rely on stall detection
      state.manageSocket = socket;
      socket.write(`DOWNLOAD_RAW ${filepath}\n`);

      fileStream = fs.createWriteStream(destPath, { flags: 'w' });
      let headerBuf = Buffer.alloc(0);
      let headerDone = false;
      let lastProgress = Date.now();
      let lastLogAt = 0;

      await new Promise((resolve, reject) => {
        let stallTimer = null;
        const cleanup = (err) => {
          if (stallTimer) {
            clearInterval(stallTimer);
            stallTimer = null;
          }
          socket.removeAllListeners();
          if (err) {
            reject(err);
          } else {
            resolve();
          }
        };

        const writeChunk = (buf) => {
          if (buf.length === 0) return;
          if (!fileStream.write(buf)) {
            socket.pause();
            fileStream.once('drain', () => socket.resume());
          }
        };

        socket.on('data', (chunk) => {
          if (state.manageCancel) {
            cleanup(new Error('Download cancelled by user'));
            return;
          }
          if (!headerDone) {
            headerBuf = Buffer.concat([headerBuf, chunk]);
            const idx = headerBuf.indexOf('\n');
            if (idx === -1) return;
            const line = headerBuf.slice(0, idx).toString('utf8').trim();
            const remainder = headerBuf.slice(idx + 1);
            headerBuf = Buffer.alloc(0);
            if (line.startsWith('ERROR')) {
              cleanup(new Error(line));
              return;
            }
            const match = line.match(/^READY\s+(\d+)/i);
            if (!match) {
              cleanup(new Error(`Unexpected response: ${line}`));
              return;
            }
            totalSize = Number(match[1]);
            headerDone = true;
            emit('manage_progress', { op: 'Download', processed: 0, total: totalSize, current_file: filepath });
            if (remainder.length > 0) {
              received += remainder.length;
              writeChunk(remainder);
            }
          } else {
            received += chunk.length;
            writeChunk(chunk);
          }
          lastProgress = Date.now();
          emit('manage_progress', { op: 'Download', processed: received, total: totalSize, current_file: null });
          const now = Date.now();
          if (now - lastLogAt > 5000) {
            lastLogAt = now;
            emit('manage_log', { message: `Downloading... ${received}/${totalSize}` });
          }
        });

        socket.on('timeout', () => cleanup(new Error('Download timed out')));
        socket.on('error', (err) => cleanup(err));
        socket.on('close', () => cleanup());
        socket.on('end', () => cleanup());

        stallTimer = setInterval(() => {
          if (!headerDone) {
            // Allow 30 seconds for header
            if (Date.now() - lastProgress > 30000) {
              cleanup(new Error('Download timed out waiting for response'));
            }
            return;
          }
          // Allow 120 seconds of no data before stall
          if (Date.now() - lastProgress > 120000) {
            cleanup(new Error(`Download stalled at ${received}/${totalSize}`));
          }
        }, 1000);
      });

      await new Promise((resolve) => fileStream.end(resolve));
      if (received < totalSize) {
        throw new Error(`Download incomplete: ${received}/${totalSize}`);
      }
      emit('manage_progress', { op: 'Download', processed: totalSize, total: totalSize, current_file: null });
      return received;
    } catch (err) {
      if (fileStream) {
        fileStream.end();
      }
      if (socket) {
        socket.destroy();
      }
      // Clean up partial file on error
      try {
        fs.unlinkSync(destPath);
      } catch {
        // Ignore cleanup errors
      }
      throw err;
    } finally {
      if (state.manageSocket === socket) {
        state.manageSocket = null;
      }
    }
  }

  async function downloadDirLegacy(ip, dirPath, destPath) {
    // Step 1: List all files recursively
    emit('manage_log', { message: 'Scanning directory...' });
    const files = await listDirRecursive(ip, TRANSFER_PORT, dirPath);

    if (files.length === 0) {
      await fs.promises.mkdir(destPath, { recursive: true });
      emit('manage_progress', { op: 'Download', processed: 0, total: 0, current_file: null });
      return { bytes: 0, files: 0 };
    }

    const totalBytes = files.reduce((sum, f) => sum + (f.size || 0), 0);
    emit('manage_log', { message: `Found ${files.length} files (${totalBytes} bytes)` });
    emit('manage_progress', { op: 'Download', processed: 0, total: totalBytes, current_file: null });

    // Step 2: Download each file
    let downloadedBytes = 0;
    let downloadedFiles = 0;

    for (const file of files) {
      if (state.manageCancel) {
        throw new Error('Download cancelled by user');
      }

      const localFilePath = path.join(destPath, file.relPath);
      const localDir = path.dirname(localFilePath);
      await fs.promises.mkdir(localDir, { recursive: true });

      emit('manage_progress', {
        op: 'Download',
        processed: downloadedBytes,
        total: totalBytes,
        current_file: file.relPath
      });

      const bytes = await downloadSingleFile(ip, file.remotePath, localFilePath, (received) => {
        emit('manage_progress', {
          op: 'Download',
          processed: downloadedBytes + received,
          total: totalBytes,
          current_file: file.relPath
        });
      });

      downloadedBytes += bytes;
      downloadedFiles++;
    }

    emit('manage_progress', { op: 'Download', processed: downloadedBytes, total: totalBytes, current_file: null });
    emit('manage_log', { message: `Downloaded ${downloadedFiles} files` });
    return { bytes: downloadedBytes, files: downloadedFiles };
  }

  async function extractArchiveWithProgress(ip, srcPath, dstPath, cancel, onProgress, onLog) {
    const socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
    socket.setTimeout(0);
    state.manageSocket = socket;

    return new Promise((resolve, reject) => {
      let settled = false;
      let lineBuffer = '';
      let cancelledSent = false;
      let lastActivity = Date.now();

      const cleanup = (err) => {
        if (settled) return;
        settled = true;
        clearInterval(timer);
        socket.removeAllListeners();
        socket.destroy();
        if (err) {
          reject(err);
        } else {
          resolve(true);
        }
      };

      const timer = setInterval(() => {
        if (cancel?.value && !cancelledSent) {
          cancelledSent = true;
          try { socket.write('CANCEL\n'); } catch { /* ignore */ }
        }
        if (Date.now() - lastActivity > 600000) {
          cleanup(new Error('Extraction timed out (no progress for 10m)'));
        }
      }, 1000);

      socket.on('data', (chunk) => {
        lastActivity = Date.now();
        lineBuffer += chunk.toString('utf8');
        let newlineIndex;
        while ((newlineIndex = lineBuffer.indexOf('\n')) !== -1) {
          const line = lineBuffer.substring(0, newlineIndex).trim();
          lineBuffer = lineBuffer.substring(newlineIndex + 1);

          if (line.startsWith('OK')) {
            cleanup();
            return;
          }
          if (line.startsWith('ERROR')) {
            cleanup(new Error(line));
            return;
          }
          if (line.startsWith('EXTRACT_PROGRESS ')) {
            const parts = line.split(' ');
            const processed = parseInt(parts[2], 10) || 0;
            const total = parseInt(parts[3], 10) || 0;
            let currentFile = null;
            if (parts.length > 4) {
              currentFile = parts.slice(4).join(' ');
            }
            onProgress(processed, total, currentFile);
            continue;
          }
          if (line.startsWith('EXTRACTING ')) {
            onLog(line);
          }
        }
      });

      socket.on('error', (err) => cleanup(err));
      socket.on('close', () => {
        if (!settled) {
          cleanup(new Error('Connection closed unexpectedly during extraction.'));
        }
      });

      socket.write(`EXTRACT_ARCHIVE ${srcPath}\t${dstPath}\n`);
    }).finally(() => {
      if (state.manageSocket === socket) {
        state.manageSocket = null;
      }
    });
  }

  async function copyWithProgress(ip, srcPath, dstPath, cancel, onProgress, onLog) {
    const socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
    socket.setTimeout(0);
    state.manageSocket = socket;

    return new Promise((resolve, reject) => {
      let settled = false;
      let lineBuffer = '';
      let cancelledSent = false;
      let lastActivity = Date.now();

      const cleanup = (err) => {
        if (settled) return;
        settled = true;
        clearInterval(timer);
        socket.removeAllListeners();
        socket.destroy();
        if (err) {
          reject(err);
        } else {
          resolve(true);
        }
      };

      const timer = setInterval(() => {
        if (cancel?.value && !cancelledSent) {
          cancelledSent = true;
          try { socket.write('CANCEL\n'); } catch { /* ignore */ }
        }
        if (Date.now() - lastActivity > 600000) {
          cleanup(new Error('Copy timed out (no progress for 10m)'));
        }
      }, 1000);

      socket.on('data', (chunk) => {
        lastActivity = Date.now();
        lineBuffer += chunk.toString('utf8');
        let newlineIndex;
        while ((newlineIndex = lineBuffer.indexOf('\n')) !== -1) {
          const line = lineBuffer.substring(0, newlineIndex).trim();
          lineBuffer = lineBuffer.substring(newlineIndex + 1);

          if (line.startsWith('OK')) {
            cleanup();
            return;
          }
          if (line.startsWith('ERROR')) {
            cleanup(new Error(line));
            return;
          }
          if (line.startsWith('COPY_PROGRESS ')) {
            const parts = line.split(' ');
            const processed = parseInt(parts[1], 10) || 0;
            const total = parseInt(parts[2], 10) || 0;
            onProgress(processed, total, null);
            continue;
          }
          if (line.length > 0) {
            onLog(line);
          }
        }
      });

      socket.on('error', (err) => cleanup(err));
      socket.on('close', () => {
        if (!settled) {
          cleanup(new Error('Connection closed unexpectedly during copy.'));
        }
      });

      socket.write(`COPY ${srcPath}\t${dstPath}\n`);
    }).finally(() => {
      if (state.manageSocket === socket) {
        state.manageSocket = null;
      }
    });
  }

  ipcMain.handle('manage_download_dir', async (_, ip, dirPath, destPath, compression) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!dirPath || !dirPath.trim() || !destPath || !destPath.trim()) throw new Error('Source and destination are required.');

    return runManageTask('Download', async () => {
      emit('manage_log', { message: `Download ${dirPath}` });
      let totalSize = 0;
      const comp = (compression || 'none').toLowerCase();
      let compTag = '';
      if (comp === 'lz4') compTag = ' LZ4';
      else if (comp === 'zstd') compTag = ' ZSTD';
      else if (comp === 'lzma') compTag = ' LZMA';
      else if (comp === 'auto') compTag = ' AUTO';

      try {
        const result = await downloadFtx(ip, `DOWNLOAD_DIR ${dirPath}${compTag}\n`, {
          mode: 'dir',
          destPath,
          cancel: { get value() { return state.manageCancel; } },
          progress: (processed, total, currentFile) => {
            totalSize = total;
            emit('manage_progress', { op: 'Download', processed, total, current_file: currentFile });
          },
        });
        emit('manage_progress', { op: 'Download', processed: totalSize, total: totalSize, current_file: null });
        emitManageDone({ op: 'Download', bytes: result.bytes, error: null });
      } catch (err) {
        if (err && err.code === 'FTX_UNSUPPORTED') {
          try {
            const result = await downloadDirLegacy(ip, dirPath, destPath);
            emitManageDone({ op: 'Download', bytes: result.bytes, error: null });
          } catch (legacyErr) {
            emitManageDone({ op: 'Download', bytes: null, error: legacyErr.message });
          }
          return;
        }
        emitManageDone({ op: 'Download', bytes: null, error: err.message });
      }
    });
  });

  ipcMain.handle('manage_upload', async (_, ip, destRoot, paths) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!destRoot || !destRoot.trim()) throw new Error('Destination path is required.');
    if (!paths || paths.length === 0) throw new Error('Select at least one file or folder.');

    return runManageTask('Upload', async () => {
      emit('manage_log', { message: 'Upload started.' });

      try {
        let totalBytes = 0;
        let totalFiles = 0;
        const batches = [];

        for (const srcPath of paths) {
          if (state.manageCancel) {
            throw new Error('Upload cancelled by user');
          }
          const stat = fs.statSync(srcPath);
          let files;
          let dest;

          if (stat.isDirectory()) {
            const folderName = path.basename(srcPath);
            dest = `${destRoot.replace(/\/$/, '')}/${folderName}`;
            const result = await collectFiles(srcPath);
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
          totalFiles += files.length;
          batches.push({ dest, files, batchBytes });
        }

        let processedBase = 0;
        emit('manage_progress', { op: 'Upload', processed: 0, total: totalBytes, current_file: null });

        for (const batch of batches) {
          if (state.manageCancel) {
            throw new Error('Upload cancelled by user');
          }

          const socket = await uploadV2Init(ip, TRANSFER_PORT, batch.dest, false);
          await sendFilesV2(batch.files, socket, {
            cancel: { get value() { return state.manageCancel; } },
            deprioritize: true,
            progress: (sent) => {
              emit('manage_progress', { op: 'Upload', processed: processedBase + sent, total: totalBytes, current_file: null });
            },
            log: (msg) => emit('manage_log', { message: msg }),
          });

          const response = await readUploadResponse(socket);
          parseUploadResponse(response);
          processedBase += batch.batchBytes;
          emit('manage_progress', { op: 'Upload', processed: processedBase, total: totalBytes, current_file: null });
        }

        emitManageDone({ op: 'Upload', bytes: totalBytes, files: totalFiles, error: null });
      } catch (err) {
        emitManageDone({ op: 'Upload', bytes: null, error: err.message });
      }
    });
  });

  // Upload RAR for server-side extraction
  ipcMain.handle('manage_upload_rar', async (_, ip, rarPath, destPath, mode, tempRoot) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!rarPath || !rarPath.trim()) throw new Error('RAR file path is required.');
    if (!destPath || !destPath.trim()) throw new Error('Destination path is required.');

    beginManageOperation('Extract');
    emit('manage_log', { message: `Uploading RAR ${rarPath} for extraction to ${destPath}` });

    try {
      const extractResult = await uploadRarForExtraction(ip, rarPath, destPath, mode, {
        cancel: { get value() { return state.manageCancel; } },
        tempRoot,
        onProgress: (op, processed, total, currentFile) => {
          emit('manage_progress', {
            op: op === 'Upload' ? 'Extract' : 'Extract',
            processed,
            total,
            current_file: currentFile
          });
        },
        onLog: (message) => emit('manage_log', { message })
      });
      if (extractResult.queuedId) {
        emit('queue_hint', {
          queue_id: extractResult.queuedId,
          source_path: rarPath,
          dest_path: destPath,
          size_bytes: extractResult.fileSize
        });
      }

      const bytes = typeof extractResult.bytes === 'number' ? extractResult.bytes : extractResult.fileSize || 0;
      emitManageDone({ op: 'Extract', bytes, error: null });
      return true;
    } catch (err) {
      emitManageDone({ op: 'Extract', bytes: null, error: err.message });
      throw err;
    }
  });

  // Transfer
  ipcMain.handle('transfer_check_dest', async (_, ip, destPath) => {
    return checkDir(ip, TRANSFER_PORT, destPath);
  });

  ipcMain.handle('transfer_scan', async (_, args) => {
    state.scanCancel = false;
    
    try {
      const sourcePath = typeof args === 'string' ? args : args?.source_path;
      const maxMs = typeof args?.max_ms === 'number' ? args.max_ms : 8000;
      const maxFiles = typeof args?.max_files === 'number' ? args.max_files : 50000;
      const quickCount = !!args?.quick_count;
      const sampleLimit = typeof args?.sample_limit === 'number' ? args.sample_limit : 400;
      if (!sourcePath || !String(sourcePath).trim()) {
        throw new Error('Source path is required.');
      }

      let stat;
      try {
        stat = await fs.promises.stat(sourcePath);
      } catch (err) {
        const code = err && err.code ? err.code : 'error';
        throw new Error(`Scan failed to access path: ${sourcePath} (${code})`);
      }

      if (!stat.isDirectory()) {
        throw new Error('Scan supports folders only.');
      }

      const { files, totalSize, partial, reason, elapsedMs, estimated } = await new Promise((resolve, reject) => {
        let filesFound = 0;
        let currentTotalSize = 0;
        let ended = false;
        let partialScan = false;
        let stopReason = null;
        let estimatedSize = false;
        let sampleCount = 0;
        let sampleSizeSum = 0;
        const startedAt = Date.now();

        const finish = () => {
          const elapsedMs = Date.now() - startedAt;
          const finalTotal = estimatedSize
            ? (sampleCount > 0 ? Math.round((sampleSizeSum / sampleCount) * filesFound) : 0)
            : currentTotalSize;
          resolve({
            files: filesFound,
            totalSize: finalTotal,
            partial: partialScan,
            reason: stopReason,
            elapsedMs,
            estimated: estimatedSize
          });
        };

        const stopEarly = (reason) => {
          if (ended) return;
          ended = true;
          partialScan = true;
          stopReason = reason;
          finish();
        };

        const shouldStop = () => {
          if (maxMs > 0 && Date.now() - startedAt >= maxMs) return 'time';
          if (maxFiles > 0 && filesFound >= maxFiles) return 'files';
          return null;
        };

        const withTimeout = (promise, ms) => new Promise((resolve, reject) => {
          const timer = setTimeout(() => resolve(null), ms);
          promise
            .then((value) => {
              clearTimeout(timer);
              resolve(value);
            })
            .catch((err) => {
              clearTimeout(timer);
              reject(err);
            });
        });

        const walkQuick = async () => {
          const stack = [sourcePath];
          while (stack.length > 0) {
            if (state.scanCancel) return reject(new Error('Scan cancelled'));
            const stop = shouldStop();
            if (stop) return stopEarly(stop);
            const dir = stack.pop();
            if (!dir) continue;
            let entries;
            try {
              entries = await withTimeout(
                fs.promises.readdir(dir, { withFileTypes: true }),
                2000
              );
            } catch {
              continue;
            }
            if (!entries) {
              partialScan = true;
              stopReason = stopReason ?? 'time';
              continue;
            }
            for (const entry of entries) {
              if (state.scanCancel) return reject(new Error('Scan cancelled'));
              const stopInner = shouldStop();
              if (stopInner) return stopEarly(stopInner);
              const fullPath = path.join(dir, entry.name);
              if (entry.isDirectory()) {
                stack.push(fullPath);
              } else if (entry.isFile()) {
                filesFound++;
                if (sampleCount < sampleLimit) {
                  try {
                    const st = await fs.promises.stat(fullPath);
                    sampleCount++;
                    sampleSizeSum += st.size;
                    currentTotalSize += st.size;
                  } catch {
                    // ignore stat failures
                  }
                }
                emit('scan_progress', { files: filesFound, total: currentTotalSize });
              }
            }
          }
          estimatedSize = true;
          finish();
        };

        const walkFull = async () => {
          const stack = [sourcePath];
          while (stack.length > 0) {
            if (state.scanCancel) throw new Error('Scan cancelled');
            const stop = shouldStop();
            if (stop) {
              partialScan = true;
              stopReason = stop;
              return;
            }
            const dir = stack.pop();
            if (!dir) continue;
            let entries;
            try {
              entries = await fs.promises.readdir(dir, { withFileTypes: true });
            } catch {
              continue;
            }
            for (const entry of entries) {
              if (state.scanCancel) throw new Error('Scan cancelled');
              const stopInner = shouldStop();
              if (stopInner) {
                partialScan = true;
                stopReason = stopInner;
                return;
              }
              const fullPath = path.join(dir, entry.name);
              if (entry.isDirectory()) {
                stack.push(fullPath);
              } else if (entry.isFile()) {
                try {
                  const st = await fs.promises.stat(fullPath);
                  filesFound++;
                  currentTotalSize += st.size;
                  emit('scan_progress', { files: filesFound, total: currentTotalSize });
                } catch {
                  // ignore stat failures
                }
              }
            }
          }
        };

        if (quickCount) {
          estimatedSize = true;
          walkQuick().catch(reject);
        } else {
          walkFull()
            .then(() => finish())
            .catch(reject);
        }
      });

      emit('scan_complete', { files, total: totalSize, partial, reason, elapsed_ms: elapsedMs, estimated });
      return { files, total: totalSize, partial, reason, elapsed_ms: elapsedMs, estimated };
    } catch (err) {
      emit('scan_error', { message: err.message });
      throw err;
    }
  });

  ipcMain.handle('transfer_scan_cancel', () => {
    state.scanCancel = true;
    return true;
  });

  ipcMain.handle('transfer_cancel', () => {
    state.transferCancel = true;
    state.transferStatus = { ...state.transferStatus, status: 'Cancelled' };
    state.transferLastUpdate = Date.now();
    return true;
  });

  ipcMain.handle('transfer_status', () => ({ ...state.transferStatus, ...state.transferMeta }));
  ipcMain.handle('transfer_reset', () => {
    state.transferCancel = false;
    state.transferActive = false;
    state.transferStatus = { run_id: 0, status: 'Idle', sent: 0, total: 0, files: 0, elapsed_secs: 0, current_file: '' };
    state.transferMeta = { requested_optimize: null, auto_tune_connections: null, effective_optimize: null, effective_compression: null };
    state.transferLastUpdate = Date.now();
    return true;
  });
  ipcMain.handle('transfer_active', () => state.transferActive);

  ipcMain.handle('transfer_start', async (_, req) => {
    if (req && req.req) {
      req = req.req;
    }
    if (!req || typeof req !== 'object') {
      throw new Error('Transfer request missing.');
    }
    logMain('transfer_start request', {
      ip: req.ip,
      source_path: req.source_path,
      dest_path: req.dest_path,
      connections: req.connections,
      compression: req.compression,
    });
    if (!req.ip || !req.ip.trim()) throw new Error('PS5 IP address is required');
    if (!req.source_path || !req.source_path.trim()) throw new Error('Source path is required');
    if (!req.dest_path || !req.dest_path.trim()) throw new Error('Destination path is required');

    if (state.transferActive) {
      const status = state.transferStatus?.status || '';
      const isTerminal = status.startsWith('Complete') || status.startsWith('Cancelled') || status.startsWith('Error') || status.startsWith('Idle');
      const stale = state.transferLastUpdate > 0 && Date.now() - state.transferLastUpdate > 30000;
      if (isTerminal || stale) {
        state.transferActive = false;
      } else {
        throw new Error('Transfer already running');
      }
    }

    state.transferRunId++;
    const runId = state.transferRunId;
    state.transferCancel = false;
    state.transferActive = true;
    state.transferStatus = { run_id: runId, status: 'Starting', sent: 0, total: req.required_size || 0, files: 0, elapsed_secs: 0, current_file: '' };
    state.transferMeta = {
      requested_optimize: !!req.optimize_upload,
      auto_tune_connections: !!req.auto_tune_connections,
      effective_optimize: null,
      effective_compression: req.compression || null,
    };
    state.transferLastUpdate = Date.now();

    const emitLog = (message) => {
      if (state.saveLogs) writeLogLine('transfer', `[${runId}] ${message}`);
      if (state.uiLogEnabled) emit('transfer_log', { run_id: runId, message });
    };

    // Use an IIFE with .catch to handle unhandled promise rejections
    setImmediate(() => {
      (async () => {
        const startTime = Date.now();

        try {
          const sourceStat = await fs.promises.stat(req.source_path);
          const isRar = sourceStat.isFile() && path.extname(req.source_path).toLowerCase() === '.rar';

          if (isRar) {
            emitLog(`Uploading RAR for extraction: ${req.source_path}`);
            state.transferStatus = {
              run_id: runId,
              status: 'Uploading archive',
              sent: 0,
              total: sourceStat.size,
              files: 1,
              elapsed_secs: 0,
              current_file: path.basename(req.source_path),
            };
            state.transferLastUpdate = Date.now();

            const extractResult = await uploadRarForExtraction(
              req.ip,
              req.source_path,
              req.dest_path,
              req.rar_extract_mode || 'normal',
              {
                cancel: { get value() { return state.transferCancel; } },
                tempRoot: req.rar_temp_root,
                overrideOnConflict: req.override_on_conflict !== false,
                onProgress: (op, processed, total, currentFile) => {
                  state.transferStatus = {
                    run_id: runId,
                    status: op === 'Extract' ? 'Extracting' : 'Uploading archive',
                    sent: processed,
                    total: total || sourceStat.size,
                    files: 1,
                    elapsed_secs: (Date.now() - startTime) / 1000,
                    current_file: currentFile || path.basename(req.source_path),
                  };
                  state.transferLastUpdate = Date.now();
                },
                onLog: emitLog,
              }
            );

            const elapsed = (Date.now() - startTime) / 1000;
            const finalTotal = typeof extractResult.bytes === 'number' && extractResult.bytes > 0
              ? extractResult.bytes
              : sourceStat.size;
            const isQueued = !!extractResult.queuedId;
            state.transferStatus = {
              run_id: runId,
              status: isQueued ? 'Queued for extraction' : 'Complete',
              sent: finalTotal,
              total: finalTotal,
              files: extractResult.files || 1,
              elapsed_secs: elapsed,
              current_file: '',
            };
            state.transferLastUpdate = Date.now();
            if (isQueued) {
              emitLog(`Extraction queued (ID ${extractResult.queuedId}).`);
              emit('queue_hint', {
                queue_id: extractResult.queuedId,
                source_path: req.source_path,
                dest_path: req.dest_path,
                size_bytes: sourceStat.size
              });
            }
            emit('transfer_complete', { run_id: runId, files: extractResult.files || 1, bytes: finalTotal });
            return;
          }

          emitLog('Scanning files...');
          const result = await collectFiles(req.source_path, { get value() { return state.transferCancel; } }, (filesFound, totalSize) => {
          state.transferStatus = { ...state.transferStatus, status: 'Scanning', files: filesFound, total: totalSize };
          state.transferLastUpdate = Date.now();
          });

          if (result.cancelled) {
            throw new Error('Cancelled');
          }

          if (result.files.length === 0) {
            throw new Error('No files found to upload');
          }

          const normalizeResumeMode = (mode) => {
            if (mode === 'size_mtime') return 'size';
            const allowed = ['size', 'hash_large', 'hash_medium', 'sha256'];
            return allowed.includes(mode) ? mode : 'none';
          };
          const shouldHashResume = (mode, size) => {
            if (mode === 'sha256') return true;
            if (mode === 'hash_large') return size >= RESUME_HASH_LARGE_BYTES;
            if (mode === 'hash_medium') return size >= RESUME_HASH_MED_BYTES;
            return false;
          };
          const resumeMode = normalizeResumeMode(req.resume_mode);
          let filesToUpload = result.files;
          if (resumeMode && resumeMode !== 'none') {
            emitLog(`Resume scan: building remote index (${resumeMode})...`);
            state.transferStatus = { ...state.transferStatus, status: 'Resume scan', files: 0, total: Number(result.files.length) };
            state.transferLastUpdate = Date.now();

            const destRoot = String(req.dest_path || '').replace(/\\/g, '/');
            const remoteIndex = await buildRemoteIndex(
              req.ip,
              TRANSFER_PORT,
              destRoot,
              result.files,
              (done, total) => {
                state.transferStatus = { ...state.transferStatus, status: 'Resume scan', files: done, total };
                state.transferLastUpdate = Date.now();
              },
              emitLog
            );

            let skipped = 0;
            const filtered = [];
            for (const file of result.files) {
              const rel = file.rel_path.replace(/\\/g, '/');
              const remote = remoteIndex.get(rel);
              if (!remote) {
                filtered.push(file);
                continue;
              }
              const sizeMatch = Number(file.size) === Number(remote.size);
              if (resumeMode === 'size') {
                if (sizeMatch) {
                  skipped++;
                  continue;
                }
                filtered.push(file);
                continue;
              }
              if (!sizeMatch) {
                filtered.push(file);
                continue;
              }
              if (!shouldHashResume(resumeMode, Number(file.size))) {
                skipped++;
                continue;
              }
              const remotePath = joinRemotePath(destRoot, rel);
              try {
                const [localHash, remoteHash] = await Promise.all([
                  hashFileLocal(file.abs_path),
                  hashFileRemote(req.ip, TRANSFER_PORT, remotePath)
                ]);
                if (localHash === remoteHash) {
                  skipped++;
                  continue;
                }
              } catch (err) {
                emitLog(`Resume hash failed for ${rel}: ${err.message || err}`);
              }
              filtered.push(file);
            }

            filesToUpload = filtered;
            const resumeSummary = `Resume scan done: ${skipped} file(s) already present, ${filesToUpload.length} to upload.`;
            emitLog(resumeSummary);
            emit('manage_log', { message: resumeSummary });
            state.transferStatus = { ...state.transferStatus, status: 'Scanning', files: filesToUpload.length, total: Number(filesToUpload.length) };
            state.transferLastUpdate = Date.now();
          }

          const totalSize = filesToUpload.reduce((sum, f) => sum + BigInt(f.size), 0n);
          const fileCount = filesToUpload.length;
          const avgSize = fileCount > 0 ? Number(totalSize) / fileCount : 0;
          let effectiveCompression = req.compression;
          let effectiveOptimize = !!req.optimize_upload;
          let basePackLimit = PACK_BUFFER_SIZE;
          let baseChunkSize = SEND_CHUNK_SIZE;
          if (req.compression === 'auto') {
            if (avgSize < SMALL_FILE_AVG_BYTES || fileCount >= 100000) {
              effectiveCompression = 'lz4';
            } else if (avgSize > 8 * 1024 * 1024) {
              effectiveCompression = 'none';
            } else {
              effectiveCompression = 'lz4';
            }
          }
          if (req.auto_tune_connections && !effectiveOptimize) {
            if (avgSize < SMALL_FILE_AVG_BYTES || fileCount >= 50000) {
              effectiveOptimize = true;
            }
          }
          if (req.auto_tune_connections) {
            if (avgSize < SMALL_FILE_AVG_BYTES || fileCount >= 200000) {
              basePackLimit = 24 * 1024 * 1024;
              baseChunkSize = 4 * 1024 * 1024;
            }
          }
          state.transferMeta = {
            ...state.transferMeta,
            effective_optimize: effectiveOptimize,
            effective_compression: effectiveCompression,
          };
          if (req.compression !== effectiveCompression) {
            emitLog(`Auto-tune compression: ${req.compression} -> ${effectiveCompression}`);
          }
          if (!!req.optimize_upload !== effectiveOptimize) {
            emitLog('Auto-tune optimize: enabled to reduce per-file overhead.');
          }
          emitLog(`Starting transfer: ${(Number(totalSize) / (1024 * 1024 * 1024)).toFixed(2)} GB using ${req.connections} connection(s)`);

          state.transferStatus = { ...state.transferStatus, status: 'Uploading', files: filesToUpload.length, total: Number(totalSize) };
          state.transferLastUpdate = Date.now();

          const socket = await uploadV2Init(req.ip, TRANSFER_PORT, req.dest_path, req.use_temp, {
            optimize_upload: effectiveOptimize,
            chmod_after_upload: req.chmod_after_upload,
          });
          const rateLimitBps = req.bandwidth_limit_mbps ? req.bandwidth_limit_mbps * 1024 * 1024 / 8 : null; // Convert Mbps to Bps
          const adaptiveTuner = req.auto_tune_connections
            ? createAdaptiveUploadTuner({
                ip: req.ip,
                basePackLimit,
                baseChunkSize,
                userRateLimitBps: rateLimitBps,
                log: emitLog,
              })
            : null;

          let parsed;
          try {
            if (adaptiveTuner) {
              adaptiveTuner.start();
            }

            await sendFilesV2(filesToUpload, socket, {
              cancel: { get value() { return state.transferCancel; } },
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
                state.transferLastUpdate = Date.now();
              },
              log: emitLog,
              compression: effectiveCompression,
              rateLimitBps: rateLimitBps,
              packLimitBytes: basePackLimit,
              streamChunkBytes: baseChunkSize,
              getPackLimit: adaptiveTuner?.getPackLimit,
              getPaceDelayMs: adaptiveTuner?.getPaceDelayMs,
              getRateLimitBps: adaptiveTuner?.getRateLimitBps,
            });

            const response = await readUploadResponse(socket);
            parsed = parseUploadResponse(response);
          } finally {
            if (adaptiveTuner) {
              adaptiveTuner.stop();
            }
          }

          const elapsed = (Date.now() - startTime) / 1000;
          state.transferStatus = { run_id: runId, status: 'Complete', sent: totalSize, total: totalSize, files: parsed.files, elapsed_secs: elapsed, current_file: '' };
          state.transferLastUpdate = Date.now();

          emit('transfer_complete', { run_id: runId, files: parsed.files, bytes: parsed.bytes });
        } catch (err) {
          emit('transfer_error', { run_id: runId, message: err.message });
          state.transferStatus = { ...state.transferStatus, status: `Error: ${err.message}` };
          state.transferLastUpdate = Date.now();
        } finally {
          state.transferActive = false;
        }
      })().catch((err) => {
        // Handle any unhandled rejection from the async IIFE
        console.error('Unhandled transfer error:', err);
        emit('transfer_error', { run_id: runId, message: err.message || 'Unknown error' });
        state.transferActive = false;
      });
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

  // Chat (LAN broadcast)
  ipcMain.handle('chat_info', () => {
    return { room_id: chatRoomId, enabled: true };
  });

  ipcMain.handle('chat_generate_name', () => {
    return `User${Math.floor(Math.random() * 10000)}`;
  });

  ipcMain.handle('chat_start', () => {
    return startChatSocket();
  });

  ipcMain.handle('chat_stop', () => {
    return stopChatSocket();
  });

  ipcMain.handle('chat_send', (_, name, text) => {
    if (!name || !text) return false;
    sendChatMessage(String(name), String(text));
    return true;
  });

  // Game meta (simplified)
  ipcMain.handle('game_meta_load', async (_, sourcePath) => {
    return loadGameMetaForPath(sourcePath);
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
  if (chatSocket) {
    try {
      chatSocket.close();
    } catch {
      // ignore
    }
    chatSocket = null;
  }

  if (process.platform !== 'darwin') app.quit();
});
