const { app, BrowserWindow, ipcMain, dialog, nativeImage, shell, powerSaveBlocker } = require('electron');
const path = require('path');
const fs = require('fs');
const os = require('os');
const net = require('net');
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
const ftp = tryRequire('basic-ftp');

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
const RESUME_COMPAT_MAX_DIRS = 5000;
const RESUME_COMPAT_MAX_FILES = 100000;
const PRECREATE_MAX_DIRS = 5000;
const PRECREATE_DIR_CONCURRENCY = 4;
const WRITE_CHUNK_SIZE = 512 * 1024; // 512KB
const LANE_CONNECTIONS = 4;
const LANE_HUGE_FILE_BYTES = 20 * 1024 * 1024 * 1024; // 20GB
const LANE_LARGE_FILE_BYTES = 4 * 1024 * 1024 * 1024; // 4GB
const LANE_HUGE_CHUNK_BYTES = 1536 * 1024 * 1024; // 1.5GB
const LANE_LARGE_CHUNK_BYTES = 512 * 1024 * 1024; // 512MB
const LANE_DEFAULT_CHUNK_BYTES = 256 * 1024 * 1024; // 256MB
const LANE_MIN_FILE_SIZE = 512 * 1024 * 1024; // 512MB

const MAD_MAX_WORKERS = 8;
const MAD_MAX_HUGE_CHUNK_BYTES = 2 * 1024 * 1024 * 1024; // 2GB
const MAD_MAX_LARGE_CHUNK_BYTES = 1024 * 1024 * 1024; // 1GB
const MAD_MAX_DEFAULT_CHUNK_BYTES = 512 * 1024 * 1024; // 512MB
const MAD_MAX_MIN_FILE_SIZE = 64 * 1024 * 1024;
const MAGIC_FTX1 = 0x31585446;

const UploadCmd = {
  StartUpload: 0x10,
  UploadChunk: 0x11,
  EndUpload: 0x12,
};

const UploadResp = {
  Ok: 0x01,
  Error: 0x02,
  Data: 0x03,
  Ready: 0x04,
  Progress: 0x05,
};

let sleepBlockerId = null;
const VERSION = '1.4.9';
const IS_WINDOWS = process.platform === 'win32';

function beginManageOperation(op) {
  state.manageDoneEmitted = false;
  state.manageActiveOp = op;
}

function getProtectedLocalPaths() {
  const roots = new Set();
  try {
    if (process.resourcesPath) roots.add(path.resolve(process.resourcesPath));
  } catch {}
  try {
    if (process.execPath) roots.add(path.resolve(path.dirname(process.execPath)));
  } catch {}
  try {
    const appPath = app.getAppPath();
    if (appPath) {
      roots.add(path.resolve(appPath));
      roots.add(path.resolve(path.dirname(appPath)));
    }
  } catch {}
  return Array.from(roots);
}

function assertSafeManageDownloadDestination(destPath) {
  const target = path.resolve(String(destPath || ''));
  const protectedRoots = getProtectedLocalPaths();
  for (const root of protectedRoots) {
    if (!root) continue;
    if (target === root || target.startsWith(root + path.sep)) {
      throw new Error(
        `Refusing download destination inside running app files: ${target}. Choose another folder (e.g. Downloads/temp).`
      );
    }
  }
}

function emitManageDone(payload) {
  if (state.manageDoneEmitted) return;
  state.manageDoneEmitted = true;
  emit('manage_done', payload);
}

const FrameType = {
  PackAck: 5,
  PackV4: 15,
  PackLz4V4: 16,
  PackZstdV4: 17,
  PackLzmaV4: 18,
  Finish: 6,
  Error: 7,
};

const RecordFlag = {
  HasOffset: 1,
  HasTotal: 2,
  Truncate: 4,
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
const DEFAULT_GAMES_SCAN_PATHS = ['etaHEN/games', 'homebrew'];

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

async function sendFileOverSocket(socket, filePath, fileSize, opts = {}) {
  const {
    cancel = { value: false },
    onProgress = () => {},
    onLog = () => {},
    chunkSize = 1024 * 1024
  } = opts;
  const handle = await fs.promises.open(filePath, 'r');
  const buffer = Buffer.allocUnsafe(chunkSize);
  let position = 0;
  let sentBytes = 0;
  let closed = false;
  let socketError = null;
  let lastLogBytes = 0;
  let lastLogTime = Date.now();

  const progressInterval = setInterval(() => {
    onProgress('Upload', sentBytes, fileSize, path.basename(filePath));
  }, 1000);

  const onClose = () => {
    closed = true;
    onLog(`RAR upload socket closed after ${sentBytes}/${fileSize} bytes`);
  };
  const onError = (err) => {
    socketError = err;
    closed = true;
    onLog(`RAR upload socket error after ${sentBytes}/${fileSize} bytes: ${err && err.message ? err.message : err}`);
  };
  socket.on('close', onClose);
  socket.on('error', onError);

  const waitForDrain = () => new Promise((resolve, reject) => {
    const onDrain = () => cleanup(resolve);
    const onError = (err) => cleanup(() => reject(err));
    const onCloseWhile = () => cleanup(() => reject(new Error('Connection closed while writing')));
    const cleanup = (fn) => {
      socket.off('drain', onDrain);
      socket.off('error', onError);
      socket.off('close', onCloseWhile);
      fn();
    };
    socket.once('drain', onDrain);
    socket.once('error', onError);
    socket.once('close', onCloseWhile);
  });

  try {
    while (position < fileSize) {
      if (cancel.value) throw new Error('Upload cancelled');
      if (socketError) throw socketError;
      if (closed || socket.destroyed) throw new Error('Connection closed while writing');
      const toRead = Math.min(chunkSize, fileSize - position);
      const { bytesRead } = await handle.read(buffer, 0, toRead, position);
      if (bytesRead === 0) {
        throw new Error(`Unexpected EOF at ${position}/${fileSize}`);
      }
      position += bytesRead;
      sentBytes += bytesRead;
      const chunk = buffer.subarray(0, bytesRead);
      if (!socket.write(chunk)) {
        await waitForDrain();
      }
      if (sentBytes - lastLogBytes >= 64 * 1024 * 1024 || Date.now() - lastLogTime >= 10000) {
        lastLogBytes = sentBytes;
        lastLogTime = Date.now();
      }
    }
    onProgress('Upload', sentBytes, fileSize, path.basename(filePath));
    onLog(`RAR upload complete: ${sentBytes}/${fileSize} bytes`);
    return sentBytes;
  } finally {
    clearInterval(progressInterval);
    socket.off('close', onClose);
    socket.off('error', onError);
    await handle.close();
  }
}

async function uploadRarForExtraction(ip, rarPath, destPath, mode, opts = {}) {
  const {
    cancel = { value: false },
    onProgress = () => {},
    onLog = () => {},
    overrideOnConflict = true,
    tempRoot = '',
    queueExtract: queueExtraction = true,
  } = opts;
  const fileSize = (await fs.promises.stat(rarPath)).size;
  if (!Number.isSafeInteger(fileSize)) {
    throw new Error(`RAR file too large for safe integer math: ${fileSize}`);
  }

  try {
    await createPath(ip, TRANSFER_PORT, destPath);
  } catch (err) {
    const message = err && err.message ? err.message : String(err);
    throw new Error(`Create destination failed: ${message}`);
  }

  if (!overrideOnConflict) {
    try {
      const exists = await checkDir(ip, TRANSFER_PORT, destPath);
      if (exists) {
        throw new Error('Destination already exists');
      }
    } catch (err) {
      const message = err && err.message ? err.message : String(err);
      throw new Error(`Destination check failed: ${message}`);
    }
  }

  onLog('Using fast archive upload path.');
  return await uploadArchiveFastAndExtract(ip, rarPath, destPath, {
    cancel,
    onProgress,
    onLog,
    overrideOnConflict,
    tempRoot,
    queueExtract: queueExtraction,
  });

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
    const cmd = `UPLOAD_RAR_${String(modeToTry || 'normal').toUpperCase()} ${escapeCommandPath(destPath)} ${fileSize}${tmpToken}${flag}\n`;
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

  try {
    await sendFileOverSocket(socket, rarPath, fileSize, { cancel, onProgress, onLog });
    socket.end();
  } finally {
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

function isRemoteDirEntry(entry) {
  if (!entry || typeof entry !== 'object') return false;
  const entryType = String(entry.entry_type || entry.type || '').toLowerCase();
  return Boolean(entry.is_dir) || entryType === 'd' || entryType === 'dir' || entryType === 'directory';
}

function joinRemoteScanPath(...parts) {
  return parts
    .filter((part) => typeof part === 'string' && part.trim().length > 0)
    .map((part, index) => {
      const value = String(part);
      if (index === 0) return value.replace(/\/+$/, '') || '/';
      return value.replace(/^\/+/, '').replace(/\/+$/, '');
    })
    .join('/');
}

function normalizeRemoteScanSubpath(value) {
  if (typeof value !== 'string') return null;
  const normalized = value.trim().replace(/\\/g, '/').replace(/^\/+/, '').replace(/\/+$/, '');
  return normalized || null;
}

async function downloadRemoteFileToBuffer(ip, remotePath, maxBytes = 8 * 1024 * 1024) {
  const socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
  socket.setTimeout(0);
  return new Promise((resolve, reject) => {
    let headerDone = false;
    let expectedSize = 0;
    let headerBuf = Buffer.alloc(0);
    const chunks = [];
    let received = 0;
    let settled = false;

    const finish = (err, value) => {
      if (settled) return;
      settled = true;
      socket.removeAllListeners();
      socket.destroy();
      if (err) {
        reject(err);
        return;
      }
      resolve(value);
    };

    socket.on('data', (chunk) => {
      if (!headerDone) {
        headerBuf = Buffer.concat([headerBuf, chunk]);
        const nl = headerBuf.indexOf('\n');
        if (nl === -1) return;
        const line = headerBuf.slice(0, nl).toString('utf8').trim();
        const remainder = headerBuf.slice(nl + 1);
        headerBuf = Buffer.alloc(0);
        if (line.startsWith('ERROR')) {
          finish(new Error(line));
          return;
        }
        const match = line.match(/^(?:OK|READY)\s+(\d+)/i);
        if (!match) {
          finish(new Error(`Unexpected response: ${line}`));
          return;
        }
        expectedSize = Number(match[1]);
        if (!Number.isFinite(expectedSize) || expectedSize < 0) {
          finish(new Error(`Invalid size for ${remotePath}`));
          return;
        }
        if (expectedSize > maxBytes) {
          finish(new Error(`File too large for scan: ${remotePath}`));
          return;
        }
        headerDone = true;
        if (remainder.length > 0) {
          chunks.push(remainder);
          received += remainder.length;
        }
      } else {
        chunks.push(chunk);
        received += chunk.length;
      }

      if (received > maxBytes) {
        finish(new Error(`File exceeded scan limit: ${remotePath}`));
        return;
      }
      if (headerDone && received >= expectedSize) {
        finish(null, Buffer.concat(chunks, received).subarray(0, expectedSize));
      }
    });

    socket.on('error', (err) => finish(err));
    socket.on('close', () => {
      if (!headerDone) {
        finish(new Error(`Connection closed before response for ${remotePath}`));
        return;
      }
      if (received >= expectedSize) {
        finish(null, Buffer.concat(chunks, received).subarray(0, expectedSize));
      } else {
        finish(new Error(`Incomplete download for ${remotePath}: ${received}/${expectedSize}`));
      }
    });
    socket.on('end', () => {
      if (!headerDone) return;
      if (received >= expectedSize) {
        finish(null, Buffer.concat(chunks, received).subarray(0, expectedSize));
      }
    });

    // Use DOWNLOAD for metadata reads; DOWNLOAD_RAW has been unstable on some payload builds.
    socket.write(`DOWNLOAD ${remotePath}\n`);
  });
}

async function loadRemoteGameMetaForPath(ip, gamePath) {
  const paramCandidates = [
    joinRemoteScanPath(gamePath, 'sce_sys', 'param.json'),
    joinRemoteScanPath(gamePath, 'param.json')
  ];
  let markerFile = null;
  let meta = null;
  for (const paramPath of paramCandidates) {
    try {
      const bytes = await downloadRemoteFileToBuffer(ip, paramPath, 512 * 1024);
      const parsed = JSON.parse(bytes.toString('utf8'));
      const parsedMeta = parseGameMetaFromParam(parsed);
      if (parsedMeta) {
        markerFile = paramPath;
        meta = parsedMeta;
        break;
      }
    } catch {
      // Not every folder contains game metadata; ignore and continue.
    }
  }
  if (!meta) {
    return { marker_file: null, meta: null, cover: null };
  }

  let cover = null;
  if (markerFile) {
    const isSceSysParam = markerFile.toLowerCase().includes('/sce_sys/param.json');
    const sceBase = isSceSysParam
      ? markerFile.slice(0, markerFile.toLowerCase().lastIndexOf('/param.json'))
      : joinRemoteScanPath(gamePath, 'sce_sys');
    for (const name of COVER_CANDIDATES) {
      const candidates = isSceSysParam
        ? [joinRemoteScanPath(sceBase, name), joinRemoteScanPath(gamePath, name)]
        : [joinRemoteScanPath(sceBase, name), joinRemoteScanPath(gamePath, name)];
      for (const candidate of candidates) {
        try {
          const bytes = await downloadRemoteFileToBuffer(ip, candidate, 8 * 1024 * 1024);
          cover = loadCoverImageFromBytes(bytes, 160);
          if (cover) break;
        } catch {
          // Try the next candidate.
        }
      }
      if (cover) break;
    }
  }

  return { marker_file: markerFile, meta, cover };
}

async function scanRemoteGames(ip, storagePaths = [], scanPaths = []) {
  const scannedStorage = [];
  const skippedStorage = [];
  const scannedGamesDirs = [];
  const games = [];

  const roots = Array.isArray(storagePaths)
    ? storagePaths.filter((value) => typeof value === 'string' && value.trim().length > 0)
    : [];
  const requestedSubpathsRaw = Array.isArray(scanPaths) ? scanPaths : [];
  const requestedSubpaths = requestedSubpathsRaw
    .map(normalizeRemoteScanSubpath)
    .filter((value, index, array) => value && array.indexOf(value) === index);
  const scanSubpaths = requestedSubpaths.length > 0 ? requestedSubpaths : DEFAULT_GAMES_SCAN_PATHS;

  for (const storagePath of roots) {
    try {
      await listDir(ip, TRANSFER_PORT, storagePath);
      scannedStorage.push(storagePath);
    } catch {
      skippedStorage.push(storagePath);
      continue;
    }

    for (const subpath of scanSubpaths) {
      const gamesDir = joinRemoteScanPath(storagePath, subpath);
      let folderEntries = [];
      try {
        folderEntries = await listDir(ip, TRANSFER_PORT, gamesDir);
        scannedGamesDirs.push(gamesDir);
      } catch {
        continue;
      }
      const gameFolders = folderEntries.filter((entry) => isRemoteDirEntry(entry) && entry.name);
      const found = await mapWithConcurrency(gameFolders, 1, async (entry) => {
        const folderName = String(entry.name);
        const gamePath = joinRemoteScanPath(gamesDir, folderName);
        try {
          const details = await loadRemoteGameMetaForPath(ip, gamePath);
          if (!details.marker_file && !details.meta) return null;
          return {
            storage_path: storagePath,
            games_path: gamesDir,
            path: gamePath,
            folder_name: folderName,
            marker_file: details.marker_file || null,
            meta: details.meta || null,
            cover: details.cover || null
          };
        } catch {
          return null;
        }
      });
      for (const item of found) {
        if (item) games.push(item);
      }
    }
  }

  return {
    games,
    scanned_storage: scannedStorage,
    scanned_games_dirs: scannedGamesDirs,
    skipped_storage: skippedStorage,
    scan_paths: scanSubpaths
  };
}

async function scanRemoteGameStats(ip, gamePath) {
  if (!gamePath || typeof gamePath !== 'string' || !gamePath.trim()) {
    throw new Error('Invalid game path.');
  }
  let scannedFiles = 0;
  let totalSize = 0;
  const entries = await listDirRecursive(
    ip,
    TRANSFER_PORT,
    gamePath,
    null,
    (entry) => {
      const sizeValue = Number(entry && entry.size);
      scannedFiles += 1;
      if (Number.isFinite(sizeValue) && sizeValue > 0) {
        totalSize += sizeValue;
      }
      if ((scannedFiles % 200) === 0) {
        emit('payload_log', {
          message: `[GAMES_SCAN_PROGRESS] path=${gamePath} files=${scannedFiles} bytes=${totalSize}`
        });
      }
    }
  );
  const fileCount = Array.isArray(entries) ? entries.length : scannedFiles;
  emit('payload_log', {
    message: `[GAMES_SCAN_COMPLETE] path=${gamePath} files=${fileCount} bytes=${totalSize}`
  });
  return {
    path: gamePath,
    file_count: fileCount,
    total_size: totalSize,
  };
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

const normalizeUploadMode = (mode) => {
  if (typeof mode === 'string') mode = mode.toLowerCase();
  if (mode === 'ftp' || mode === 'mix') return mode;
  return 'payload';
};

const normalizeFtpPort = (value) => {
  if (value === '1337' || value === 1337) return 1337;
  if (value === '2121' || value === 2121) return 2121;
  return 'auto';
};

const formatFtpPortLabel = (preferredPort) => (preferredPort ? String(preferredPort) : '1337/2121');

const getFtpServiceHint = () =>
  'FTP requires ftpsrv running or the etaHEN FTP service enabled on the PS5.';

const buildFtpUnavailableError = (preferredPort) =>
  new Error(
    `FTP not detected on port${preferredPort ? '' : 's'} ${formatFtpPortLabel(preferredPort)}. ` +
    'Make sure ftpsrv is running or the etaHEN FTP service is enabled.'
  );

const formatBytes = (bytes) => {
  const value = typeof bytes === 'bigint' ? Number(bytes) : Number(bytes);
  if (!Number.isFinite(value) || value < 0) return '0 B';
  const kb = 1024;
  const mb = kb * 1024;
  const gb = mb * 1024;
  if (value >= gb) return `${(value / gb).toFixed(2)} GB`;
  if (value >= mb) return `${(value / mb).toFixed(2)} MB`;
  if (value >= kb) return `${(value / kb).toFixed(2)} KB`;
  return `${Math.floor(value)} B`;
};

const detectFtpPort = async (host, preferredPort) => {
  const ports = preferredPort ? [preferredPort] : [1337, 2121];
  const tryPort = (port) =>
    new Promise((resolve) => {
      const socket = new net.Socket();
      let resolved = false;
      const done = (ok) => {
        if (resolved) return;
        resolved = true;
        socket.destroy();
        resolve(ok ? port : null);
      };
      socket.setTimeout(800, () => done(false));
      socket.once('error', () => done(false));
      socket.connect(port, host, () => {
        socket.once('data', (data) => {
          const text = data.toString('utf8');
          if (text.startsWith('220') || text.toLowerCase().includes('ftp')) {
            done(true);
          } else {
            done(false);
          }
        });
      });
    });

  for (const port of ports) {
    const okPort = await tryPort(port);
    if (okPort) return okPort;
  }
  return null;
};

const accessFtpClient = async (client, host, port) => {
  const user = 'anonymous';
  const password = 'anonymous@';
  try {
    await client.access({ host, port, user, password, secure: false, timeout: 10000 });
    return;
  } catch (err) {
    const message = err?.message || '';
    const code = err?.code || err?.data?.code;
    const passUnsupported =
      message.includes('PASS') ||
      message.includes('Command not recognized') ||
      code === 502;
    if (!passUnsupported) throw err;
  }
  await client.close();
  await client.connect(host, port);
  const res = await client.send('USER', user);
  if (res?.code === 331) {
    await client.send('PASS', password);
  }
};

const uploadFilesViaFtp = async (host, port, destRoot, files, opts = {}) => {
  if (!ftp) {
    throw new Error('FTP library unavailable.');
  }
  const getNextFile = typeof files === 'function'
    ? files
    : (() => {
        let idx = 0;
        return () => (idx < files.length ? files[idx++] : null);
      })();
  const cancel = opts.cancel || { value: false };
  const log = opts.log;
  const onProgress = opts.onProgress;
  const onFile = opts.onFile;
  const onFileDone = opts.onFileDone;
  const onSkipFile = opts.onSkipFile;
  const maxConnections = 6;
  const connections = clamp(Math.floor(opts.connections || 1), 1, maxConnections);
  const throttleMs = Math.max(0, Number(opts.throttle_ms || 0));
  const smallFileThreshold = Math.max(0, Number(opts.small_file_threshold || 0));
  let totalSent = 0;
  let totalFiles = 0;
  const runWorker = async () => {
    const client = new ftp.Client(0);
    client.ftp.verbose = false;
    let currentName = '';
    let lastBytes = 0;
    client.trackProgress((info) => {
      if (info.type !== 'upload') return;
      if (info.name !== currentName) {
        currentName = info.name || '';
        lastBytes = 0;
        if (onFile && currentName) onFile(currentName);
      }
      const delta = Math.max(0, info.bytes - lastBytes);
      if (delta > 0) {
        totalSent += delta;
        lastBytes = info.bytes;
        if (onProgress) onProgress(totalSent, currentName);
      }
    });

    try {
      await accessFtpClient(client, host, port);
      while (true) {
        const file = getNextFile();
        if (!file) break;
        if (cancel.value) throw new Error('Upload cancelled by user');
        const rel = String(file.rel_path || '').replace(/\\/g, '/');
        const remotePath = joinRemotePath(destRoot, rel);
        const remoteDir = path.posix.dirname(remotePath);
        if (remoteDir && remoteDir !== '.' && remoteDir !== '/') {
          await client.ensureDir(remoteDir);
        }
        try {
          await client.uploadFrom(file.abs_path, remotePath);
        } catch (err) {
          const code = err?.code;
          if (code === 'ENOENT' || code === 'EACCES' || code === 'EPERM') {
            if (log) log(`Skipping missing/unreadable file: ${file.rel_path}`, 'warn');
            if (typeof onSkipFile === 'function') onSkipFile(file, err);
            continue;
          }
          throw err;
        }
        totalFiles += 1;
        if (onFileDone) onFileDone(file);
        if (throttleMs > 0 && (smallFileThreshold <= 0 || file.size <= smallFileThreshold)) {
          await sleepMs(throttleMs);
        }
      }
    } finally {
      client.trackProgress();
      client.close();
    }
  };

  try {
    const workers = Array.from({ length: connections }, () => runWorker());
    await Promise.all(workers);
  } catch (err) {
    if (log) log(`FTP upload failed: ${err.message || err}`, 'error');
    throw err;
  }
  return { bytes: totalSent, files: totalFiles };
};

const getConfigPath = () => path.join(getAppDataDir(), 'ps5upload.ini');
const getHistoryPath = () => path.join(getAppDataDir(), 'ps5upload_history.json');
const getQueuePath = () => path.join(getAppDataDir(), 'ps5upload_queue.json');
const getProfilesPath = () => path.join(getAppDataDir(), 'ps5upload_profiles.ini');
const getLogsDir = () => path.join(getAppDataDir(), 'logs');

const createTransferStatus = (overrides = {}) => ({
  run_id: 0,
  status: 'Idle',
  sent: 0,
  total: 0,
  files: 0,
  elapsed_secs: 0,
  current_file: '',
  payload_sent: 0,
  ftp_sent: 0,
  payload_speed_bps: 0,
  ftp_speed_bps: 0,
  total_speed_bps: 0,
  upload_mode: null,
  payload_transfer_path: null,
  payload_workers: null,
  ...overrides,
});

// State
let mainWindow = null;
const state = {
  transferCancel: false,
  transferActive: false,
  transferRunId: 0,
  transferAbort: null,
  transferSocket: null,
  transferStatus: { run_id: 0, status: 'Idle', sent: 0, total: 0, files: 0, elapsed_secs: 0, current_file: '' },
  transferMeta: { requested_optimize: null, auto_tune_connections: null, effective_optimize: null, effective_compression: null, requested_ftp_connections: null, effective_ftp_connections: null },
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
  scanCache: null,
  scanInProgressKey: null,
};


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

// Robust socket write with backpressure logging
function writeAllRetry(socket, data, cancel, log = () => {}) {
  return new Promise((resolve, reject) => {
    let offset = 0;
    let drainWaitStart = null;
    let drainLogInterval = null;
    let drainNotified = false;
    let finished = false;

    const cleanup = () => {
      if (drainLogInterval) {
        clearInterval(drainLogInterval);
        drainLogInterval = null;
      }
      socket.removeListener('error', onError);
      socket.removeListener('close', onClose);
    };

    const onError = (err) => {
      if (finished) return;
      finished = true;
      cleanup();
      reject(err);
    };

    const onClose = () => {
      if (finished) return;
      finished = true;
      cleanup();
      reject(new Error('Connection closed while writing'));
    };

    socket.on('error', onError);
    socket.on('close', onClose);

    const writeLoop = () => {
      if (finished) return;

      if (cancel.value) {
        finished = true;
        cleanup();
        socket.destroy();
        return reject(new Error('Upload cancelled by user'));
      }

      // If we were waiting for drain, clear the log interval
      if (drainWaitStart) {
        if (drainLogInterval) {
          clearInterval(drainLogInterval);
          drainLogInterval = null;
        }
        drainWaitStart = null;
        drainNotified = false;
      }

      while (offset < data.length) {
        const end = Math.min(offset + WRITE_CHUNK_SIZE, data.length);
        const chunk = data.slice(offset, end);
        const ok = socket.write(chunk);
        offset = end;
        if (!ok) { // Buffer full, wait for 'drain'
          drainWaitStart = Date.now();
          drainNotified = true;
          socket.once('drain', writeLoop);
          return;
        }
      }
      finished = true;
      cleanup();
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
  socket.setTimeout(15 * 60 * 1000, () => {
    try { socket.destroy(new Error('Upload socket timeout')); } catch {}
  });
  if (typeof socket.setSendBufferSize === 'function') {
    socket.setSendBufferSize(UPLOAD_SOCKET_BUFFER_SIZE);
  }
  if (typeof socket.setRecvBufferSize === 'function') {
    socket.setRecvBufferSize(UPLOAD_SOCKET_BUFFER_SIZE);
  }
}

function escapeCommandPath(value) {
  const text = String(value ?? '');
  const escaped = text
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"')
    .replace(/\n/g, '\\n')
    .replace(/\r/g, '\\r')
    .replace(/\t/g, '\\t');
  return `"${escaped}"`;
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

async function sendSimpleCommand(ip, port, cmd, signal) {
  const MAX_RESPONSE_SIZE = 1024 * 1024; // 1MB limit
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;

    const cleanup = () => {
      socket.removeAllListeners();
      socket.destroy();
      if (signal) {
        signal.removeEventListener('abort', onAbort);
      }
    };

    const onAbort = () => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(new Error('Cancelled'));
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

    if (signal) {
      if (signal.aborted) {
        onAbort();
        return;
      }
      signal.addEventListener('abort', onAbort);
    }

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
      if (str.startsWith('ERROR:')) {
        resolved = true;
        cleanup();
        reject(new Error(str.trim()));
        return;
      }
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
          if (str.startsWith('ERROR:')) {
            reject(new Error(str.trim()));
            return;
          }
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

async function listDir(ip, port, dirPath, signal) {
  const MAX_RESPONSE_SIZE = 10 * 1024 * 1024; // 10MB limit for directory listings
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;

    const cleanup = () => {
      socket.removeAllListeners();
      socket.destroy();
      if (signal) {
        signal.removeEventListener('abort', onAbort);
      }
    };

    const onAbort = () => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(new Error('Cancelled'));
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
      if (str.startsWith('ERROR:')) {
        resolved = true;
        cleanup();
        reject(new Error(str.trim()));
        return;
      }
      if (str.includes('\n]\n') || str.endsWith('\n]')) {
        resolved = true;
        cleanup();
        try {
          const jsonEnd = str.lastIndexOf(']');
          const jsonStr = str.substring(0, jsonEnd + 1);
          const entries = JSON.parse(jsonStr);
          resolve(entries);
        } catch (e) {
          const snippet = str.length > 200 ? `${str.slice(0, 200)}...` : str;
          reject(new Error(`Invalid JSON response: ${snippet}`));
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
          if (str.startsWith('ERROR:')) {
            reject(new Error(str.trim()));
            return;
          }
          const jsonEnd = str.lastIndexOf(']');
          const jsonStr = str.substring(0, jsonEnd + 1);
          resolve(JSON.parse(jsonStr));
        } catch (e) {
          const str = data.toString('utf8');
          const snippet = str.length > 200 ? `${str.slice(0, 200)}...` : str;
          reject(new Error(`Invalid JSON response: ${snippet}`));
        }
      }
    });

    if (signal) {
      if (signal.aborted) {
        onAbort();
        return;
      }
      signal.addEventListener('abort', onAbort);
    }

    socket.write(`LIST_DIR ${dirPath}\n`);
  });
}

async function listDirRecursive(ip, port, dirPath, signal, onEntry = null, options = {}) {
  const { collect = true } = options;
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let buffer = '';
    let resolved = false;
    let sawStart = false;
    let done = false;
    const entries = collect ? [] : null;

    const cleanup = () => {
      socket.removeAllListeners();
      socket.destroy();
      if (signal) {
        signal.removeEventListener('abort', onAbort);
      }
    };

    const onAbort = () => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(new Error('Cancelled'));
    };

    const handleLine = (line) => {
      let text = line.trim();
      if (!text) return true;
      if (text.startsWith('ERROR:')) {
        resolved = true;
        cleanup();
        reject(new Error(text));
        return false;
      }
      if (!sawStart) {
        if (text.startsWith('ERROR')) {
          resolved = true;
          cleanup();
          reject(new Error(text));
          return false;
        }
        if (text.startsWith('[')) {
          sawStart = true;
          text = text.slice(1).trim();
          if (!text) return true;
        }
      }
      if (text.startsWith(']')) {
        done = true;
        resolved = true;
        cleanup();
        resolve(entries || []);
        return false;
      }
      if (text[0] === ',') {
        text = text.slice(1).trim();
      }
      if (text.endsWith(',')) {
        text = text.slice(0, -1).trim();
      }
      if (!text) return true;
      try {
        const entry = JSON.parse(text);
        if (collect && entries) entries.push(entry);
        if (onEntry) onEntry(entry);
      } catch (e) {
        resolved = true;
        cleanup();
        const snippet = text.length > 200 ? `${text.slice(0, 200)}...` : text;
        reject(new Error(`Invalid JSON response: ${snippet}`));
        return false;
      }
      return true;
    };

    // 5 minute timeout for large directories
    socket.setTimeout(300000);
    socket.on('timeout', () => {
      if (resolved) return;
      resolved = true;
      cleanup();
      reject(new Error('Read timed out'));
    });

    socket.on('data', (chunk) => {
      if (resolved) return;
      buffer += chunk.toString('utf8');
      let idx;
      while ((idx = buffer.indexOf('\n')) !== -1) {
        const line = buffer.slice(0, idx);
        buffer = buffer.slice(idx + 1);
        if (!handleLine(line)) return;
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
      if (buffer.length > 0) {
        handleLine(buffer);
      }
      if (done) return;
      resolved = true;
      cleanup();
      reject(new Error('Connection closed'));
    });

    if (signal) {
      if (signal.aborted) {
        onAbort();
        return;
      }
      signal.addEventListener('abort', onAbort);
    }

    socket.write(`LIST_DIR_RECURSIVE ${dirPath}\n`);
  });
}

  async function listDirRecursiveCompat(ip, port, dirPath, signal, onLog) {
    const abortSignal = signal && typeof signal.addEventListener === 'function' ? signal : null;
    const files = [];
    const stack = [{ path: dirPath, rel: '' }];
    while (stack.length > 0) {
      if (abortSignal?.aborted) throw new Error('Cancelled');
      const current = stack.pop();
      const entries = await listDir(ip, port, current.path, abortSignal || undefined);
      if (typeof onLog === 'function') {
        onLog(`ListDir ${current.path}: ${entries.length} entries`);
        if (entries.length > 0) {
          const sample = entries.slice(0, 5).map((entry) => entry.name).join(', ');
          onLog(`ListDir sample: ${sample}`);
        }
      }
      for (const entry of entries) {
        const name = entry.name;
        const relPath = current.rel ? `${current.rel}/${name}` : name;
        const remotePath = `${current.path}/${name}`;
        const entryType = (entry.entry_type || entry.type || '').toLowerCase();
        const isDir = entry.is_dir || entryType === 'd' || entryType === 'dir' || entryType === 'directory';
        const isFile = entry.is_file || entryType === 'f' || entryType === '-' || entryType === 'file';
        if (isDir) {
          stack.push({ path: remotePath, rel: relPath });
        } else if (isFile) {
          files.push({ remotePath, relPath, size: entry.size || 0 });
        }
      }
  }
  return files;
}

async function hashFileLocal(filePath, signal) {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash('sha256');
    const stream = fs.createReadStream(filePath);
    const onAbort = () => {
      stream.destroy(new Error('Cancelled'));
    };
    stream.on('data', chunk => hash.update(chunk));
    stream.on('error', reject);
    stream.on('end', () => resolve(hash.digest('hex')));
    if (signal) {
      if (signal.aborted) {
        onAbort();
        return;
      }
      signal.addEventListener('abort', onAbort, { once: true });
      stream.on('close', () => signal.removeEventListener('abort', onAbort));
    }
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

async function buildRemoteIndex(ip, port, destRoot, files, onProgress, onLog, signal) {
  const index = new Map();
  let entries;
  const totalHint = Array.isArray(files) ? files.length : 0;
  onLog?.(`Resume scan: listing remote tree via LIST_DIR_RECURSIVE at ${destRoot}`);
  try {
    let seen = 0;
    await listDirRecursive(
      ip,
      port,
      destRoot,
      signal,
      (entry) => {
        if (!entry || entry.type !== 'file') return;
        index.set(entry.name.replace(/\\/g, '/'), {
          size: Number(entry.size) || 0,
          mtime: Number(entry.mtime) || 0
        });
        seen += 1;
        if (onProgress) onProgress(Math.min(seen, totalHint || seen), totalHint || seen);
      },
      { collect: false }
    );
  } catch (err) {
    onLog?.(`Resume scan: failed to list recursively ${destRoot}: ${err.message || err}`);
    onLog?.(`Resume scan: LIST_DIR_RECURSIVE failed, retrying with compatibility listing. Reason: ${err.message || err}`);
    // Compatibility listing for payloads without stable recursive listing.
    return buildRemoteIndexCompat(ip, port, destRoot, files, onProgress, onLog, signal, {
      maxDirs: RESUME_COMPAT_MAX_DIRS,
      maxFiles: RESUME_COMPAT_MAX_FILES,
    });
  }

  onLog?.(`Resume scan: recursive listing complete, ${index.size} remote file(s) indexed`);
  if (onProgress) onProgress(1, 1); // Signal completion
  return index;
}

// Compatibility listing mode when recursive listing is unavailable.
async function buildRemoteIndexCompat(ip, port, destRoot, files, onProgress, onLog, signal, opts = {}) {
  const dirSet = new Set();
  for (const file of files) {
    const rel = file.rel_path || '';
    let dir = path.posix.dirname(rel);
    if (dir === '.') dir = '';
    dirSet.add(dir);
  }
  const dirs = Array.from(dirSet);
  if (opts.maxFiles && Array.isArray(files) && files.length > opts.maxFiles) {
    onLog?.(`Resume scan: skipping compatibility listing (${files.length} files exceeds limit ${opts.maxFiles}).`);
    return null;
  }
  if (opts.maxDirs && dirs.length > opts.maxDirs) {
    onLog?.(`Resume scan: skipping compatibility listing (${dirs.length} directories exceeds limit ${opts.maxDirs}).`);
    return null;
  }
  const total = dirs.length;
  let done = 0;
  onLog?.(`Resume scan: compatibility listing ${total} director${total === 1 ? 'y' : 'ies'} under ${destRoot}`);

  const index = new Map();
  const listOne = async (dir) => {
    const remoteDir = joinRemotePath(destRoot, dir);
    try {
      const entries = await listDir(ip, port, remoteDir, signal);
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
  onLog?.(`Resume scan: compatibility listing complete, ${index.size} remote file(s) indexed`);
  return index;
}

async function hashFileRemote(ip, port, filePath, signal) {
  const response = await sendSimpleCommand(ip, port, `HASH_FILE ${filePath}\n`, signal);
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

async function ensurePayloadReady(ip) {
  let lastErr = null;
  let recoveryTriggered = false;
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      await getPayloadStatus(ip, TRANSFER_PORT);
      return true;
    } catch (err) {
      lastErr = err;
      const message = err?.message || String(err);
      if (!recoveryTriggered && message.includes('ECONNREFUSED')) {
        recoveryTriggered = true;
        try {
          await triggerPayloadRecovery('manage download connection refused');
        } catch {
          // ignore recovery failures
        }
      }
      await sleepMs(1000);
    }
  }
  throw new Error(`Payload not ready: ${lastErr?.message || lastErr}`);
}

function createDefaultPayloadCaps(version = null) {
  return {
    schema_version: 1,
    source: 'compat',
    payload_version: version || null,
    firmware: null,
    features: {
      status: true,
      queue: true,
      queue_extract: true,
      upload_queue_sync: true,
      history_sync: true,
      maintenance: true,
      chmod: true,
      games_scan_meta: true,
    },
    limits: {},
    commands: [
      'VERSION',
      'PAYLOAD_STATUS',
      'QUEUE_EXTRACT',
      'QUEUE_CANCEL',
      'QUEUE_CLEAR',
      'QUEUE_CLEAR_ALL',
      'QUEUE_CLEAR_FAILED',
      'QUEUE_REORDER',
      'QUEUE_PROCESS',
      'QUEUE_PAUSE',
      'QUEUE_RETRY',
      'QUEUE_REMOVE',
      'UPLOAD_QUEUE_GET',
      'UPLOAD_QUEUE_SYNC',
      'HISTORY_GET',
      'HISTORY_SYNC',
      'MAINTENANCE',
      'CHMOD777',
    ],
    notes: [],
    updated_at_ms: Date.now(),
  };
}

function normalizePayloadCaps(raw, fallbackVersion = null) {
  const base = createDefaultPayloadCaps(fallbackVersion);
  if (!raw || typeof raw !== 'object') return base;
  const next = {
    ...base,
    ...raw,
    schema_version: parseInt(String(raw.schema_version || base.schema_version), 10) || 1,
    source: typeof raw.source === 'string' && raw.source.trim() ? raw.source : 'payload',
    payload_version: raw.payload_version != null ? String(raw.payload_version) : base.payload_version,
    firmware: raw.firmware != null ? String(raw.firmware) : null,
    updated_at_ms: Date.now(),
  };
  if (raw.features && typeof raw.features === 'object') {
    next.features = { ...base.features, ...raw.features };
  } else {
    next.features = { ...base.features };
  }
  if (raw.limits && typeof raw.limits === 'object') {
    next.limits = { ...raw.limits };
  } else {
    next.limits = {};
  }
  if (Array.isArray(raw.commands)) {
    next.commands = raw.commands.map((item) => String(item));
  } else {
    next.commands = [...base.commands];
  }
  if (Array.isArray(raw.notes)) {
    next.notes = raw.notes.map((item) => String(item));
  } else {
    next.notes = [];
  }
  return next;
}

async function getPayloadCaps(ip, port) {
  let version = null;
  let versionErr = null;
  try {
    version = await getPayloadVersion(ip, port);
  } catch (err) {
    versionErr = err;
  }

  try {
    const raw = await sendCommandExpectPayload(ip, port, 'CAPS\n');
    const parsed = JSON.parse(raw);
    return normalizePayloadCaps(parsed, version);
  } catch (err) {
    if (versionErr) throw versionErr;
    const fallback = createDefaultPayloadCaps(version);
    fallback.source = 'compat-defaults';
    fallback.notes = [];
    fallback.updated_at_ms = Date.now();
    return fallback;
  }
}


async function queueExtract(ip, port, src, dst, opts = {}) {
  const cleanupPath = typeof opts.cleanupPath === 'string' ? opts.cleanupPath.trim() : '';
  const deleteSource = opts.deleteSource === true;
  const tokens = [src, dst];
  if (cleanupPath || deleteSource) {
    tokens.push(cleanupPath);
    if (deleteSource) {
      tokens.push('DEL');
    }
  }
  const cmd = `QUEUE_EXTRACT ${tokens.join('\t')}\n`;
  const response = await sendSimpleCommand(ip, port, cmd);
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

async function queueRemove(ip, port, id) {
  const response = await sendSimpleCommand(ip, port, `QUEUE_REMOVE ${id}\n`);
  if (!response.startsWith('OK')) {
    throw new Error(`Queue remove failed: ${response}`);
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

async function requestPayloadShutdown(ip, port) {
  try {
    const socket = await createSocketWithTimeout(ip, port, 2000);
    socket.setTimeout(2000);
    return await new Promise((resolve) => {
      let data = Buffer.alloc(0);
      socket.on('timeout', () => {
        socket.destroy();
        resolve(false);
      });
      socket.on('error', () => resolve(false));
      socket.on('data', (chunk) => {
        data = Buffer.concat([data, chunk]);
        if (data.toString('utf8').includes('\n')) {
          const line = data.toString('utf8').trim();
          socket.destroy();
          resolve(line.startsWith('OK'));
        }
      });
      socket.write('SHUTDOWN\n');
    });
  } catch {
    return false;
  }
}

async function uploadPayloadInit(ip, port, destPath, useTemp, opts = {}, signal) {
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

    const onAbort = () => {
      socket.destroy();
      reject(new Error('Cancelled'));
    };

    const onData = (chunk) => {
      data = Buffer.concat([data, chunk]);
      const response = data.toString('utf8').trim();
      if (response === 'READY') {
        socket.removeListener('data', onData);
        socket.setTimeout(0);
        if (signal) {
          signal.removeEventListener('abort', onAbort);
        }
        resolve(socket);
      } else if (response.length > 0 && !response.startsWith('READY')) {
        socket.destroy();
        if (response.startsWith('OK')) {
          reject(new Error('PAYLOAD_UNSUPPORTED'));
        } else {
          reject(new Error(`Server rejected payload upload: ${response}`));
        }
      }
    };

    socket.on('data', onData);
    socket.on('error', (err) => reject(err));

    if (signal) {
      if (signal.aborted) {
        onAbort();
        return;
      }
      signal.addEventListener('abort', onAbort, { once: true });
    }

    socket.write(`UPLOAD_V4 ${escapeCommandPath(destPath)} ${mode}${flagStr}\n`);
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
  connections: 4,
  ftp_connections: 10,
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
        case 'ftp_connections': config.ftp_connections = Math.max(1, parseInt(value, 10) || 1); break;
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
        case 'rar_extract_mode': config.rar_extract_mode = 'turbo'; break;
        case 'window_width': config.window_width = parseInt(value, 10) || 1440; break;
        case 'window_height': config.window_height = parseInt(value, 10) || 960; break;
        case 'window_x': config.window_x = parseInt(value, 10) || -1; break;
        case 'window_y': config.window_y = parseInt(value, 10) || -1; break;
      }
    }

    if (config.auto_check_payload && !config.payload_auto_reload) {
      config.payload_auto_reload = true;
    }

    const normalized = config;
    const normalizedText = serializeConfig(normalized);
    if (content.trim() !== normalizedText.trim()) {
      ensureDir(getAppDataDir());
      fs.writeFileSync(configPath, normalizedText);
    }
    return normalized;
  } catch (e) {
    return { ...defaultConfig };
  }
}

function serializeConfig(config) {
  const lines = [
    `address=${config.address}`,
    `storage=${config.storage}`,
    `connections=${config.connections}`,
    `ftp_connections=${config.ftp_connections}`,
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
    `rar_extract_mode=${config.rar_extract_mode}`,
    `window_width=${config.window_width || 1440}`,
    `window_height=${config.window_height || 960}`,
    `window_x=${config.window_x || -1}`,
    `window_y=${config.window_y || -1}`,
  ];
  return `${lines.join('\n')}\n`;
}

function saveConfig(config) {
  ensureDir(getAppDataDir());
  fs.writeFileSync(getConfigPath(), serializeConfig(config));
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
          connections: 4,
          ftp_connections: 10,
          use_temp: false,
          auto_tune_connections: true,
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
          case 'connections': currentProfile.connections = parseInt(value, 10) || 8; break;
        case 'ftp_connections': currentProfile.ftp_connections = parseInt(value, 10) || 10; break;
          case 'use_temp': currentProfile.use_temp = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
          case 'auto_tune_connections': currentProfile.auto_tune_connections = ['1', 'true', 'yes', 'on'].includes(value.toLowerCase()); break;
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
    lines.push(`ftp_connections=${profile.ftp_connections ?? 10}`);
    lines.push(`use_temp=${profile.use_temp}`);
    lines.push(`auto_tune_connections=${profile.auto_tune_connections}`);
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
    if (!Number.isSafeInteger(stat.size)) {
      throw new Error(`File too large for safe integer math: ${basePath}`);
    }
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
        if (entry.name.toLowerCase().endsWith('.asar')) {
          const stat = await fs.promises.stat(fullPath);
          if (!Number.isSafeInteger(stat.size)) {
            throw new Error(`File too large for safe integer math: ${fullPath}`);
          }
          files.push({
            rel_path: relPath.replace(/\\/g, '/'),
            abs_path: fullPath,
            size: Number(stat.size),
            mtime: Math.floor(stat.mtimeMs / 1000),
          });
          totalSize += BigInt(stat.size);
          continue;
        }
        await walk(fullPath, relPath);
      } else if (entry.isFile()) {
        const stat = await fs.promises.stat(fullPath);
        if (!Number.isSafeInteger(stat.size)) {
          throw new Error(`File too large for safe integer math: ${fullPath}`);
        }
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

async function sendFrameHeader(socket, frameType, length, cancel, log = () => {}) {
  const header = Buffer.alloc(16);
  header.writeUInt32LE(MAGIC_FTX1, 0);
  header.writeUInt32LE(frameType, 4);
  header.writeBigUInt64LE(BigInt(length), 8);
  await writeAllRetry(socket, header, cancel, log);
}

async function sendPackPayload(socket, frameType, payload, cancel, log) {
  await sendFrameHeader(socket, frameType, payload.length, cancel, log);
  await writeAllRetry(socket, payload, cancel, log);
}

function createPackAckReader(socket) {
  let ackBuffer = Buffer.alloc(0);
  let responseBuffer = Buffer.alloc(0);
  const waiters = new Map();
  const completed = new Set();

  const onData = (chunk) => {
    ackBuffer = Buffer.concat([ackBuffer, chunk]);
    while (ackBuffer.length >= 16) {
      const magic = ackBuffer.readUInt32LE(0);
      if (magic !== MAGIC_FTX1) {
        responseBuffer = Buffer.concat([responseBuffer, ackBuffer]);
        ackBuffer = Buffer.alloc(0);
        return;
      }
      const type = ackBuffer.readUInt32LE(4);
      const bodyLen = Number(ackBuffer.readBigUInt64LE(8));
      if (ackBuffer.length < 16 + bodyLen) return;
      const body = ackBuffer.subarray(16, 16 + bodyLen);
      ackBuffer = ackBuffer.subarray(16 + bodyLen);
      if (type === FrameType.PackAck && bodyLen >= 8) {
        const ackId = Number(body.readBigUInt64LE(0));
        const waiter = waiters.get(ackId);
        if (waiter) {
          waiters.delete(ackId);
          waiter();
        } else {
          completed.add(ackId);
        }
      } else {
        responseBuffer = Buffer.concat([responseBuffer, body]);
      }
    }
  };

  socket.on('data', onData);
  return {
    waitForAck(packId) {
      if (completed.has(packId)) {
        completed.delete(packId);
        return Promise.resolve();
      }
      return new Promise((resolve) => {
        waiters.set(packId, resolve);
      });
    },
    detach() {
      socket.off('data', onData);
    },
    getBufferedResponse() {
      if (ackBuffer.length > 0) {
        responseBuffer = Buffer.concat([responseBuffer, ackBuffer]);
        ackBuffer = Buffer.alloc(0);
      }
      return responseBuffer;
    },
  };
}

function buildV4SingleRecordPack(packId, relPathBytes, data, offset, totalSize, flags) {
  const dataLen = data.length;
  const hasOffset = (flags & RecordFlag.HasOffset) !== 0;
  const hasTotal = (flags & RecordFlag.HasTotal) !== 0;
  const extra = (hasOffset ? 8 : 0) + (hasTotal ? 8 : 0);
  const bodyLen = 8 + 4 + 2 + 2 + relPathBytes.length + 8 + extra + dataLen;
  const body = Buffer.allocUnsafe(bodyLen);
  let o = 0;
  body.writeBigUInt64LE(BigInt(packId), o); o += 8;
  body.writeUInt32LE(1, o); o += 4;
  body.writeUInt16LE(relPathBytes.length, o); o += 2;
  body.writeUInt16LE(flags, o); o += 2;
  relPathBytes.copy(body, o); o += relPathBytes.length;
  body.writeBigUInt64LE(BigInt(dataLen), o); o += 8;
  if (hasOffset) {
    body.writeBigUInt64LE(BigInt(offset), o);
    o += 8;
  }
  if (hasTotal) {
    body.writeBigUInt64LE(BigInt(totalSize), o);
    o += 8;
  }
  if (dataLen > 0) data.copy(body, o);
  return body;
}

function createPackAssembler() {
  const header = Buffer.alloc(12); // pack_id (8) + record_count (4)
  const parts = [header];
  let totalLen = header.length;
  let recordCount = 0;
  let bytesAdded = 0n;
  let filesAdded = 0;

  return {
    get length() {
      return totalLen;
    },
    get recordCount() {
      return recordCount;
    },
    get bytesAdded() {
      return bytesAdded;
    },
    get filesAdded() {
      return filesAdded;
    },
    addRecord(relPathBytes, data, meta = null) {
      const hasMeta = !!meta;
      const flags = hasMeta
        ? (
          RecordFlag.HasOffset |
          ((Number(meta.totalSize || 0) >= 0) ? RecordFlag.HasTotal : 0) |
          (meta.truncate ? RecordFlag.Truncate : 0)
        )
        : 0;
      const recordHeader = Buffer.alloc(2 + (hasMeta ? 2 : 0) + relPathBytes.length + 8 + (hasMeta ? 16 : 0));
      recordHeader.writeUInt16LE(relPathBytes.length, 0);
      let o = 2;
      if (hasMeta) {
        recordHeader.writeUInt16LE(flags, o);
        o += 2;
      }
      relPathBytes.copy(recordHeader, o);
      o += relPathBytes.length;
      recordHeader.writeBigUInt64LE(BigInt(data.length), o);
      o += 8;
      if (hasMeta) {
        recordHeader.writeBigUInt64LE(BigInt(Math.max(0, Number(meta.offset || 0))), o);
        o += 8;
        recordHeader.writeBigUInt64LE(BigInt(Math.max(0, Number(meta.totalSize || 0))), o);
      }
      parts.push(recordHeader, data);
      totalLen += recordHeader.length + data.length;
      bytesAdded += BigInt(data.length);
      recordCount += 1;
    },
    markFileDone() {
      filesAdded += 1;
    },
    toBuffer(packId) {
      header.writeBigUInt64LE(BigInt(packId), 0);
      header.writeUInt32LE(recordCount, 8);
      return Buffer.concat(parts, totalLen);
    },
  };
}

async function sendFilesPayloadV4(files, socketRef, options = {}) {
  const {
    cancel = { value: false },
    progress = () => {},
    log = () => {},
    compression = 'none',
    rateLimitBps = null,
    packLimitBytes = PACK_BUFFER_SIZE,
    streamChunkBytes = SEND_CHUNK_SIZE,
    deprioritize = false,
    getPackLimit,
    getPaceDelayMs,
    getRateLimitBps,
    reconnect,
    maxInflight = 8,
    maxRetries = 10,
    onSkipFile,
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

  const inflight = new Map();
  let packId = 1;
  let retries = 0;
  let responseBuffer = Buffer.alloc(0);
  let ackBuffer = Buffer.alloc(0);
  let ackWaiterResolve = null;

  const applyAdaptiveRate = () => {
    const nextLimit = rateLimitFn();
    if (typeof limiter.setLimitBps === 'function') {
      limiter.setLimitBps(nextLimit);
    } else {
      limiter.limitBps = nextLimit;
    }
  };

  const maybePace = async () => {
    const delayMs = await paceDelayFn();
    if (delayMs > 0) {
      await sleepMs(delayMs);
    }
  };

  const waitForAck = () => new Promise((resolve) => {
    ackWaiterResolve = resolve;
  });

  const onAck = (ackId) => {
    if (inflight.has(ackId)) {
      inflight.delete(ackId);
    }
    if (ackWaiterResolve && inflight.size < maxInflight) {
      const resolve = ackWaiterResolve;
      ackWaiterResolve = null;
      resolve();
    }
  };

  const attachAckReader = (socket) => {
    const onData = (chunk) => {
      ackBuffer = Buffer.concat([ackBuffer, chunk]);
      while (ackBuffer.length >= 16) {
        const magic = ackBuffer.readUInt32LE(0);
        if (magic !== MAGIC_FTX1) {
          responseBuffer = Buffer.concat([responseBuffer, ackBuffer]);
          ackBuffer = Buffer.alloc(0);
          return;
        }
        const type = ackBuffer.readUInt32LE(4);
        const bodyLen = Number(ackBuffer.readBigUInt64LE(8));
        if (ackBuffer.length < 16 + bodyLen) return;
        const body = ackBuffer.subarray(16, 16 + bodyLen);
        ackBuffer = ackBuffer.subarray(16 + bodyLen);
        if (type === FrameType.PackAck && bodyLen >= 8) {
          const ackId = Number(body.readBigUInt64LE(0));
          onAck(ackId);
        } else {
          responseBuffer = Buffer.concat([responseBuffer, body]);
        }
      }
    };
    socket.on('data', onData);
    return () => socket.off('data', onData);
  };

  let detachAckReader = attachAckReader(socketRef.current);

  const resendInflight = async () => {
    const entries = Array.from(inflight.entries()).sort((a, b) => a[0] - b[0]);
    for (const [, entry] of entries) {
      await sendPackPayload(socketRef.current, entry.frameType, entry.payload, cancel, log);
      limiter.throttle(entry.payload.length);
    }
  };

  const handleReconnect = async () => {
    if (!reconnect) throw new Error('Reconnect not available');
    retries += 1;
    if (retries > maxRetries) throw new Error('Upload retry limit reached');
    try {
      socketRef.current.destroy();
    } catch {}
    socketRef.current = await reconnect();
    detachAckReader();
    detachAckReader = attachAckReader(socketRef.current);
    await resendInflight();
  };

  let pack = createPackAssembler();

  const flushPack = async (lastFile = false) => {
    if (pack.recordCount === 0 && !lastFile) return;

    let frameType = FrameType.PackV4;
    let payload = pack.toBuffer(packId);

    if (compression !== 'none') {
      log(`Compressing pack (${compression})...`);
      let compressed;
      try {
        if (compression === 'lz4') {
          if (!lz4) throw new Error('lz4 module unavailable');
          compressed = lz4.encode(payload);
          frameType = FrameType.PackLz4V4;
        } else if (compression === 'zstd') {
          if (!fzstd) throw new Error('fzstd module unavailable');
          compressed = fzstd.compress(payload);
          frameType = FrameType.PackZstdV4;
        } else if (compression === 'lzma') {
          if (!lzma) throw new Error('lzma-native module unavailable');
          compressed = await lzma.compress(payload);
          frameType = FrameType.PackLzmaV4;
        }
      } catch (e) {
        log(`Compression failed (${compression}): ${e.message}. Sending uncompressed.`);
        compressed = null;
        frameType = FrameType.PackV4;
      }

      if (compressed && compressed.length < payload.length) {
        if (compression === 'zstd' || compression === 'lzma') {
          const originalSizeBuf = Buffer.alloc(4);
          originalSizeBuf.writeUInt32LE(payload.length, 0);
          payload = Buffer.concat([originalSizeBuf, compressed]);
        } else {
          payload = compressed;
        }
      } else {
        log(`Compressed size was larger or equal for ${compression}. Sending uncompressed.`);
        frameType = FrameType.PackV4;
      }
    }

    const currentPackId = packId;
    const entry = { payload, frameType, bytes: pack.bytesAdded, files: pack.filesAdded };
    inflight.set(currentPackId, entry);

    applyAdaptiveRate();
    try {
      await sendPackPayload(socketRef.current, frameType, payload, cancel, log);
    } catch (err) {
      await handleReconnect();
      await sendPackPayload(socketRef.current, frameType, payload, cancel, log);
    }

    limiter.throttle(payload.length);
    if (deprioritize) {
      await manageDeprioritize();
    }
    await maybePace();

    totalSentBytes += pack.bytesAdded;
    totalSentFiles += pack.filesAdded;
    packId += 1;

    const elapsed = (Date.now() - startTime) / 1000;
    progress(Number(totalSentBytes), totalSentFiles, elapsed, null);

    pack = createPackAssembler();

    while (inflight.size >= maxInflight) {
      await waitForAck();
    }
  };

  const logEachFile = files.length <= 10000;
  const logInterval = logEachFile ? 1 : Math.max(250, Math.floor(files.length / 200));
  const TINY_FILE_THRESHOLD = 64 * 1024;

  const shouldSkipReadError = (err) => {
    const code = err?.code;
    return code === 'ENOENT' || code === 'EACCES' || code === 'EPERM';
  };

  for (let i = 0; i < files.length; i++) {
    const file = files[i];
    if (cancel.value) {
      throw new Error('Upload cancelled by user');
    }

    if (logEachFile || i % logInterval === 0) {
      log(logEachFile ? `Packing: ${file.rel_path}` : `Packing... ${i + 1}/${files.length}`);
    }

    const relPathBytes = Buffer.from(file.rel_path, 'utf8');

    if (file.size < TINY_FILE_THRESHOLD) {
      let data;
      try {
        data = await fs.promises.readFile(file.abs_path);
      } catch (err) {
        if (shouldSkipReadError(err)) {
          log(`Skipping missing/unreadable file: ${file.rel_path}`);
          if (typeof onSkipFile === 'function') onSkipFile(file, err);
          continue;
        }
        throw err;
      }
      const overhead = 2 + 2 + relPathBytes.length + 8 + 8 + 8;
      const packLimit = clamp(packLimitFn(), PACK_BUFFER_MIN, PACK_BUFFER_SIZE);
      if (packLimit - pack.length <= overhead + data.length) {
        await flushPack();
      }
      pack.addRecord(relPathBytes, data, { offset: 0, totalSize: file.size, truncate: true });
      pack.markFileDone();
      const elapsed = (Date.now() - startTime) / 1000;
      progress(Number(totalSentBytes) + Number(pack.bytesAdded), totalSentFiles + pack.filesAdded, elapsed, file.rel_path);
      continue;
    }

    let sawData = false;
    let stream;
    try {
      stream = fs.createReadStream(file.abs_path, { highWaterMark: streamChunkBytes });
    } catch (err) {
      if (shouldSkipReadError(err)) {
        log(`Skipping missing/unreadable file: ${file.rel_path}`);
        if (typeof onSkipFile === 'function') onSkipFile(file, err);
        continue;
      }
      throw err;
    }

    try {
      for await (const chunk of stream) {
        if (cancel.value) {
          stream.destroy();
          throw new Error('Upload cancelled by user');
        }

        sawData = true;
        let offset = 0;
        let fileOffset = 0;
        let firstChunk = true;

        while (offset < chunk.length) {
          const overhead = 2 + 2 + relPathBytes.length + 8 + 8 + 8;
          const packLimit = clamp(packLimitFn(), PACK_BUFFER_MIN, PACK_BUFFER_SIZE);
          const remaining = packLimit - pack.length;

          if (remaining <= overhead) {
            await flushPack();
            continue;
          }

          const maxData = remaining - overhead;
          const sliceLen = Math.min(maxData, chunk.length - offset);
          const dataSlice = sliceLen === chunk.length ? chunk : chunk.slice(offset, offset + sliceLen);

          pack.addRecord(relPathBytes, dataSlice, {
            offset: fileOffset,
            totalSize: file.size,
            truncate: firstChunk,
          });
          offset += sliceLen;
          fileOffset += sliceLen;
          firstChunk = false;

          const elapsed = (Date.now() - startTime) / 1000;
          progress(Number(totalSentBytes) + Number(pack.bytesAdded), totalSentFiles + pack.filesAdded, elapsed, file.rel_path);
          if (deprioritize) {
            await manageDeprioritize();
          }
          await maybePace();
        }
      }
    } catch (err) {
      if (shouldSkipReadError(err) && !sawData) {
        log(`Skipping missing/unreadable file: ${file.rel_path}`);
        if (typeof onSkipFile === 'function') onSkipFile(file, err);
        continue;
      }
      throw err;
    }

    if (!sawData) {
      const overhead = 2 + 2 + relPathBytes.length + 8 + 8 + 8;
      const packLimit = clamp(packLimitFn(), PACK_BUFFER_MIN, PACK_BUFFER_SIZE);
      if (packLimit - pack.length <= overhead) {
        await flushPack();
      }
      pack.addRecord(relPathBytes, Buffer.alloc(0), { offset: 0, totalSize: 0, truncate: true });
    }

    pack.markFileDone();

    const elapsed = (Date.now() - startTime) / 1000;
    progress(Number(totalSentBytes) + Number(pack.bytesAdded), totalSentFiles + pack.filesAdded, elapsed, file.rel_path);
  }

  if (pack.recordCount > 0) {
    await flushPack(true);
  }

  while (inflight.size > 0) {
    await waitForAck();
  }

  await sendFrameHeader(socketRef.current, FrameType.Finish, 0, cancel, log);
  limiter.throttle(16);

  if (ackBuffer.length > 0) {
    responseBuffer = Buffer.concat([responseBuffer, ackBuffer]);
    ackBuffer = Buffer.alloc(0);
  }
  detachAckReader();
  return { files: totalSentFiles, bytes: Number(totalSentBytes), responseBuffer };
}

async function sendFilesPayloadV4Dynamic(getNextFile, socketRef, options = {}) {
  const {
    cancel = { value: false },
    progress = () => {},
    log = () => {},
    compression = 'none',
    rateLimitBps = null,
    packLimitBytes = PACK_BUFFER_SIZE,
    streamChunkBytes = SEND_CHUNK_SIZE,
    deprioritize = false,
    getPackLimit,
    getPaceDelayMs,
    getRateLimitBps,
    reconnect,
    maxInflight = 8,
    maxRetries = 10,
    onSkipFile,
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

  const inflight = new Map();
  let packId = 1;
  let retries = 0;
  let responseBuffer = Buffer.alloc(0);
  let ackBuffer = Buffer.alloc(0);
  let ackWaiterResolve = null;

  const applyAdaptiveRate = () => {
    const nextLimit = rateLimitFn();
    if (typeof limiter.setLimitBps === 'function') {
      limiter.setLimitBps(nextLimit);
    } else {
      limiter.limitBps = nextLimit;
    }
  };

  const maybePace = async () => {
    const delayMs = await paceDelayFn();
    if (delayMs > 0) {
      await sleepMs(delayMs);
    }
  };

  const waitForAck = () => new Promise((resolve) => {
    ackWaiterResolve = resolve;
  });

  const onAck = (ackId) => {
    if (inflight.has(ackId)) {
      inflight.delete(ackId);
    }
    if (ackWaiterResolve && inflight.size < maxInflight) {
      const resolve = ackWaiterResolve;
      ackWaiterResolve = null;
      resolve();
    }
  };

  const attachAckReader = (socket) => {
    const onData = (chunk) => {
      ackBuffer = Buffer.concat([ackBuffer, chunk]);
      while (ackBuffer.length >= 16) {
        const magic = ackBuffer.readUInt32LE(0);
        if (magic !== MAGIC_FTX1) {
          responseBuffer = Buffer.concat([responseBuffer, ackBuffer]);
          ackBuffer = Buffer.alloc(0);
          return;
        }
        const type = ackBuffer.readUInt32LE(4);
        const bodyLen = Number(ackBuffer.readBigUInt64LE(8));
        if (ackBuffer.length < 16 + bodyLen) return;
        const body = ackBuffer.subarray(16, 16 + bodyLen);
        ackBuffer = ackBuffer.subarray(16 + bodyLen);
        if (type === FrameType.PackAck && bodyLen >= 8) {
          const ackId = Number(body.readBigUInt64LE(0));
          onAck(ackId);
        } else {
          responseBuffer = Buffer.concat([responseBuffer, body]);
        }
      }
    };
    socket.on('data', onData);
    return () => socket.off('data', onData);
  };

  let detachAckReader = attachAckReader(socketRef.current);

  const resendInflight = async () => {
    const entries = Array.from(inflight.entries()).sort((a, b) => a[0] - b[0]);
    for (const [, entry] of entries) {
      await sendPackPayload(socketRef.current, entry.frameType, entry.payload, cancel, log);
      limiter.throttle(entry.payload.length);
    }
  };

  const handleReconnect = async () => {
    if (!reconnect) throw new Error('Reconnect not available');
    retries += 1;
    if (retries > maxRetries) throw new Error('Upload retry limit reached');
    try {
      socketRef.current.destroy();
    } catch {}
    socketRef.current = await reconnect();
    detachAckReader();
    detachAckReader = attachAckReader(socketRef.current);
    await resendInflight();
  };

  let pack = createPackAssembler();

  const flushPack = async (lastFile = false) => {
    if (pack.recordCount === 0 && !lastFile) return;

    let frameType = FrameType.PackV4;
    let payload = pack.toBuffer(packId);

    if (compression !== 'none') {
      log(`Compressing pack (${compression})...`);
      let compressed;
      try {
        if (compression === 'lz4') {
          if (!lz4) throw new Error('lz4 module unavailable');
          compressed = lz4.encode(payload);
          frameType = FrameType.PackLz4V4;
        } else if (compression === 'zstd') {
          if (!fzstd) throw new Error('fzstd module unavailable');
          compressed = fzstd.compress(payload);
          frameType = FrameType.PackZstdV4;
        } else if (compression === 'lzma') {
          if (!lzma) throw new Error('lzma-native module unavailable');
          compressed = await lzma.compress(payload);
          frameType = FrameType.PackLzmaV4;
        }
      } catch (e) {
        log(`Compression failed (${compression}): ${e.message}. Sending uncompressed.`);
        frameType = FrameType.PackV4;
        compressed = null;
      }
      if (compressed && compressed.length < payload.length) {
        if (compression === 'zstd' || compression === 'lzma') {
          const originalSizeBuf = Buffer.alloc(4);
          originalSizeBuf.writeUInt32LE(payload.length, 0);
          payload = Buffer.concat([originalSizeBuf, compressed]);
        } else {
          payload = compressed;
        }
      } else {
        log(`Compressed size was larger or equal for ${compression}. Sending uncompressed.`);
        frameType = FrameType.PackV4;
      }
    }

    const currentPackId = packId;
    const entry = { payload, frameType, bytes: pack.bytesAdded, files: pack.filesAdded };
    inflight.set(currentPackId, entry);

    applyAdaptiveRate();
    try {
      await sendPackPayload(socketRef.current, frameType, payload, cancel, log);
    } catch (err) {
      await handleReconnect();
      await sendPackPayload(socketRef.current, frameType, payload, cancel, log);
    }

    limiter.throttle(payload.length);
    if (deprioritize) {
      await manageDeprioritize();
    }
    await maybePace();

    totalSentBytes += pack.bytesAdded;
    totalSentFiles += pack.filesAdded;
    packId += 1;

    const elapsed = (Date.now() - startTime) / 1000;
    progress(Number(totalSentBytes), totalSentFiles, elapsed, null);

    pack = createPackAssembler();

    while (inflight.size >= maxInflight) {
      await waitForAck();
    }
  };

  const TINY_FILE_THRESHOLD = 64 * 1024;

  const shouldSkipReadError = (err) => {
    const code = err?.code;
    return code === 'ENOENT' || code === 'EACCES' || code === 'EPERM';
  };

  while (true) {
    const file = getNextFile();
    if (!file) break;
    if (cancel.value) throw new Error('Upload cancelled by user');
    await maybePace();

    const relPathBytes = Buffer.from(file.rel_path, 'utf8');

    if (file.size < TINY_FILE_THRESHOLD) {
      let data;
      try {
        data = await fs.promises.readFile(file.abs_path);
      } catch (err) {
        if (shouldSkipReadError(err)) {
          log(`Skipping missing/unreadable file: ${file.rel_path}`);
          if (typeof onSkipFile === 'function') onSkipFile(file, err);
          continue;
        }
        throw err;
      }
      const overhead = 2 + 2 + relPathBytes.length + 8 + 8 + 8;
      const packLimit = clamp(packLimitFn(), PACK_BUFFER_MIN, PACK_BUFFER_SIZE);
      if (packLimit - pack.length <= overhead + data.length) {
        await flushPack();
      }
      pack.addRecord(relPathBytes, data, { offset: 0, totalSize: file.size, truncate: true });
      pack.markFileDone();
      const elapsed = (Date.now() - startTime) / 1000;
      progress(Number(totalSentBytes) + Number(pack.bytesAdded), totalSentFiles + pack.filesAdded, elapsed, file.rel_path);
      continue;
    }

    let stream;
    try {
      stream = fs.createReadStream(file.abs_path, { highWaterMark: streamChunkBytes });
    } catch (err) {
      if (shouldSkipReadError(err)) {
        log(`Skipping missing/unreadable file: ${file.rel_path}`);
        if (typeof onSkipFile === 'function') onSkipFile(file, err);
        continue;
      }
      throw err;
    }

    try {
      for await (const chunk of stream) {
        if (cancel.value) {
          stream.destroy();
          throw new Error('Upload cancelled by user');
        }

        let offset = 0;
        let fileOffset = 0;
        let firstChunk = true;

        while (offset < chunk.length) {
          const overhead = 2 + 2 + relPathBytes.length + 8 + 8 + 8;
          const packLimit = clamp(packLimitFn(), PACK_BUFFER_MIN, PACK_BUFFER_SIZE);
          const remaining = packLimit - pack.length;

          if (remaining <= overhead) {
            await flushPack();
            continue;
          }

          const maxData = remaining - overhead;
          const sliceLen = Math.min(maxData, chunk.length - offset);
          const slice = chunk.subarray(offset, offset + sliceLen);
          offset += sliceLen;
          pack.addRecord(relPathBytes, slice, {
            offset: fileOffset,
            totalSize: file.size,
            truncate: firstChunk,
          });
          fileOffset += sliceLen;
          firstChunk = false;
          const elapsed = (Date.now() - startTime) / 1000;
          progress(Number(totalSentBytes) + Number(pack.bytesAdded), totalSentFiles + pack.filesAdded, elapsed, file.rel_path);
          if (deprioritize) {
            await manageDeprioritize();
          }
          await maybePace();
          if (pack.length >= packLimit) {
            await flushPack();
          }
        }
      }
    } catch (err) {
      if (shouldSkipReadError(err)) {
        log(`Skipping missing/unreadable file: ${file.rel_path}`);
        if (typeof onSkipFile === 'function') onSkipFile(file, err);
        continue;
      }
      throw err;
    }

    pack.markFileDone();
    const elapsed = (Date.now() - startTime) / 1000;
    progress(Number(totalSentBytes) + Number(pack.bytesAdded), totalSentFiles + pack.filesAdded, elapsed, file.rel_path);
  }

  if (pack.recordCount > 0) {
    await flushPack(true);
  }

  while (inflight.size > 0) {
    await waitForAck();
  }

  await sendFrameHeader(socketRef.current, FrameType.Finish, 0, cancel, log);
  limiter.throttle(16);

  if (ackBuffer.length > 0) {
    responseBuffer = Buffer.concat([responseBuffer, ackBuffer]);
    ackBuffer = Buffer.alloc(0);
  }
  detachAckReader();
  return { files: totalSentFiles, bytes: Number(totalSentBytes), responseBuffer };
}

function splitFilesForWorkers(files, workerCount) {
  const groups = Array.from({ length: workerCount }, () => []);
  const totals = new Array(workerCount).fill(0);
  const sorted = [...files].sort((a, b) => b.size - a.size);
  for (const file of sorted) {
    let best = 0;
    for (let i = 1; i < workerCount; i++) {
      if (totals[i] < totals[best]) best = i;
    }
    groups[best].push(file);
    totals[best] += Number(file.size) || 0;
  }
  return groups.filter((g) => g.length > 0);
}

async function runPayloadUploadParallelFiles(files, opts = {}) {
  const {
    connections,
    uploadInit,
    cancel,
    compression,
    rateLimitBps,
    packLimitBytes,
    streamChunkBytes,
    extraPaceMs,
    onProgress,
    onSkipFile,
    log,
  } = opts;
  const workerGroups = splitFilesForWorkers(files, connections);
  const workerBytes = new Array(workerGroups.length).fill(0);
  const workerFiles = new Array(workerGroups.length).fill(0);

  await Promise.all(workerGroups.map(async (group, idx) => {
    let socket = null;
    try {
      socket = await uploadInit();
      const socketRef = { current: socket };
      const result = await sendFilesPayloadV4(group, socketRef, {
        cancel,
        compression,
        rateLimitBps,
        packLimitBytes,
        streamChunkBytes,
        getPaceDelayMs: async () => extraPaceMs || 0,
        reconnect: uploadInit,
        onSkipFile,
        log,
        progress: (sent, filesSent, elapsed, currentFile) => {
          workerBytes[idx] = Number(sent) || 0;
          workerFiles[idx] = Number(filesSent) || 0;
          const totalSent = workerBytes.reduce((sum, v) => sum + v, 0);
          const totalFiles = workerFiles.reduce((sum, v) => sum + v, 0);
          onProgress(totalSent, totalFiles, currentFile || '');
        },
      });
      const response = await readUploadResponse(socketRef.current, { value: false }, result.responseBuffer);
      parseUploadResponse(response);
    } finally {
      if (socket) {
        try {
          socket.destroy();
        } catch {}
      }
    }
  }));
}

async function runPayloadUploadParallelSingleFileV4(file, opts = {}) {
  const {
    connections,
    uploadInit,
    cancel,
    onProgress,
  } = opts;

  const relPathBytes = Buffer.from(file.rel_path, 'utf8');
  const totalSize = Number(file.size) || 0;
  if (totalSize <= 0) {
    onProgress(0, 1, file.rel_path);
    return;
  }

  const chunkSize = clamp(Math.ceil(totalSize / (connections * 8)), 4 * 1024 * 1024, 32 * 1024 * 1024);
  const chunks = [];
  for (let offset = 0; offset < totalSize; offset += chunkSize) {
    const len = Math.min(chunkSize, totalSize - offset);
    chunks.push({ offset, len });
  }

  const workerQueues = Array.from({ length: connections }, () => []);
  for (let i = 0; i < chunks.length; i++) {
    workerQueues[i % connections].push(chunks[i]);
  }
  const activeQueues = workerQueues.filter((q) => q.length > 0);
  const progressBytes = new Array(activeQueues.length).fill(0);

  const setupSocket = await uploadInit();
  const setupReader = createPackAckReader(setupSocket);
  try {
    const setupPack = buildV4SingleRecordPack(
      0,
      relPathBytes,
      Buffer.alloc(0),
      0,
      totalSize,
      RecordFlag.HasOffset | RecordFlag.HasTotal | RecordFlag.Truncate
    );
    await sendPackPayload(setupSocket, FrameType.PackV4, setupPack, cancel, () => {});
    await setupReader.waitForAck(0);
    await sendFrameHeader(setupSocket, FrameType.Finish, 0, cancel, () => {});
    setupReader.detach();
    await readUploadResponse(setupSocket, { value: false }, setupReader.getBufferedResponse());
  } finally {
    try {
      setupSocket.destroy();
    } catch {}
  }

  await Promise.all(activeQueues.map(async (queue, workerIdx) => {
    let socket = null;
    let fd = null;
    try {
      socket = await uploadInit();
      const ackReader = createPackAckReader(socket);
      fd = await fs.promises.open(file.abs_path, 'r');
      let packId = 1;
      for (const chunk of queue) {
        if (cancel.value) throw new Error('Upload cancelled by user');
        const buf = Buffer.allocUnsafe(chunk.len);
        const { bytesRead } = await fd.read(buf, 0, chunk.len, chunk.offset);
        const payload = bytesRead === chunk.len ? buf : buf.subarray(0, bytesRead);
        const body = buildV4SingleRecordPack(
          packId,
          relPathBytes,
          payload,
          chunk.offset,
          totalSize,
          RecordFlag.HasOffset | RecordFlag.HasTotal
        );
        await sendPackPayload(socket, FrameType.PackV4, body, cancel, () => {});
        await ackReader.waitForAck(packId);
        packId += 1;
        progressBytes[workerIdx] += payload.length;
        const sent = progressBytes.reduce((sum, v) => sum + v, 0);
        onProgress(sent, 1, file.rel_path);
      }
      await sendFrameHeader(socket, FrameType.Finish, 0, cancel, () => {});
      ackReader.detach();
      await readUploadResponse(socket, { value: false }, ackReader.getBufferedResponse());
    } finally {
      if (fd) await fd.close().catch(() => {});
      if (socket) {
        try {
          socket.destroy();
        } catch {}
      }
    }
  }));
}

function readLineFromSocket(socket, timeoutMs = READ_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);
    let settled = false;
    const cleanup = () => {
      clearTimeout(timer);
      socket.off('data', onData);
      socket.off('error', onErr);
      socket.off('close', onClose);
    };
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(new Error('Read timed out'));
    }, timeoutMs);

    const onData = (data) => {
      if (settled) return;
      buffer = Buffer.concat([buffer, data]);
      const idx = buffer.indexOf(0x0a);
      if (idx >= 0) {
        settled = true;
        cleanup();
        resolve(buffer.subarray(0, idx).toString('utf8').trim());
      }
    };
    const onErr = (err) => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(err);
    };
    const onClose = () => {
      if (settled) return;
      settled = true;
      cleanup();
      // If we got partial data, return it; otherwise error
      if (buffer.length > 0) {
        resolve(buffer.toString('utf8').trim());
      } else {
        reject(new Error('Socket closed before response'));
      }
    };
    socket.on('data', onData);
    socket.on('error', onErr);
    socket.on('close', onClose);
  });
}

function createSocketLineReader(socket) {
  let buffer = Buffer.alloc(0);
  const pending = [];
  let socketError = null;

  const flush = () => {
    while (pending.length > 0) {
      const idx = buffer.indexOf(0x0a);
      if (idx < 0) return;
      const line = buffer.subarray(0, idx).toString('utf8').trim();
      buffer = buffer.subarray(idx + 1);
      const { resolve } = pending.shift();
      resolve(line);
    }
  };

  const onData = (data) => {
    buffer = Buffer.concat([buffer, data]);
    flush();
  };
  const onErr = (err) => {
    socketError = err;
    while (pending.length > 0) {
      const { reject } = pending.shift();
      reject(err);
    }
  };
  const onClose = () => {
    if (buffer.length > 0 && pending.length > 0) {
      const line = buffer.toString('utf8').trim();
      buffer = Buffer.alloc(0);
      const { resolve } = pending.shift();
      resolve(line);
    }
    while (pending.length > 0) {
      const { reject } = pending.shift();
      reject(new Error('Socket closed before response'));
    }
  };

  socket.on('data', onData);
  socket.on('error', onErr);
  socket.on('close', onClose);

  return {
    readLine: (timeoutMs = READ_TIMEOUT_MS) => new Promise((resolve, reject) => {
      if (socketError) return reject(socketError);
      const idx = buffer.indexOf(0x0a);
      if (idx >= 0) {
        const line = buffer.subarray(0, idx).toString('utf8').trim();
        buffer = buffer.subarray(idx + 1);
        return resolve(line);
      }
      const timer = setTimeout(() => {
        const i = pending.findIndex((p) => p.resolve === resolve);
        if (i >= 0) pending.splice(i, 1);
        reject(new Error('Read timed out'));
      }, timeoutMs);
      pending.push({
        resolve: (line) => {
          clearTimeout(timer);
          resolve(line);
        },
        reject: (err) => {
          clearTimeout(timer);
          reject(err);
        },
      });
    }),
  };
}

function getStorageRootFromPath(destPath) {
  const normalized = String(destPath || '').replace(/\\/g, '/').trim();
  if (!normalized.startsWith('/')) return null;
  if (normalized === '/data' || normalized.startsWith('/data/')) return '/data';
  if (normalized.startsWith('/mnt/')) {
    const parts = normalized.split('/').filter(Boolean);
    if (parts.length >= 2) return `/${parts[0]}/${parts[1]}`;
  }
  return null;
}

function buildTempRootForArchive(destPath, tempRootOverride) {
  const override = typeof tempRootOverride === 'string' ? tempRootOverride.trim() : '';
  if (override) {
    if (override.endsWith('/ps5upload/tmp')) return override;
    return `${override.replace(/\/+$/, '')}/ps5upload/tmp`;
  }
  const root = getStorageRootFromPath(destPath) || '/data';
  return `${root}/ps5upload/tmp`;
}

async function uploadArchiveFastAndExtract(ip, rarPath, destPath, opts = {}) {
  const {
    cancel = { value: false },
    onProgress = () => {},
    onLog = () => {},
    tempRoot = '',
    chmodAfterUpload = false,
    queueExtract: queueExtraction = true,
  } = opts;
  const fileStat = await fs.promises.stat(rarPath);
  const fileSize = fileStat.size;
  if (!Number.isSafeInteger(fileSize)) {
    throw new Error(`RAR file too large for safe integer math: ${fileSize}`);
  }
  const cleanedTempRoot = typeof tempRoot === 'string' ? tempRoot.trim() : '';
  if (cleanedTempRoot && /\s/.test(cleanedTempRoot)) {
    throw new Error('Temp storage path must not contain spaces.');
  }
  const tempRootPath = buildTempRootForArchive(destPath, cleanedTempRoot);
  const tempDir = `${tempRootPath.replace(/\/+$/, '')}/rar_${Date.now()}_${Math.floor(Math.random() * 1e6)}`;
  const rarName = path.basename(rarPath);
  const rarRemotePath = `${tempDir}/${rarName}`;

  onLog(`Archive temp dir: ${tempDir}`);
  onLog(`Archive remote path: ${rarRemotePath}`);
  await createPath(ip, TRANSFER_PORT, tempDir);

  const file = {
    rel_path: rarName,
    abs_path: rarPath,
    size: fileSize,
    mtime: Math.floor(fileStat.mtimeMs / 1000),
  };

  let lastProgressAt = Date.now();
  let stallLogAt = 0;
  const progressWatch = setInterval(() => {
    const now = Date.now();
    if (now - lastProgressAt > 30000 && now - stallLogAt > 30000) {
      stallLogAt = now;
      onLog(`Archive upload: no progress for ${Math.round((now - lastProgressAt) / 1000)}s (source read or payload READY delay).`);
    }
  }, 5000);

  const reportProgress = (phase, sent, total, current) => {
    lastProgressAt = Date.now();
    onProgress(phase, sent, total, current);
  };

  let shouldCleanup = true;
  try {
    if (fileSize >= LANE_MIN_FILE_SIZE) {
      const laneChunk = getLaneChunkSize(fileSize);
      const totalChunks = Math.ceil(fileSize / laneChunk);
      onLog(`Archive fast path: connection mode (${LANE_CONNECTIONS} connections, ${formatBytes(laneChunk)} chunks, ${totalChunks} total).`);
      await runPayloadUploadLaneSingleFile(file, {
        ip,
        destPath: tempDir,
        connections: LANE_CONNECTIONS,
        chunkSize: laneChunk,
        cancel,
        chmodAfterUpload,
        log: onLog,
        onProgress: (sent) => reportProgress('Upload', sent, fileSize, rarName),
      });
    } else {
      onLog('Archive fast path: single-stream payload upload.');
      await runPayloadUploadFastMultiFile([file], {
        ip,
        destPath: tempDir,
        connections: 1,
        cancel,
        chmodAfterUpload,
        log: onLog,
        onProgress: (sent) => reportProgress('Upload', sent, fileSize, rarName),
      });
    }

    if (queueExtraction) {
      const queuedId = await queueExtract(ip, TRANSFER_PORT, rarRemotePath, destPath, {
        cleanupPath: tempDir,
        deleteSource: true,
      });
      onLog(`Extraction queued (ID ${queuedId}).`);
      shouldCleanup = false;
      return { fileSize, bytes: fileSize, files: 1, queuedId };
    }

    await extractArchiveWithProgress(
      ip,
      rarRemotePath,
      destPath,
      cancel,
      (processed, total, currentFile) => reportProgress('Extract', processed, total, currentFile),
      onLog
    );
  } finally {
    clearInterval(progressWatch);
    if (shouldCleanup) {
      for (let attempt = 1; attempt <= 3; attempt++) {
        try { await deletePath(ip, TRANSFER_PORT, rarRemotePath); break; } catch (err) {
          if (attempt === 3) onLog(`Cleanup failed for archive: ${err?.message || err}`);
          else await sleepMs(200 * attempt);
        }
      }
      for (let attempt = 1; attempt <= 3; attempt++) {
        try { await deletePath(ip, TRANSFER_PORT, tempDir); break; } catch (err) {
          if (attempt === 3) onLog(`Cleanup failed for temp dir: ${err?.message || err}`);
          else await sleepMs(200 * attempt);
        }
      }
    }
  }
  return { fileSize, bytes: fileSize, files: 1, queuedId: null };
}

async function precreateRemoteDirectories(ip, destRoot, files, opts = {}) {
  if (!Array.isArray(files) || files.length === 0) return { total: 0, created: 0, skipped: 0 };
  const cancel = opts.cancel || { value: false };
  const log = opts.log;

  const dirSet = new Set();
  for (const file of files) {
    const rel = String(file?.rel_path || '').replace(/\\/g, '/');
    if (!rel) continue;
    let dir = path.posix.dirname(rel);
    if (!dir || dir === '.') continue;
    dirSet.add(dir);
  }
  const dirs = Array.from(dirSet).sort((a, b) => a.length - b.length);
  if (dirs.length === 0) return { total: 0, created: 0, skipped: 0 };

  if (dirs.length > PRECREATE_MAX_DIRS) {
    log?.(`Pre-create: skipping ${dirs.length} directories (exceeds limit ${PRECREATE_MAX_DIRS}).`, 'info');
    return { total: dirs.length, created: 0, skipped: dirs.length };
  }

  log?.(`Pre-create: creating ${dirs.length} directories...`, 'info');
  const total = dirs.length;
  const logInterval = Math.max(1, Math.floor(total / 10));
  let created = 0;
  let failed = 0;

  const createOne = async (dir, idx) => {
    if (cancel.value) throw new Error('Upload cancelled');
    const remoteDir = joinRemotePath(destRoot, dir);
    try {
      await createPath(ip, TRANSFER_PORT, remoteDir);
      created += 1;
    } catch (err) {
      failed += 1;
      if (failed <= 3 && log) {
        log(`Pre-create: failed to create ${remoteDir}: ${err.message || err}`, 'warn');
      }
    } finally {
      const done = created + failed;
      if (done % logInterval === 0 || done === total) {
        log?.(`Pre-create: ${done}/${total} directories processed.`, 'info');
      }
    }
  };

  await mapWithConcurrency(dirs, PRECREATE_DIR_CONCURRENCY, createOne);
  if (failed > 0) {
    log?.(`Pre-create: done (${created} created, ${failed} failed).`, 'warn');
  } else {
    log?.(`Pre-create: done (${created} created).`, 'info');
  }
  return { total, created, skipped: failed };
}

function getLaneChunkSize(totalSize) {
  if (totalSize >= LANE_HUGE_FILE_BYTES) return LANE_HUGE_CHUNK_BYTES;
  if (totalSize >= LANE_LARGE_FILE_BYTES) return LANE_LARGE_CHUNK_BYTES;
  return LANE_DEFAULT_CHUNK_BYTES;
}

function getMadMaxChunkSize(totalSize) {
  if (totalSize >= LANE_HUGE_FILE_BYTES) return MAD_MAX_HUGE_CHUNK_BYTES;
  if (totalSize >= LANE_LARGE_FILE_BYTES) return MAD_MAX_LARGE_CHUNK_BYTES;
  return MAD_MAX_DEFAULT_CHUNK_BYTES;
}

async function runPayloadUploadLaneSingleFile(file, opts = {}) {
  const {
    ip,
    destPath,
    connections = LANE_CONNECTIONS,
    chunkSize = LANE_LARGE_CHUNK_BYTES,
    cancel = { value: false },
    chmodAfterUpload = false,
    onProgress = () => {},
    log = () => {},
  } = opts;

  const totalSize = Number(file?.size || 0);
  if (!ip || !destPath || !file?.abs_path || !file?.rel_path || totalSize < 0) {
    throw new Error('Connection upload: invalid parameters');
  }
  if (totalSize === 0) {
    onProgress(0, 1, file.rel_path);
    return;
  }

  const chunks = [];
  for (let offset = 0; offset < totalSize; offset += chunkSize) {
    const len = Math.min(chunkSize, totalSize - offset);
    chunks.push({ offset, len });
  }
  if (typeof log === 'function') {
    log(`Connection upload: ${connections} connections, ${formatBytes(chunkSize)} chunks, ${chunks.length} total for ${file.rel_path}.`);
  }

  const workerQueues = Array.from({ length: connections }, () => []);
  for (let i = 0; i < chunks.length; i += 1) {
    workerQueues[i % connections].push(chunks[i]);
  }
  const activeQueues = workerQueues.filter((q) => q.length > 0);
  const workerProgress = new Array(activeQueues.length).fill(0);

  let preallocResolved = false;
  let preallocResolve = null;
  let preallocReject = null;
  const preallocPromise = new Promise((resolve, reject) => { preallocResolve = resolve; preallocReject = reject; });

  const waitForPrealloc = async () => {
    if (preallocResolved) return;
    await preallocPromise;
  };

  let firstReadyLogged = false;
  let firstReadLogged = false;
  const runWorker = async (queue, idx) => {
    const socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
    tuneUploadSocket(socket);
    const reader = createSocketReader(socket);
    const fd = await fs.promises.open(file.abs_path, 'r');
    try {
      for (const chunk of queue) {
        if (cancel?.value) throw new Error('Upload cancelled by user');
        if (chunk.offset !== 0) await waitForPrealloc();

        const remotePath = joinRemotePath(destPath, file.rel_path);
        const startPayload = buildUploadStartPayload(remotePath, totalSize, chunk.offset);
        if (typeof log === 'function') {
          log(`Conn ${idx + 1}/${activeQueues.length}: start chunk @ ${formatBytes(chunk.offset)} (${formatBytes(chunk.len)})`);
        }
        await writeBinaryCommand(socket, UploadCmd.StartUpload, startPayload, cancel, log);

        const readyStart = Date.now();
        const readyResp = await readBinaryResponse(reader, READ_TIMEOUT_MS);
        const readyMs = Date.now() - readyStart;
        if (readyResp.code !== UploadResp.Ready) {
          const msg = readyResp.data?.length ? readyResp.data.toString('utf8') : 'no response';
          throw new Error(`Connection offset rejected: ${msg}`);
        }
        if (!firstReadyLogged) {
          firstReadyLogged = true;
          if (typeof log === 'function' && readyMs > 1500) {
            log(`Connection ${idx + 1}/${activeQueues.length}: READY took ${(readyMs / 1000).toFixed(2)}s`);
          }
        }
        if (chunk.offset === 0 && !preallocResolved) {
          preallocResolved = true;
          preallocResolve();
        }

        let remaining = chunk.len;
        let pos = chunk.offset;
        const ioBuf = Buffer.allocUnsafe(5 + 8 * 1024 * 1024);
        while (remaining > 0) {
          if (cancel?.value) throw new Error('Upload cancelled by user');
          const take = Math.min(ioBuf.length - 5, remaining);
          const readStart = Date.now();
          const { bytesRead } = await fd.read(ioBuf, 5, take, pos);
          const readMs = Date.now() - readStart;
          if (bytesRead <= 0) throw new Error('Connection offset read failed');
          if (!firstReadLogged && typeof log === 'function' && readMs > 1500) {
            firstReadLogged = true;
            log(`Connection ${idx + 1}/${activeQueues.length}: first read took ${(readMs / 1000).toFixed(2)}s`);
          }
          ioBuf[0] = UploadCmd.UploadChunk;
          ioBuf.writeUInt32LE(bytesRead, 1);
          await writeAllRetry(socket, ioBuf.subarray(0, 5 + bytesRead), cancel || { value: false }, log);
          remaining -= bytesRead;
          pos += bytesRead;
          workerProgress[idx] += bytesRead;
          const sent = workerProgress.reduce((sum, v) => sum + v, 0);
          onProgress(sent, 1, file.rel_path);
        }

        await writeBinaryCommand(socket, UploadCmd.EndUpload, Buffer.alloc(0), cancel, log);
        const endResp = await readBinaryResponse(reader, READ_TIMEOUT_MS);
        if (endResp.code !== UploadResp.Ok) {
          const msg = endResp.data?.length ? endResp.data.toString('utf8') : 'unknown response';
          throw new Error(`Connection offset failed: ${msg}`);
        }

        // Chunk complete; progress already updated during streaming
      }
    } catch (err) {
      if (!preallocResolved) { preallocResolved = true; preallocReject(err); }
      throw err;
    } finally {
      await fd.close().catch(() => {});
      try {
        reader.close();
        socket.destroy();
      } catch {}
    }
  };

  await Promise.all(activeQueues.map((queue, idx) => runWorker(queue, idx)));
}

async function runPayloadUploadFastMultiFile(files, opts = {}) {
  const {
    ip,
    destPath,
    connections = 8,
    cancel = { value: false },
    chmodAfterUpload = false,
    onProgress = () => {},
    log = () => {},
    onSkipFile,
  } = opts;

  // Sort files: large files first for better parallelism
  const sorted = [...files].sort((a, b) => b.size - a.size);

  // Note: No need to pre-create directories — handle_upload_fast_wrapper on
  // the payload side calls mkdir_p() for each file's parent directory automatically.

  // Lane lock: when one worker enters lane mode (large file), others wait
  // before picking up their next file to avoid connection explosion.
  let laneLocked = false;
  let laneWaiters = [];
  const acquireLaneLock = async () => {
    while (laneLocked) {
      await new Promise((resolve) => laneWaiters.push(resolve));
    }
    laneLocked = true;
  };
  const releaseLaneLock = () => {
    laneLocked = false;
    const waiters = laneWaiters;
    laneWaiters = [];
    for (const resolve of waiters) resolve();
  };
  const waitIfLaneBusy = async () => {
    while (laneLocked) {
      await new Promise((resolve) => laneWaiters.push(resolve));
    }
  };

  // Connection pool
  let fileIndex = 0;
  let totalSent = 0;
  let completedFiles = 0;

  const uploadOneFile = async (file, idx) => {
    const socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
    tuneUploadSocket(socket);
    const reader = createSocketReader(socket);

    try {
      const remotePath = joinRemotePath(destPath, file.rel_path);
      const startPayload = buildUploadStartPayload(remotePath, file.size, 0);
      await writeBinaryCommand(socket, UploadCmd.StartUpload, startPayload, cancel, log);

      // Wait for READY
      const readyResp = await readBinaryResponse(reader, READ_TIMEOUT_MS);
      if (readyResp.code !== UploadResp.Ready) {
        const msg = readyResp.data?.length ? readyResp.data.toString('utf8') : 'no response';
        throw new Error(`Upload rejected: ${msg}`);
      }

      // Stream file data with 8MB buffer
      if (file.size > 0) {
        const fd = await fs.promises.open(file.abs_path, 'r');
        try {
          const ioBuf = Buffer.allocUnsafe(5 + 8 * 1024 * 1024);
          let remaining = file.size;
          let pos = 0;
          while (remaining > 0) {
            if (cancel.value) throw new Error('Upload cancelled');
            const take = Math.min(ioBuf.length - 5, remaining);
            const { bytesRead } = await fd.read(ioBuf, 5, take, pos);
            if (bytesRead <= 0) throw new Error('Read failed');
            ioBuf[0] = UploadCmd.UploadChunk;
            ioBuf.writeUInt32LE(bytesRead, 1);
            await writeAllRetry(socket, ioBuf.subarray(0, 5 + bytesRead), cancel, log);
            remaining -= bytesRead;
            pos += bytesRead;
            totalSent += bytesRead;
            onProgress(totalSent, completedFiles, file.rel_path);
          }
        } finally {
          await fd.close().catch(() => {});
        }
      }

      // Wait for OK
      await writeBinaryCommand(socket, UploadCmd.EndUpload, Buffer.alloc(0), cancel, log);
      const endResp = await readBinaryResponse(reader, READ_TIMEOUT_MS);
      if (endResp.code !== UploadResp.Ok) {
        const msg = endResp.data?.length ? endResp.data.toString('utf8') : 'unknown response';
        throw new Error(`Upload failed: ${msg}`);
      }
      completedFiles++;
    } finally {
      try { socket.destroy(); } catch {}
      try { reader.close(); } catch {}
    }
  };

  // Run workers - each worker takes next file from queue
  const workers = [];
  for (let w = 0; w < Math.min(connections, sorted.length); w++) {
    workers.push((async () => {
      while (true) {
        if (cancel.value) break;
        await waitIfLaneBusy();
        const idx = fileIndex++;
        if (idx >= sorted.length) break;
        const file = sorted[idx];
        try {
          if (Number(file.size || 0) >= LANE_MIN_FILE_SIZE) {
            await acquireLaneLock();
            try {
              const laneBaseBytes = totalSent;
              const laneChunkSize = getLaneChunkSize(Number(file.size));
              log(`Multi-file lane upload: ${file.rel_path} (${formatBytes(file.size)}, ${connections} connections)`);
              await runPayloadUploadLaneSingleFile(file, {
                ip,
                destPath,
                connections,
                chunkSize: laneChunkSize,
                cancel,
                chmodAfterUpload,
                log,
                onProgress: (sent) => {
                  totalSent = laneBaseBytes + sent;
                  onProgress(totalSent, completedFiles, file.rel_path);
                },
              });
              completedFiles++;
            } finally {
              releaseLaneLock();
            }
          } else {
            await uploadOneFile(file, idx);
          }
        } catch (err) {
          if (err.code === 'ENOENT' || err.code === 'EACCES') {
            log(`Skipping: ${file.rel_path}`);
            if (typeof onSkipFile === 'function') onSkipFile(file, err);
            continue;
          }
          throw err;
        }
      }
    })());
  }

  await Promise.all(workers);
}

async function runPayloadUploadMadMaxSingleFile(file, opts = {}) {
  const {
    ip,
    destPath,
    cancel,
    chmodAfterUpload = false,
    onProgress = () => {},
    log = () => {},
  } = opts;

  const totalSize = Number(file?.size || 0);
  if (!ip || !destPath || !file?.abs_path || !file?.rel_path || totalSize < 0) {
    throw new Error('Mad Max upload: invalid parameters');
  }
  const workerCount = MAD_MAX_WORKERS;
  const chunkSize = getMadMaxChunkSize(totalSize);
  await runPayloadUploadLaneSingleFile(file, {
    ip,
    destPath,
    connections: workerCount,
    chunkSize,
    cancel,
    chmodAfterUpload,
    onProgress,
    log,
  });
}

function createAdaptiveUploadTuner({ ip, basePackLimit, baseChunkSize, userRateLimitBps, log, runId, allowPayloadTune }) {
  const state = {
    packLimit: basePackLimit,
    chunkSize: baseChunkSize,
    paceMs: 0,
    rateLimitBps: userRateLimitBps,
    stableTicks: 0,
    lastBackpressureEvents: 0,
    lastBackpressureWaitMs: 0,
    lastMode: '',
    hardPauseUntil: 0,
    lastPayloadTune: '',
  };

  let timer = null;

  const applyStatus = (status) => {
    if (!status || !status.transfer) return;

    if (state.hardPauseUntil > Date.now()) {
      // Still in hard pause, do nothing
      return;
    }
    if (state.hardPauseUntil > 0) {
      emit('transfer_log', { run_id: runId, key: 'log.payload.recovered' });
      state.hardPauseUntil = 0;
    }

    const transfer = status.transfer || {};
    const queueCount = Number(transfer.queue_count || 0);
    const packInUse = Number(transfer.pack_in_use || 0);
    const backpressureEvents = Number(transfer.backpressure_events || 0);
    const backpressureWaitMs = Number(transfer.backpressure_wait_ms || 0);
    const packQueueCount = Number(transfer.pack_queue_count || 0);
    const recvRateBps = Number(transfer.recv_rate_bps || 0);
    const writeRateBps = Number(transfer.write_rate_bps || 0);
    const recommendPack = Number(transfer.recommend_pack_limit || 0);
    const recommendPace = Number(transfer.recommend_pace_ms || 0);
    const recommendRate = Number(transfer.recommend_rate_limit_bps || 0);
    const tuneLevel = Number(transfer.tune_level ?? -1);
    const deltaEvents = backpressureEvents - state.lastBackpressureEvents;
    const deltaWait = backpressureWaitMs - state.lastBackpressureWaitMs;
    state.lastBackpressureEvents = backpressureEvents;
    state.lastBackpressureWaitMs = backpressureWaitMs;

    let pressure = 0;
    // queue_count is a deep write queue (up to thousands), so tiny thresholds over-throttle.
    if (queueCount >= 15000) pressure += 2;
    else if (queueCount >= 12000) pressure += 1;

    // pack_queue_count is a shallow queue (~16); treat near-full as real pressure.
    if (packQueueCount >= 15) pressure += 3;
    else if (packQueueCount >= 14) pressure += 2;
    else if (packQueueCount >= 13) pressure += 1;

    if (packInUse >= 15) pressure += 1;
    if (deltaEvents > 0 || deltaWait > 1500) pressure += 1;
    if (writeRateBps > 0 && recvRateBps > writeRateBps * 3.0) pressure += 1;
    if (typeof transfer.last_progress === 'number' && transfer.last_progress > 0) {
      const ageSec = Math.max(0, Date.now() / 1000 - transfer.last_progress);
      if (ageSec > 10) pressure += 1;
    }

    if (pressure >= 5) { // Critical pressure
      emit('transfer_log', { run_id: runId, key: 'log.payload.criticalPressure', params: { seconds: 2 } });
      state.hardPauseUntil = Date.now() + 2000;
      state.packLimit = clamp(Math.floor(state.packLimit * 0.5), PACK_BUFFER_MIN, basePackLimit);
      state.paceMs = clamp(state.paceMs + 20, 0, 80);
      state.stableTicks = 0;
    } else if (pressure >= 3) { // High pressure
      emit('transfer_log', { run_id: runId, key: 'log.payload.highPressure' });
      state.packLimit = clamp(Math.floor(state.packLimit * 0.7), PACK_BUFFER_MIN, basePackLimit);
      state.paceMs = clamp(state.paceMs + 8, 0, 60);
      state.stableTicks = 0;
    } else if (pressure >= 2) { // Low pressure
      emit('transfer_log', { run_id: runId, key: 'log.payload.pressure' });
      state.packLimit = clamp(Math.floor(state.packLimit * 0.95), PACK_BUFFER_MIN, basePackLimit);
      state.paceMs = clamp(Math.max(state.paceMs, 1), 0, 40);
      state.stableTicks = 0;
    } else { // No pressure
      state.stableTicks += 1;
      if (state.stableTicks >= 2) {
        state.paceMs = clamp(state.paceMs - 2, 0, 16);
        state.packLimit = clamp(state.packLimit + 2 * 1024 * 1024, PACK_BUFFER_MIN, basePackLimit);
      }
    }

    const payloadTuneAllowed = allowPayloadTune || tuneLevel >= 2;
    if (payloadTuneAllowed && tuneLevel >= 2 && recommendPack > 0) {
      state.packLimit = Math.min(state.packLimit, recommendPack);
    }
    if (payloadTuneAllowed && tuneLevel >= 2 && recommendPace > state.paceMs) {
      state.paceMs = recommendPace;
    }
    if (userRateLimitBps) {
      state.rateLimitBps = userRateLimitBps;
    } else {
      state.rateLimitBps = null;
    }

    const nextMode = `${state.packLimit}-${state.paceMs}-${state.rateLimitBps || 0}`;
    if (nextMode !== state.lastMode) {
      log(
        `Adaptive tune: pack=${(state.packLimit / (1024 * 1024)).toFixed(1)} MB, pace=${state.paceMs}ms, ` +
        `rate=${state.rateLimitBps ? `${(state.rateLimitBps / (1024 * 1024)).toFixed(1)} MB/s` : 'unlimited'} ` +
        `(queue=${queueCount}, packq=${packQueueCount}, pack=${packInUse}, backpressure+${Math.max(deltaEvents, 0)})`
      );
      state.lastMode = nextMode;
    }

    if (tuneLevel >= 0) {
      const payloadTune = `tune=${tuneLevel} pack=${recommendPack} pace=${recommendPace} rate=${recommendRate}`;
      if (payloadTune !== state.lastPayloadTune) {
        log(`Payload tune: level=${tuneLevel} pack=${recommendPack} pace=${recommendPace} rate=${recommendRate}${payloadTuneAllowed ? '' : ' (safety-only)'}`);
        state.lastPayloadTune = payloadTune;
      }
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
    getPaceDelayMs: async () => {
      const hardPauseMs = state.hardPauseUntil - Date.now();
      if (hardPauseMs > 0) {
        await sleepMs(hardPauseMs);
      }
      return state.paceMs;
    },
    getRateLimitBps: () => state.rateLimitBps,
    getChunkSize: () => state.chunkSize,
  };
}

async function readUploadResponse(socket, cancel = { value: false }, initialData = null) {
  const MAX_RESPONSE_SIZE = 1024 * 1024; // 1MB limit
  return new Promise((resolve, reject) => {
    let data = initialData && initialData.length ? Buffer.from(initialData) : Buffer.alloc(0);
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
  if (response === 'OK' || response === 'SUCCESS') {
    return { files: 0, bytes: 0 };
  }
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

function buildUploadStartPayload(remotePath, totalSize, offset) {
  const pathBuf = Buffer.from(String(remotePath || ''), 'utf8');
  const payload = Buffer.alloc(pathBuf.length + 1 + 8 + 8);
  pathBuf.copy(payload, 0);
  payload.writeBigUInt64LE(BigInt(totalSize), pathBuf.length + 1);
  payload.writeBigUInt64LE(BigInt(offset), pathBuf.length + 9);
  return payload;
}

async function readBinaryResponse(reader, timeoutMs = READ_TIMEOUT_MS) {
  const header = await reader.readExact(5, timeoutMs);
  const code = header.readUInt8(0);
  const len = header.readUInt32LE(1);
  const data = len > 0 ? await reader.readExact(len, timeoutMs) : Buffer.alloc(0);
  return { code, data };
}

async function writeBinaryCommand(socket, cmd, payload, cancel, log = () => {}) {
  const body = payload || Buffer.alloc(0);
  const header = Buffer.alloc(5);
  header[0] = cmd;
  header.writeUInt32LE(body.length, 1);
  await writeAllRetry(socket, header, cancel || { value: false }, log);
  if (body.length > 0) {
    await writeAllRetry(socket, body, cancel || { value: false }, log);
  }
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
  if (frameType === FrameType.PackV4) {
    return body;
  }
  if (frameType === FrameType.PackLz4V4) {
    if (!lz4) throw new Error('lz4 module unavailable');
    if (body.length < 4) throw new Error('Invalid LZ4 frame');
    const rawLen = body.readUInt32LE(0);
    const compressed = body.slice(4);
    const out = Buffer.alloc(rawLen);
    const decoded = lz4.decodeBlock(compressed, out);
    if (decoded < 0) throw new Error('LZ4 decode failed');
    return decoded === rawLen ? out : out.slice(0, decoded);
  }
  if (frameType === FrameType.PackZstdV4) {
    if (!fzstd) throw new Error('fzstd module unavailable');
    if (body.length < 4) throw new Error('Invalid ZSTD frame');
    const rawLen = body.readUInt32LE(0);
    const compressed = body.slice(4);
    const out = Buffer.alloc(rawLen);
    const result = fzstd.decompress(compressed, out);
    return Buffer.from(result.buffer, result.byteOffset, result.byteLength);
  }
  if (frameType === FrameType.PackLzmaV4) {
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

function parsePackRecordsV4(packBuffer, onRecord) {
  if (packBuffer.length < 4) {
    throw new Error('Invalid pack payload');
  }
  const records = packBuffer.readUInt32LE(0);
  let offset = 4;
  for (let i = 0; i < records; i++) {
    if (offset + 2 > packBuffer.length) throw new Error('Invalid pack record header');
    const pathLen = packBuffer.readUInt16LE(offset);
    offset += 2;
    if (offset + 2 > packBuffer.length) throw new Error('Invalid pack record flags');
    const flags = packBuffer.readUInt16LE(offset);
    offset += 2;
    if (offset + pathLen + 8 > packBuffer.length) throw new Error('Invalid pack record path');
    const relPath = packBuffer.slice(offset, offset + pathLen).toString('utf8');
    offset += pathLen;
    const dataLen = Number(packBuffer.readBigUInt64LE(offset));
    offset += 8;
    if ((flags & RecordFlag.HasOffset) !== 0) {
      if (offset + 8 > packBuffer.length) throw new Error('Invalid pack record offset');
      offset += 8;
    }
    if ((flags & RecordFlag.HasTotal) !== 0) {
      if (offset + 8 > packBuffer.length) throw new Error('Invalid pack record total');
      offset += 8;
    }
    if (offset + dataLen > packBuffer.length) throw new Error('Invalid pack record data');
    const data = packBuffer.slice(offset, offset + dataLen);
    offset += dataLen;
    onRecord(relPath, data);
  }
  if (offset !== packBuffer.length) {
    throw new Error('Invalid trailing pack data');
  }
}

function parsePackRecordsCompat(packBuffer, onRecord) {
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
  if (offset !== packBuffer.length) {
    throw new Error('Invalid trailing pack data');
  }
}

function parsePackRecords(packBuffer, onRecord) {
  try {
    parsePackRecordsV4(packBuffer, onRecord);
  } catch (v4Err) {
    // Compatibility path for older payloads/frames that use pre-V4 record layout.
    parsePackRecordsCompat(packBuffer, onRecord);
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
      if (
        frame.type !== FrameType.PackV4 &&
        frame.type !== FrameType.PackLz4V4 &&
        frame.type !== FrameType.PackZstdV4 &&
        frame.type !== FrameType.PackLzmaV4
      ) {
        const err = new Error(`Unsupported frame type ${frame.type}`);
        err.code = 'FTX_UNSUPPORTED';
        throw err;
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

const normalizeVersion = (version) => String(version || '').replace(/^v/i, "").trim();

const compareVersions = (v1, v2) => {
  const a = normalizeVersion(v1).split(".").map(Number);
  const b = normalizeVersion(v2).split(".").map(Number);
  const len = Math.max(a.length, b.length);
  for (let i = 0; i < len; i++) {
    const av = Number.isFinite(a[i]) ? a[i] : 0;
    const bv = Number.isFinite(b[i]) ? b[i] : 0;
    if (av > bv) return 1;
    if (av < bv) return -1;
  }
  return 0;
};

async function fetchLatestRelease(includePrerelease = false) {
  const apiUrl = 'https://api.github.com/repos/phantomptr/ps5upload/releases';
  const data = await fetchUrl(apiUrl);
  const releases = JSON.parse(data);

  const sorted = releases
    .filter(r => includePrerelease || !r.prerelease)
    .sort((a, b) => compareVersions(b.tag_name, a.tag_name));

  if (sorted.length > 0) {
    return sorted[0];
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

function findLocalPayloadElf() {
  const candidates = [
    path.resolve(__dirname, '../../payload/ps5upload.elf'),
    path.resolve(process.cwd(), 'payload/ps5upload.elf'),
    path.resolve(process.cwd(), 'ps5upload.elf'),
  ];
  for (const p of candidates) {
    try {
      if (fs.existsSync(p) && payloadPathIsElf(p)) return p;
    } catch {
      // ignore
    }
  }
  return null;
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

async function waitForPayloadStartup(ip, opts = {}) {
  const timeoutMs = Math.max(2000, Number(opts.timeoutMs) || 15000);
  const pollMs = Math.max(250, Number(opts.pollMs) || 500);
  const expectedVersion = opts.expectedVersion ? String(opts.expectedVersion).trim() : '';
  const startedAt = Date.now();
  let lastErr = null;
  while (Date.now() - startedAt < timeoutMs) {
    try {
      const version = await getPayloadVersion(ip, TRANSFER_PORT);
      if (!expectedVersion || version === expectedVersion) {
        return { ok: true, version };
      }
      // Payload started, but not the expected version.
      return { ok: false, version, error: `Running ${version}, expected ${expectedVersion}` };
    } catch (err) {
      lastErr = err;
    }
    await sleepMs(pollMs);
  }
  return { ok: false, version: null, error: `Payload did not start in ${Math.round(timeoutMs / 1000)}s: ${lastErr?.message || lastErr || 'timeout'}` };
}

function probePayloadFile(filepath) {
  if (!payloadPathIsElf(filepath)) {
    return { is_ps5upload: false, code: 'payload_probe_invalid_ext' };
  }

  const nameMatch = filepath.toLowerCase().includes('ps5upload');
  const content = fs.readFileSync(filepath, { encoding: null }).slice(0, 512 * 1024);
  const signatureMatch = content.includes(Buffer.from('ps5upload')) || content.includes(Buffer.from('PS5UPLOAD'));

  if (nameMatch || signatureMatch) {
    return { is_ps5upload: true, code: 'payload_probe_detected' };
  }
  return { is_ps5upload: false, code: 'payload_probe_no_signature' };
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

async function tryAutoReloadPayload(options = {}) {
  const { force = false, reason = '' } = options || {};
  if (!state.payloadAutoReloadEnabled || !state.payloadIp) return;
  if (payloadAutoReloadInFlight) return;
  const now = Date.now();
  if (now - payloadAutoReloadLastAttempt < 15000) return;

  payloadAutoReloadInFlight = true;
  payloadAutoReloadLastAttempt = now;
  emit('payload_busy', { busy: true });

  try {
    if (!force) {
      try {
        await getPayloadVersion(state.payloadIp, TRANSFER_PORT);
        emit('payload_busy', { busy: false });
        return;
      } catch {
        // not running; continue to reload
      }
    } else {
      emit('payload_log', { message: `Auto reload forced${reason ? ` (${reason})` : ''}.` });
      await requestPayloadShutdown(state.payloadIp, TRANSFER_PORT);
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
      if (fetch === 'current') {
        const localPayload = findLocalPayloadElf();
        if (localPayload) {
          emit('payload_log', { message: `Auto reload using local payload: ${localPayload}` });
          await sendPayloadFile(state.payloadIp, localPayload);
        } else {
          let release;
          try {
            release = await fetchReleaseByTag(`v${VERSION}`);
          } catch {
            emit('payload_log', { message: `Tag v${VERSION} not found, falling back to latest release.` });
            release = await fetchLatestRelease(false);
          }
          const asset = release.assets.find(a => a.name === 'ps5upload.elf');
          if (!asset) {
            emit('payload_log', { message: 'Auto reload failed: payload asset not found.' });
            return;
          }
          const tmpPath = path.join(os.tmpdir(), 'ps5upload_autoreload.elf');
          await downloadAsset(asset.browser_download_url, tmpPath);
          await sendPayloadFile(state.payloadIp, tmpPath);
        }
      } else {
        const release = await fetchLatestRelease(false);
        const asset = release.assets.find(a => a.name === 'ps5upload.elf');
        if (!asset) {
          emit('payload_log', { message: 'Auto reload failed: payload asset not found.' });
          return;
        }
        const tmpPath = path.join(os.tmpdir(), 'ps5upload_autoreload.elf');
        await downloadAsset(asset.browser_download_url, tmpPath);
        await sendPayloadFile(state.payloadIp, tmpPath);
      }
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

async function triggerPayloadRecovery(reason) {
  if (!state.payloadAutoReloadEnabled || !state.payloadIp) return;
  emit('payload_log', { message: `Payload recovery: ${reason}` });
  await tryAutoReloadPayload({ force: true, reason });
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
      const probe = probePayloadFile(filepath);
      if (probe?.is_ps5upload) {
        const startup = await waitForPayloadStartup(ip, { timeoutMs: 15000, pollMs: 500, expectedVersion: VERSION });
        if (!startup.ok) {
          throw new Error(`Payload upload completed but startup verification failed: ${startup.error}`);
        }
        emit('payload_log', { message: `Payload started: v${startup.version}` });
      }
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
      if (fetch === 'current') {
        const localPayload = findLocalPayloadElf();
        if (localPayload) {
          emit('payload_log', { message: `Using local payload: ${localPayload}` });
          const bytes = await sendPayloadFile(ip, localPayload);
          const startup = await waitForPayloadStartup(ip, { timeoutMs: 15000, pollMs: 500, expectedVersion: VERSION });
          if (!startup.ok) {
            throw new Error(`Payload upload completed but startup verification failed: ${startup.error}`);
          }
          emit('payload_log', { message: `Payload started: v${startup.version}` });
          emit('payload_done', { bytes, error: null });
          return true;
        }
      }

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

      const logMsg = `[VERSION CHECK] Identified latest release as: ${release.tag_name}. Downloading payload...`;
      console.log(logMsg);
      emit('payload_log', { message: logMsg });

      const assets = Array.isArray(release?.assets) ? release.assets : [];
      let asset = assets.find(a => a.name === 'ps5upload.elf');
      if (!asset) {
        asset = assets.find(a => a.name && a.name.endsWith('.elf'));
      }
      if (!asset) {
        const names = assets.map(a => a?.name).filter(Boolean).join(', ') || 'none';
        throw new Error(`Payload asset not found. Assets: ${names}`);
      }

      const tmpPath = path.join(os.tmpdir(), `ps5upload_${fetch}.elf`);
      await downloadAsset(asset.browser_download_url, tmpPath);

      emit('payload_log', { message: `Payload downloaded: ${tmpPath}` });

      const bytes = await sendPayloadFile(ip, tmpPath);
      const expectVersion = fetch === 'current' ? VERSION : null;
      const startup = await waitForPayloadStartup(ip, { timeoutMs: 15000, pollMs: 500, expectedVersion: expectVersion });
      if (!startup.ok) {
        throw new Error(`Payload upload completed but startup verification failed: ${startup.error}`);
      }
      emit('payload_log', { message: `Payload started: v${startup.version}` });
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

  ipcMain.handle('payload_caps', async (_, ipArg) => {
    const ip = typeof ipArg === 'string' ? ipArg : ipArg?.ip;
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return getPayloadCaps(ip, TRANSFER_PORT);
  });

  ipcMain.handle('payload_probe', (_, filepath) => {
    if (!filepath || !filepath.trim()) throw new Error('Select a payload (.elf/.bin) file first.');
    return probePayloadFile(filepath);
  });

  ipcMain.handle('payload_status', async (_, ipArg) => {
    const ip = typeof ipArg === 'string' ? ipArg : ipArg?.ip;
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return getPayloadStatus(ip, TRANSFER_PORT);
  });

  ipcMain.handle('payload_status_snapshot', () => state.payloadStatus || { status: null, error: null, updated_at_ms: 0 });

  ipcMain.handle('payload_status_refresh', async (_, ipArg) => {
    const ip = typeof ipArg === 'string' ? ipArg : ipArg?.ip;
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

  ipcMain.handle('payload_queue_remove', async (_, ip, id) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return queueRemove(ip, TRANSFER_PORT, id);
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
    assertSafeManageDownloadDestination(destPath);

    return runManageTask('Download', async () => {
      await ensurePayloadReady(ip);
      emit('manage_log', { message: `Download ${filepath}` });
      try {
        // Use compatibility/raw path for single-file manage downloads.
        // It has proven more stable across payload variants than FTX streaming.
        const bytes = await downloadFileCompat(ip, filepath, destPath);
        emitManageDone({ op: 'Download', bytes, error: null });
      } catch (err) {
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

  async function downloadFileCompat(ip, filepath, destPath) {
    const RANGE_CHUNK_SIZE = 8 * 1024 * 1024;
    const MAX_RETRIES_PER_CHUNK = 6;
    let totalSize = null;
    let received = 0;
    let useRangeMode = true;
    let fd = null;
    let lastLogAt = 0;

    const emitProgress = () => {
      emit('manage_progress', { op: 'Download', processed: received, total: totalSize || received, current_file: null });
      const now = Date.now();
      if (now - lastLogAt > 5000) {
        lastLogAt = now;
        emit('manage_log', { message: `Downloading... ${received}/${totalSize || '?'}` });
      }
    };

    const requestChunk = async (offset, length) => {
      let socket = null;
      let headerDone = false;
      let bodyExpected = null;
      let bodyReceived = 0;
      let headerBuf = Buffer.alloc(0);
      let lastProgressAt = Date.now();

      try {
        socket = await createSocketWithTimeout(ip, TRANSFER_PORT);
        socket.setTimeout(0);
        state.manageSocket = socket;

        await new Promise((resolve, reject) => {
          let settled = false;
          let stallTimer = null;
          const finish = (err) => {
            if (settled) return;
            settled = true;
            if (stallTimer) {
              clearInterval(stallTimer);
              stallTimer = null;
            }
            socket.removeAllListeners();
            if (err) reject(err);
            else resolve();
          };

          socket.on('data', (chunk) => {
            if (state.manageCancel) {
              finish(new Error('Download cancelled by user'));
              return;
            }

            let data = chunk;
            if (!headerDone) {
              headerBuf = Buffer.concat([headerBuf, data]);
              const idx = headerBuf.indexOf('\n');
              if (idx === -1) return;
              const line = headerBuf.slice(0, idx).toString('utf8').trim();
              data = headerBuf.slice(idx + 1);
              headerBuf = Buffer.alloc(0);

              if (line.startsWith('ERROR')) {
                const err = new Error(line);
                if (line.includes('Unknown command')) err.code = 'UNSUPPORTED';
                finish(err);
                return;
              }

              if (useRangeMode) {
                const m = line.match(/^READY\s+(\d+)\s+(\d+)\s+(\d+)$/i);
                if (!m) {
                  finish(new Error(`Unexpected response: ${line}`));
                  return;
                }
                const remoteTotal = Number(m[1]);
                const remoteOffset = Number(m[2]);
                const remoteLen = Number(m[3]);
                if (!Number.isFinite(remoteTotal) || !Number.isFinite(remoteOffset) || !Number.isFinite(remoteLen)) {
                  finish(new Error(`Invalid READY response: ${line}`));
                  return;
                }
                if (remoteOffset !== offset) {
                  finish(new Error(`Offset mismatch: expected ${offset}, got ${remoteOffset}`));
                  return;
                }
                if (totalSize == null) totalSize = remoteTotal;
                else if (remoteTotal !== totalSize) {
                  finish(new Error(`Remote size changed: ${totalSize} -> ${remoteTotal}`));
                  return;
                }
                bodyExpected = remoteLen;
              } else {
                const m = line.match(/^READY\s+(\d+)$/i);
                if (!m) {
                  finish(new Error(`Unexpected response: ${line}`));
                  return;
                }
                const remoteTotal = Number(m[1]);
                if (!Number.isFinite(remoteTotal)) {
                  finish(new Error(`Invalid READY response: ${line}`));
                  return;
                }
                if (totalSize == null) totalSize = remoteTotal;
                else if (remoteTotal !== totalSize) {
                  finish(new Error(`Remote size changed: ${totalSize} -> ${remoteTotal}`));
                  return;
                }
                bodyExpected = Math.max(0, remoteTotal - offset);
              }

              headerDone = true;
              emit('manage_progress', { op: 'Download', processed: received, total: totalSize || received, current_file: filepath });
            }

            if (data.length > 0) {
              fs.writeSync(fd, data);
              bodyReceived += data.length;
              received += data.length;
              lastProgressAt = Date.now();
              emitProgress();
            }
          });

          socket.on('error', (err) => finish(err));
          socket.on('timeout', () => finish(new Error('Download timed out')));
          socket.on('close', () => {
            if (headerDone && bodyExpected != null && bodyReceived >= bodyExpected) {
              finish();
            } else {
              finish(new Error(`Connection closed at ${received}/${totalSize || '?'}`));
            }
          });
          socket.on('end', () => {
            if (headerDone && bodyExpected != null && bodyReceived >= bodyExpected) {
              finish();
            }
          });

          const cmd = useRangeMode
            ? `DOWNLOAD_RAW_RANGE ${offset} ${length}\t${filepath}\n`
            : (offset > 0 ? `DOWNLOAD_RAW_FROM ${offset}\t${filepath}\n` : `DOWNLOAD_RAW ${filepath}\n`);
          socket.write(cmd);

          stallTimer = setInterval(() => {
            const idleMs = Date.now() - lastProgressAt;
            if (!headerDone) {
              if (idleMs > 30000) finish(new Error('Download timed out waiting for response'));
              return;
            }
            if (idleMs > 30000) {
              finish(new Error(`Download stalled at ${received}/${totalSize || '?'}`));
            }
          }, 1000);
        });
      } finally {
        if (socket) socket.destroy();
        if (state.manageSocket === socket) state.manageSocket = null;
      }
    };

    const prevNoAsar = process.noAsar;
    process.noAsar = true;
    try {
      try { fs.unlinkSync(destPath); } catch {}
      fd = fs.openSync(destPath, 'a');

      while (totalSize == null || received < totalSize) {
        if (state.manageCancel) throw new Error('Download cancelled by user');
        const offset = received;
        const length = RANGE_CHUNK_SIZE;
        let success = false;
        for (let attempt = 1; attempt <= MAX_RETRIES_PER_CHUNK; attempt += 1) {
          try {
            await requestChunk(offset, length);
            success = true;
            break;
          } catch (err) {
            const msg = err && err.message ? String(err.message) : String(err);
            if (useRangeMode && (err.code === 'UNSUPPORTED' || msg.includes('Invalid DOWNLOAD_RAW_RANGE'))) {
              useRangeMode = false;
              received = offset;
              fs.closeSync(fd);
              fd = fs.openSync(destPath, offset === 0 ? 'w' : 'a');
              break;
            }
            emit('manage_log', { message: `Download retry ${attempt}/${MAX_RETRIES_PER_CHUNK} at ${offset}: ${msg}` });
            await sleepMs(250);
          }
        }
        if (!success && !(useRangeMode === false && received === offset)) {
          throw new Error(`Download failed at ${offset}/${totalSize || '?'}`);
        }
      }

      if (totalSize == null || received < totalSize) {
        throw new Error(`Download incomplete: ${received}/${totalSize || '?'}`);
      }
      emit('manage_progress', { op: 'Download', processed: totalSize, total: totalSize, current_file: null });
      return received;
    } finally {
      process.noAsar = prevNoAsar;
      if (fd != null) {
        try { fs.closeSync(fd); } catch {}
      }
    }
  }

  async function downloadDirCompat(ip, dirPath, destPath) {
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

  async function downloadDirSafe(ip, dirPath, destPath, onProgress, cancel, onLog) {
    const files = await listDirRecursiveCompat(ip, TRANSFER_PORT, dirPath, null, onLog);
    if (typeof onLog === 'function') {
      onLog(`Safe download list: ${files.length} file(s)`);
    }
    if (files.length === 0) {
      throw new Error('No files found in directory (payload list returned empty).');
    }
    await ensurePayloadReady(ip);
    let downloadedBytes = 0;
    let totalBytes = files.reduce((sum, f) => sum + (f.size || 0), 0);
    onProgress(0, totalBytes, null);
    for (const file of files) {
      if (cancel?.value) throw new Error('Download cancelled');
      const localPath = ensureDownloadPath(destPath, file.relPath);
      await fs.promises.mkdir(path.dirname(localPath), { recursive: true });
      let attempts = 0;
      while (attempts < 3) {
        attempts += 1;
        try {
          // Use raw compatibility path for directory downloads too.
          // This avoids long-lived FTX stalls on some payload/network combinations.
          const bytes = await downloadFileCompat(ip, file.remotePath, localPath);
          downloadedBytes += bytes;
          onProgress(downloadedBytes, totalBytes, file.relPath);
          break;
        } catch (err) {
          const message = err?.message || String(err);
          if (message.includes('ECONNREFUSED') || message.includes('Connection closed before response')) {
            if (typeof onLog === 'function') {
              onLog(`Download retry after connection refused: ${file.relPath}`);
            }
            try {
              await triggerPayloadRecovery('manage download refused');
            } catch {
              // ignore recovery failures
            }
            await ensurePayloadReady(ip);
          }
          if (attempts >= 3) throw err;
          await sleepMs(200);
        }
      }
      await sleepMs(25);
    }
    return { bytes: downloadedBytes };
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
    assertSafeManageDownloadDestination(destPath);

    return runManageTask('Download', async () => {
      await ensurePayloadReady(ip);
      emit('manage_log', { message: `Download ${dirPath}` });
      let totalSize = 0;
      const comp = (compression || 'none').toLowerCase();
      let compTag = '';
      if (comp === 'lz4') compTag = ' LZ4';
      else if (comp === 'zstd') compTag = ' ZSTD';
      else if (comp === 'lzma') compTag = ' LZMA';
      else if (comp === 'auto') compTag = ' AUTO';

      try {
        const result = await downloadDirSafe(
          ip,
          dirPath,
          destPath,
          (processed, total, currentFile) => {
            totalSize = total;
            emit('manage_progress', { op: 'Download', processed, total, current_file: currentFile });
          },
          { get value() { return state.manageCancel; } },
          (message) => emit('manage_log', { message })
        );
        emit('manage_progress', { op: 'Download', processed: totalSize, total: totalSize, current_file: null });
        emitManageDone({ op: 'Download', bytes: result.bytes, error: null });
      } catch (err) {
        const message = err?.message || String(err);
        emitManageDone({ op: 'Download', bytes: null, error: message });
      }
    });
  });

  ipcMain.handle('manage_upload', async (_, ip, destRoot, paths, opts = {}) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    if (!destRoot || !destRoot.trim()) throw new Error('Destination path is required.');
    if (!paths || paths.length === 0) throw new Error('Select at least one file or folder.');

    return runManageTask('Upload', async () => {
      emit('manage_log', { message: 'Upload started.' });

      try {
        let totalBytes = 0;
        let totalFiles = 0;
        const batches = [];

        let uploadMode = normalizeUploadMode(opts?.upload_mode);
        if (!opts || opts.upload_mode == null) {
          try {
            const config = loadConfig();
            uploadMode = normalizeUploadMode(config?.upload_mode);
          } catch {
            uploadMode = 'payload';
          }
        }
        const preferredFtpPort = normalizeFtpPort(opts?.ftp_port);
        const resolvedFtpPort = preferredFtpPort === 'auto' ? null : preferredFtpPort;
        emit('manage_log', { message: `Manage upload mode: ${uploadMode} (ftp_port=${preferredFtpPort})` });
        if (uploadMode === 'ftp' || uploadMode === 'mix') {
          emit('manage_log', { message: getFtpServiceHint() });
        }

        for (const srcPath of paths) {
          if (state.manageCancel) {
            throw new Error('Upload cancelled by user');
          }
          const stat = fs.statSync(srcPath);
          let files;
          let dest;
          let isArchive = false;

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
            isArchive = /\.(rar|zip|7z)$/i.test(srcPath);
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

          const tryFtpFallback = async () => {
            throw new Error('FTP fallback disabled in fast-only mode.');
          };

          if (uploadMode === 'ftp') {
            const ftpPort = await detectFtpPort(ip, resolvedFtpPort);
            if (!ftpPort) throw buildFtpUnavailableError(resolvedFtpPort);
            await uploadFilesViaFtp(ip, ftpPort, batch.dest, batch.files, {
              cancel: { get value() { return state.manageCancel; } },
              log: (msg, level) => emit('manage_log', { message: level ? `${level}: ${msg}` : msg }),
              onProgress: (sent, currentFile) => {
                emit('manage_progress', { op: 'Upload', processed: processedBase + sent, total: totalBytes, current_file: currentFile });
              },
              onFileDone: () => {},
            });
          } else {
            try {
              if (batch.files.length > 1) {
                await precreateRemoteDirectories(ip, batch.dest, batch.files, {
                  cancel: { get value() { return state.manageCancel; } },
                  log: (msg) => emit('manage_log', { message: msg }),
                });
              }
              if (batch.files.length === 1 && Number(batch.files[0].size || 0) >= LANE_MIN_FILE_SIZE) {
                emit('manage_log', { message: `Manage upload: connection mode (${LANE_CONNECTIONS} connections).` });
                await runPayloadUploadLaneSingleFile(batch.files[0], {
                  ip,
                  destPath: batch.dest,
                  connections: LANE_CONNECTIONS,
                  chunkSize: getLaneChunkSize(Number(batch.files[0].size || 0)),
                  cancel: { get value() { return state.manageCancel; } },
                  chmodAfterUpload: false,
                  log: (msg) => emit('manage_log', { message: msg }),
                  onProgress: (sent) => {
                    emit('manage_progress', { op: 'Upload', processed: processedBase + sent, total: totalBytes, current_file: batch.files[0].rel_path });
                  },
                });
              } else {
                emit('manage_log', { message: `Manage upload: fast mode (${Math.min(4, batch.files.length)} workers).` });
                await runPayloadUploadFastMultiFile(batch.files, {
                  ip,
                  destPath: batch.dest,
                  connections: Math.min(4, batch.files.length),
                  cancel: { get value() { return state.manageCancel; } },
                  chmodAfterUpload: false,
                  onSkipFile: () => {},
                  log: (msg) => emit('manage_log', { message: msg }),
                  onProgress: (sent, filesSent, currentFile) => {
                    emit('manage_progress', { op: 'Upload', processed: processedBase + sent, total: totalBytes, current_file: currentFile || null });
                  },
                });
              }
            } catch (err) {
              const msg = `Payload upload failed in fast-only mode: ${err.message || err}`;
              emit('manage_log', { message: msg });
              throw err;
            }
          }
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
      let uploadMode = 'payload';
      let preferredFtpPort = 'auto';
      try {
        const config = loadConfig();
        uploadMode = normalizeUploadMode(config?.upload_mode);
        preferredFtpPort = normalizeFtpPort(config?.ftp_port);
      } catch {
        uploadMode = 'payload';
      }

      let extractResult;
      if (uploadMode === 'ftp') {
        const stat = await fs.promises.stat(rarPath);
        const resolvedFtpPort = preferredFtpPort === 'auto' ? null : preferredFtpPort;
        emit('manage_log', { message: getFtpServiceHint() });
        const ftpPort = await detectFtpPort(ip, resolvedFtpPort);
        if (!ftpPort) throw buildFtpUnavailableError(resolvedFtpPort);
        await createPath(ip, TRANSFER_PORT, destPath);
        const rarName = path.basename(rarPath);
        const cleanedTempRoot = typeof tempRoot === 'string' ? tempRoot.trim() : '';
        const rarRemoteDir = cleanedTempRoot || destPath;
        const rarRemotePath = joinRemotePath(rarRemoteDir, rarName);

        await uploadFilesViaFtp(ip, ftpPort, rarRemoteDir, [{
          rel_path: rarName,
          abs_path: rarPath,
          size: Number(stat.size),
          mtime: Math.floor(stat.mtimeMs / 1000),
        }], {
          cancel: { get value() { return state.manageCancel; } },
          log: (msg, level) => emit('manage_log', { message: level ? `${level}: ${msg}` : msg }),
          onProgress: (processed, currentFile) => {
            emit('manage_progress', {
              op: 'Extract',
              processed,
              total: Number(stat.size),
              current_file: currentFile
            });
          },
          onFileDone: () => {},
        });

        await extractArchiveWithProgress(
          ip,
          rarRemotePath,
          destPath,
          { get value() { return state.manageCancel; } },
          (processed, total, currentFile) => {
            emit('manage_progress', {
              op: 'Extract',
              processed,
              total,
              current_file: currentFile
            });
          },
          (message) => emit('manage_log', { message })
        );
        extractResult = { fileSize: stat.size, bytes: stat.size, files: 1, queuedId: null };
      } else {
        extractResult = await uploadRarForExtraction(ip, rarPath, destPath, mode, {
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
          onLog: (message) => emit('manage_log', { message }),
          queueExtract: false,
        });
      }
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
    const cacheWindowMs = 60000;
    try {
      const sourcePath = typeof args === 'string' ? args : args?.source_path;
      const maxMs = typeof args?.max_ms === 'number' ? args.max_ms : 8000;
      const maxFiles = typeof args?.max_files === 'number' ? args.max_files : 50000;
      const quickCount = !!args?.quick_count;
      const sampleLimit = typeof args?.sample_limit === 'number' ? args.sample_limit : 400;
      if (!sourcePath || !String(sourcePath).trim()) {
        throw new Error('Source path is required.');
      }
      const scanKey = JSON.stringify({ sourcePath, maxMs, maxFiles, quickCount, sampleLimit });
      const now = Date.now();
      if (state.scanInProgressKey === scanKey) {
        return { deduped: true };
      }
      if (state.scanCache && state.scanCache.key === scanKey && (now - state.scanCache.completed_at) < cacheWindowMs) {
        const cached = state.scanCache.result;
        emit('scan_complete', cached);
        return cached;
      }
      state.scanInProgressKey = scanKey;

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

      const result = { files, total: totalSize, partial, reason, elapsed_ms: elapsedMs, estimated };
      state.scanCache = { key: scanKey, result, completed_at: Date.now() };
      emit('scan_complete', result);
      return result;
    } catch (err) {
      emit('scan_error', { message: err.message });
      throw err;
    } finally {
      state.scanInProgressKey = null;
    }
  });

  ipcMain.handle('transfer_scan_cancel', () => {
    state.scanCancel = true;
    return true;
  });

  ipcMain.handle('transfer_cancel', () => {
    state.transferCancel = true;
    if (state.transferAbort) {
      state.transferAbort.abort();
    }
    if (state.transferSocket) {
      state.transferSocket.destroy();
    }
    state.transferStatus = { ...state.transferStatus, status: 'Cancelled' };
    state.transferLastUpdate = Date.now();
    state.transferActive = false;
    return true;
  });

  ipcMain.handle('transfer_status', () => ({ ...state.transferStatus, ...state.transferMeta }));
  ipcMain.handle('transfer_reset', () => {
    state.transferCancel = false;
    state.transferActive = false;
    state.transferStatus = createTransferStatus();
    state.transferMeta = { requested_optimize: null, auto_tune_connections: null, effective_optimize: null, effective_compression: null, requested_ftp_connections: null, effective_ftp_connections: null };
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
      ftp_connections: req.ftp_connections,
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
    const transferAbort = new AbortController();
    state.transferAbort = transferAbort;
    state.transferActive = true;
    state.transferStatus = {
      run_id: runId,
      status: 'Starting',
      sent: 0,
      total: req.required_size || 0,
      files: 0,
      elapsed_secs: 0,
      current_file: '',
      payload_transfer_path: null,
      payload_workers: null,
    };
    state.transferMeta = {
      requested_optimize: !!req.optimize_upload,
      auto_tune_connections: !!req.auto_tune_connections,
      effective_optimize: null,
      effective_compression: req.compression || null,
      requested_ftp_connections: 10,
      effective_ftp_connections: null,
    };
    state.transferLastUpdate = Date.now();

const emitLog = (message, level = 'info', force = false) => {
      if (state.saveLogs) writeLogLine('transfer', `[${level.toUpperCase()}] [${runId}] ${message}`);
      if (state.uiLogEnabled || force) emit('transfer_log', { run_id: runId, message, level });
    };
    const debugLog = (message) => emitLog(message, 'debug');

    // Use an IIFE with .catch to handle unhandled promise rejections
    setImmediate(() => {
      (async () => {
        const startTime = Date.now();
        let watchdog = null;
        let filesToUpload = [];
        let totalSize = 0n;
        const prevNoAsar = process.noAsar;
        process.noAsar = true;

        try {
          let uploadMode = normalizeUploadMode(req.upload_mode);
          const requestedFtpConnections = 10;
          const sourceStat = await fs.promises.stat(req.source_path);
          const isRar = sourceStat.isFile() && path.extname(req.source_path).toLowerCase() === '.rar';

          if (isRar) {
            if (uploadMode === 'mix') {
              emitLog('Archive uploads do not support Mix mode; switching to payload.', 'info');
              uploadMode = 'payload';
              state.transferStatus = { ...state.transferStatus, upload_mode: uploadMode };
            }
            emitLog(`Uploading RAR for extraction: ${req.source_path}`, 'info');
            if (uploadMode === 'ftp') {
              const preferred = normalizeFtpPort(req.ftp_port);
              const resolvedFtpPort = preferred === 'auto' ? null : preferred;
              emitLog(getFtpServiceHint(), 'info');
              const ftpPort = await detectFtpPort(req.ip, resolvedFtpPort);
              if (!ftpPort) {
                throw buildFtpUnavailableError(resolvedFtpPort);
              }
              const rarSpeed = { lastAt: Date.now(), lastSent: 0, ema: 0 };
              const rarName = path.basename(req.source_path);
              const tempRoot = typeof req.rar_temp_root === 'string' ? req.rar_temp_root.trim() : '';
              const tempRootPath = buildTempRootForArchive(req.dest_path, tempRoot);
              const tempDir = `${tempRootPath.replace(/\/+$/, '')}/rar_${Date.now()}_${Math.floor(Math.random() * 1e6)}`;
              const rarRemoteDir = tempDir;
              const rarRemotePath = joinRemotePath(rarRemoteDir, rarName);
              await createPath(req.ip, TRANSFER_PORT, tempDir);
              state.transferStatus = createTransferStatus({
                run_id: runId,
                status: 'Uploading archive',
                sent: 0,
                total: sourceStat.size,
                files: 1,
                elapsed_secs: 0,
                current_file: rarName,
                upload_mode: 'ftp',
              });
              state.transferLastUpdate = Date.now();

              await uploadFilesViaFtp(req.ip, ftpPort, rarRemoteDir, [{
                rel_path: rarName,
                abs_path: req.source_path,
                size: Number(sourceStat.size),
                mtime: Math.floor(sourceStat.mtimeMs / 1000),
              }], {
                cancel: { get value() { return state.transferCancel; } },
                log: emitLog,
                onProgress: (totalSent, currentFile) => {
                  const now = Date.now();
                  const elapsed = (now - rarSpeed.lastAt) / 1000;
                  if (elapsed > 0) {
                    const delta = totalSent - rarSpeed.lastSent;
                    const inst = delta > 0 ? delta / elapsed : 0;
                    const alpha = 1 - Math.exp(-elapsed / 3);
                    rarSpeed.ema = rarSpeed.ema > 0 ? rarSpeed.ema + (inst - rarSpeed.ema) * alpha : inst;
                    rarSpeed.lastAt = now;
                    rarSpeed.lastSent = totalSent;
                  }
                  state.transferStatus = createTransferStatus({
                    run_id: runId,
                    status: 'Uploading archive',
                    sent: totalSent,
                    total: sourceStat.size,
                    files: 1,
                    elapsed_secs: (Date.now() - startTime) / 1000,
                    current_file: currentFile || rarName,
                    ftp_sent: totalSent,
                    payload_sent: 0,
                    payload_speed_bps: 0,
                    ftp_speed_bps: rarSpeed.ema,
                    total_speed_bps: rarSpeed.ema,
                    upload_mode: 'ftp',
                  });
                  state.transferLastUpdate = Date.now();
                },
                onFileDone: () => {},
              });

              emitLog('Archive upload complete. Queuing extraction...', 'info');
              const queuedId = await queueExtract(req.ip, TRANSFER_PORT, rarRemotePath, req.dest_path, {
                cleanupPath: tempDir,
                deleteSource: true,
              });
              const elapsed = (Date.now() - startTime) / 1000;
              state.transferStatus = createTransferStatus({
                run_id: runId,
                status: 'Queued for extraction',
                sent: sourceStat.size,
                total: sourceStat.size,
                files: 1,
                elapsed_secs: elapsed,
                current_file: '',
                upload_mode: 'ftp',
              });
              state.transferLastUpdate = Date.now();
              emitLog(`Extraction queued (ID ${queuedId}).`, 'info');
              emit('queue_hint', {
                queue_id: queuedId,
                source_path: req.source_path,
                dest_path: req.dest_path,
                size_bytes: sourceStat.size
              });
              emit('transfer_complete', { run_id: runId, files: 1, bytes: sourceStat.size });
              return;
            }

            const rarSpeed = {
              lastAt: Date.now(),
              lastSent: 0,
              ema: 0,
            };
            state.transferStatus = createTransferStatus({
              run_id: runId,
              status: 'Uploading archive',
              sent: 0,
              total: sourceStat.size,
              files: 1,
              elapsed_secs: 0,
              current_file: path.basename(req.source_path),
            });
            state.transferLastUpdate = Date.now();

            const extractResult = await uploadRarForExtraction(
              req.ip,
              req.source_path,
              req.dest_path,
              'turbo',
              {
                cancel: { get value() { return state.transferCancel; } },
                tempRoot: req.rar_temp_root,
                overrideOnConflict: req.override_on_conflict !== false,
                queueExtract: true,
                onProgress: (op, processed, total, currentFile) => {
                  const now = Date.now();
                  const elapsed = (now - rarSpeed.lastAt) / 1000;
                  if (elapsed > 0) {
                    const delta = processed - rarSpeed.lastSent;
                    const inst = delta > 0 ? delta / elapsed : 0;
                    const alpha = 1 - Math.exp(-elapsed / 3);
                    rarSpeed.ema = rarSpeed.ema > 0 ? rarSpeed.ema + (inst - rarSpeed.ema) * alpha : inst;
                    rarSpeed.lastAt = now;
                    rarSpeed.lastSent = processed;
                  }
                  state.transferStatus = createTransferStatus({
                    run_id: runId,
                    status: op === 'Extract' ? 'Extracting' : 'Uploading archive',
                    sent: processed,
                    total: total || sourceStat.size,
                    files: 1,
                    elapsed_secs: (Date.now() - startTime) / 1000,
                    current_file: currentFile || path.basename(req.source_path),
                    payload_sent: processed,
                    payload_speed_bps: rarSpeed.ema,
                    ftp_speed_bps: 0,
                    total_speed_bps: rarSpeed.ema,
                    upload_mode: 'payload',
                  });
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
            state.transferStatus = createTransferStatus({
              run_id: runId,
              status: isQueued ? 'Queued for extraction' : 'Complete',
              sent: finalTotal,
              total: finalTotal,
              files: extractResult.files || 1,
              elapsed_secs: elapsed,
              current_file: '',
            });
            state.transferLastUpdate = Date.now();
            if (isQueued) {
              emitLog(`Extraction queued (ID ${extractResult.queuedId}).`, 'info');
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

          emitLog('Scanning files...', 'info');
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
          filesToUpload = result.files;
          if (resumeMode && resumeMode !== 'none' && uploadMode !== 'ftp') {
            emitLog(`Resume scan: building remote index (${resumeMode})...`, 'info');
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
              debugLog,
              transferAbort.signal
            );

            if (!remoteIndex) {
              emitLog('Resume scan skipped (compat listing too large); proceeding without duplicate checks.', 'info');
              filesToUpload = result.files;
              state.transferStatus = { ...state.transferStatus, status: 'Scanning', files: filesToUpload.length, total: Number(filesToUpload.length) };
              state.transferLastUpdate = Date.now();
            } else {
            let skipped = 0;
            let missing = 0;
            let sizeMatched = 0;
            let sizeMismatched = 0;
            let hashChecked = 0;
            let hashMatched = 0;
            let hashMismatched = 0;
            let hashFailed = 0;
            const filtered = [];
            for (const file of result.files) {
              const rel = file.rel_path.replace(/\\/g, '/');
              const remote = remoteIndex.get(rel);
              if (!remote) {
                missing++;
                filtered.push(file);
                continue;
              }
              const sizeMatch = Number(file.size) === Number(remote.size);
              if (resumeMode === 'size') {
                if (sizeMatch) {
                  skipped++;
                  sizeMatched++;
                  continue;
                }
                sizeMismatched++;
                filtered.push(file);
                continue;
              }
              if (!sizeMatch) {
                sizeMismatched++;
                filtered.push(file);
                continue;
              }
              if (!shouldHashResume(resumeMode, Number(file.size))) {
                skipped++;
                sizeMatched++;
                continue;
              }
              const remotePath = joinRemotePath(destRoot, rel);
              try {
                hashChecked++;
                const [localHash, remoteHash] = await Promise.all([
                  hashFileLocal(file.abs_path, transferAbort.signal),
                  hashFileRemote(req.ip, TRANSFER_PORT, remotePath, transferAbort.signal)
                ]);
                if (localHash === remoteHash) {
                  skipped++;
                  hashMatched++;
                  continue;
                }
                hashMismatched++;
              } catch (err) {
                hashFailed++;
                emitLog(`Resume hash failed for ${rel}: ${err.message || err}`, 'warn');
              }
              filtered.push(file);
            }

            filesToUpload = filtered;
            const resumeSummary = resumeMode === 'size'
              ? `Resume scan done: ${skipped} file(s) already present (size match), ${missing} missing, ${sizeMismatched} size mismatch, ${filesToUpload.length} to upload.`
              : `Resume scan done: ${skipped} file(s) already present (${sizeMatched} size match, ${hashMatched} hash match), ${missing} missing, ${sizeMismatched} size mismatch, ${hashMismatched} hash mismatch, ${hashFailed} hash errors, ${filesToUpload.length} to upload.`;
            emitLog(resumeSummary, 'info');
            emit('manage_log', { message: resumeSummary });
            state.transferStatus = { ...state.transferStatus, status: 'Scanning', files: filesToUpload.length, total: Number(filesToUpload.length) };
            state.transferLastUpdate = Date.now();
            }
          } else if (resumeMode && resumeMode !== 'none' && uploadMode === 'ftp') {
            emitLog('Resume scan skipped for FTP mode; FTP uses per-file size/resume.', 'info');
          }

          if (filesToUpload.length === 0) {
            const elapsed = (Date.now() - startTime) / 1000;
            state.transferStatus = createTransferStatus({
              run_id: runId,
              status: 'Complete',
              sent: 0n,
              total: 0n,
              files: 0,
              elapsed_secs: elapsed,
              current_file: ''
            });
            state.transferLastUpdate = Date.now();
            emit('transfer_complete', { run_id: runId, files: 0, bytes: 0 });
            return;
          }

          totalSize = filesToUpload.reduce((sum, f) => sum + BigInt(f.size), 0n);
          const fileCount = filesToUpload.length;
          if (uploadMode === 'mix' && fileCount <= 1) {
            emitLog('Mix mode disabled for single-file transfer; using payload.', 'info');
            uploadMode = 'payload';
          }
          let ftpPort = null;
          if (uploadMode !== 'payload') {
            const preferred = normalizeFtpPort(req.ftp_port);
            const resolvedFtpPort = preferred === 'auto' ? null : preferred;
            emitLog(getFtpServiceHint(), 'info');
            ftpPort = await detectFtpPort(req.ip, resolvedFtpPort);
            if (!ftpPort) {
              throw buildFtpUnavailableError(resolvedFtpPort);
            }
          }
          state.transferStatus = { ...state.transferStatus, upload_mode: uploadMode };

          let payloadFiles = filesToUpload;
          let ftpFiles = [];
          let mixSorted = null;
          let mixLow = 0;
          let mixHigh = -1;
          let mixPayloadBytesEstimate = 0;
          let mixFtpBytesEstimate = 0;
          let mixPayloadCountEstimate = 0;
          let mixFtpCountEstimate = 0;
          if (uploadMode === 'mix') {
            mixSorted = [...filesToUpload].sort((a, b) => a.size - b.size);
            mixLow = 0;
            mixHigh = mixSorted.length - 1;
            const payloadBytesEstimate = mixSorted
              .slice(0, Math.floor(mixSorted.length / 2))
              .reduce((sum, f) => sum + f.size, 0);
            const ftpBytesEstimate = mixSorted
              .slice(Math.floor(mixSorted.length / 2))
              .reduce((sum, f) => sum + f.size, 0);
            mixPayloadBytesEstimate = payloadBytesEstimate;
            mixFtpBytesEstimate = ftpBytesEstimate;
            mixPayloadCountEstimate = Math.max(1, Math.floor(mixSorted.length / 2));
            mixFtpCountEstimate = Math.max(1, mixSorted.length - mixPayloadCountEstimate);
            emitLog(
              'Mix mode: payload pulls the smallest remaining files, FTP pulls the largest, and both keep going until they meet.',
              'info',
              true
            );
            emit('transfer_summary_ready', {
              run_id: runId,
              payload_files: 0,
              payload_size: payloadBytesEstimate,
              ftp_files: 0,
              ftp_size: ftpBytesEstimate
            });
            payloadFiles = () => {
              if (!mixSorted || mixLow > mixHigh) return null;
              return mixSorted[mixLow++];
            };
            ftpFiles = () => {
              if (!mixSorted || mixLow > mixHigh) return null;
              return mixSorted[mixHigh--];
            };
          } else if (uploadMode === 'ftp') {
            payloadFiles = [];
            ftpFiles = filesToUpload;
          }
          const getRemainingMixFiles = () => {
            if (!mixSorted || mixLow > mixHigh) return [];
            return mixSorted.slice(mixLow, mixHigh + 1);
          };

          const payloadTotalSize = Array.isArray(payloadFiles)
            ? payloadFiles.reduce((sum, f) => sum + BigInt(f.size), 0n)
            : BigInt(mixPayloadBytesEstimate || 0);
          const ftpTotalSize = Array.isArray(ftpFiles)
            ? ftpFiles.reduce((sum, f) => sum + BigInt(f.size), 0n)
            : BigInt(mixFtpBytesEstimate || 0);
          const payloadFileCount = Array.isArray(payloadFiles) ? payloadFiles.length : mixPayloadCountEstimate;
          const ftpFileCount = Array.isArray(ftpFiles) ? ftpFiles.length : mixFtpCountEstimate;
          const avgPayloadSize = payloadFileCount > 0 ? Number(payloadTotalSize) / payloadFileCount : 0;
          const avgFtpSize = ftpFileCount > 0 ? Number(ftpTotalSize) / ftpFileCount : 0;
          let effectiveFtpConnections = requestedFtpConnections;
          let effectivePayloadConnections = 4;

          let effectiveCompression = req.compression;
          let effectiveOptimize = !!req.optimize_upload;
          let basePackLimit = PACK_BUFFER_SIZE;
          let baseChunkSize = SEND_CHUNK_SIZE;
          let extraPaceMs = 0;
          let ftpThrottleMs = 0;
          const kb = 1024;
          const mb = 1024 * 1024;
          const ftpSmallFileThreshold = 256 * kb;
          const allowCompression = payloadFileCount > 0;
          if (payloadFileCount > 0) {
            if (req.compression === 'auto') {
              if (avgPayloadSize < SMALL_FILE_AVG_BYTES || payloadFileCount >= 100000) {
                effectiveCompression = 'lz4';
              } else if (avgPayloadSize > 8 * 1024 * 1024) {
                effectiveCompression = 'none';
              } else {
                effectiveCompression = 'lz4';
              }
            }
            if (req.auto_tune_connections && !effectiveOptimize) {
              if (avgPayloadSize < SMALL_FILE_AVG_BYTES || payloadFileCount >= 50000) {
                effectiveOptimize = true;
              }
            }
            if (req.auto_tune_connections) {
              if (avgPayloadSize < SMALL_FILE_AVG_BYTES || payloadFileCount >= 200000) {
                basePackLimit = 24 * 1024 * 1024;
                baseChunkSize = 4 * 1024 * 1024;
              }
            }
            const tinyPayloadBatch = avgPayloadSize < 64 * kb || payloadFileCount >= 100000;
            const smallPayloadBatch = avgPayloadSize < 256 * kb || payloadFileCount >= 50000;
            if (tinyPayloadBatch) {
              basePackLimit = Math.min(basePackLimit, 12 * mb);
              baseChunkSize = Math.min(baseChunkSize, 256 * kb);
              extraPaceMs = Math.max(extraPaceMs, 5);
            } else if (smallPayloadBatch) {
              basePackLimit = Math.min(basePackLimit, 24 * mb);
              baseChunkSize = Math.min(baseChunkSize, 512 * kb);
              extraPaceMs = Math.max(extraPaceMs, 2);
            }
          }
          if (ftpFileCount > 0) {
            const ftpThrottleBoost = req.auto_tune_connections ? 2 : 0;
            const tinyFtpBatch = avgFtpSize < 128 * kb || ftpFileCount >= 100000;
            const smallFtpBatch = avgFtpSize < 512 * kb || ftpFileCount >= 50000;
            if (tinyFtpBatch) {
              ftpThrottleMs = Math.max(ftpThrottleMs, 5 + ftpThrottleBoost);
            } else if (smallFtpBatch) {
              ftpThrottleMs = Math.max(ftpThrottleMs, 2 + ftpThrottleBoost);
            }
            if (req.auto_tune_connections) {
              if (avgFtpSize < 256 * kb || ftpFileCount >= 50000) {
                effectiveFtpConnections = clamp(effectiveFtpConnections, 1, 4);
              } else if (avgFtpSize < 2 * mb || ftpFileCount >= 20000) {
                effectiveFtpConnections = clamp(effectiveFtpConnections, 2, 6);
              } else if (avgFtpSize > 256 * mb && ftpFileCount < 1000) {
                effectiveFtpConnections = clamp(effectiveFtpConnections, 4, 10);
              }
            }
          }

          state.transferMeta = {
            ...state.transferMeta,
            effective_optimize: payloadFileCount > 0 ? effectiveOptimize : null,
            effective_compression: payloadFileCount > 0 ? effectiveCompression : null,
          };
          if (allowCompression && req.compression !== effectiveCompression) {
            emitLog(`Auto-tune compression: ${req.compression} -> ${effectiveCompression}`, 'debug');
          }
          if (allowCompression) {
            if (effectiveCompression === 'lz4' && !lz4) {
              emitLog('Compression lz4 unavailable; using none.', 'warn');
              effectiveCompression = 'none';
            } else if (effectiveCompression === 'zstd' && !fzstd) {
              emitLog('Compression zstd unavailable; using none.', 'warn');
              effectiveCompression = 'none';
            } else if (effectiveCompression === 'lzma' && !lzma) {
              emitLog('Compression lzma unavailable; using none.', 'warn');
              effectiveCompression = 'none';
            }
          } else {
            effectiveCompression = 'none';
          }
          if (payloadFileCount > 0 && !!req.optimize_upload !== effectiveOptimize) {
            emitLog('Auto-tune optimize: enabled to reduce per-file overhead.', 'debug');
          }
          if (extraPaceMs > 0 && payloadFileCount > 0) {
            emitLog(`Small-file safeguard: payload pacing floor ${extraPaceMs}ms, pack ${(basePackLimit / (1024 * 1024)).toFixed(1)} MB.`, 'info');
          }
          if (ftpThrottleMs > 0 && ftpFileCount > 0) {
            emitLog(`Small-file safeguard: FTP delay ${ftpThrottleMs}ms between small files.`, 'info');
          }
	          if (req.auto_tune_connections && payloadFileCount > 0) {
	            const singlePayloadFile = Array.isArray(payloadFiles) && payloadFileCount === 1;
	            const payloadSizeBytes = Number(payloadTotalSize);
	            if (singlePayloadFile) {
	              if (payloadSizeBytes >= 8 * 1024 * 1024 * 1024) {
	                effectivePayloadConnections = clamp(effectivePayloadConnections, 4, 4);
	              } else if (payloadSizeBytes >= 1024 * 1024 * 1024) {
	                effectivePayloadConnections = clamp(effectivePayloadConnections, 3, 4);
	              } else if (payloadSizeBytes <= 128 * 1024 * 1024) {
	                effectivePayloadConnections = clamp(effectivePayloadConnections, 1, 4);
	              }
	            } else {
	              if (avgPayloadSize < 256 * kb || payloadFileCount >= 100000) {
	                effectivePayloadConnections = clamp(effectivePayloadConnections, 1, 4);
	              } else if (avgPayloadSize < 2 * mb || payloadFileCount >= 20000) {
	                effectivePayloadConnections = clamp(effectivePayloadConnections, 2, 4);
	              } else if (avgPayloadSize > 256 * mb && payloadFileCount < 1000) {
	                effectivePayloadConnections = clamp(effectivePayloadConnections, 3, 4);
	              }
	            }
	          }
	          // Stability first: cap payload concurrency at 4 to avoid overwhelming the PS5 side.
	          effectivePayloadConnections = clamp(effectivePayloadConnections, 1, 4);
          if (uploadMode !== 'payload') {
            state.transferMeta.effective_ftp_connections = effectiveFtpConnections;
          }
          const connectionSummary = uploadMode === 'payload'
            ? `payload=${effectivePayloadConnections}`
            : uploadMode === 'ftp'
            ? `ftp=${effectiveFtpConnections}`
            : `payload=${effectivePayloadConnections}, ftp=${effectiveFtpConnections}`;
          emitLog(
            `Starting transfer: ${(Number(totalSize) / (1024 * 1024 * 1024)).toFixed(2)} GB using ${connectionSummary} connection(s)`,
            'info'
          );

          let payloadSent = 0n;
          let payloadFilesSent = 0;
          let ftpSent = 0n;
          let ftpFilesSent = 0;
          const speedState = {
            lastAt: Date.now(),
            lastPayloadSent: 0n,
            lastFtpSent: 0n,
            payloadEma: 0,
            ftpEma: 0,
            totalEma: 0
          };
          const updateSpeedMetrics = () => {
            const now = Date.now();
            if (payloadSent < speedState.lastPayloadSent || ftpSent < speedState.lastFtpSent) {
              speedState.lastPayloadSent = payloadSent;
              speedState.lastFtpSent = ftpSent;
              speedState.lastAt = now;
              speedState.payloadEma = 0;
              speedState.ftpEma = 0;
              speedState.totalEma = 0;
              return;
            }
            const elapsed = (now - speedState.lastAt) / 1000;
            if (elapsed <= 0) return;
            const payloadDelta = Number(payloadSent - speedState.lastPayloadSent);
            const ftpDelta = Number(ftpSent - speedState.lastFtpSent);
            const totalDelta = payloadDelta + ftpDelta;
            const payloadInst = payloadDelta > 0 ? payloadDelta / elapsed : 0;
            const ftpInst = ftpDelta > 0 ? ftpDelta / elapsed : 0;
            const totalInst = totalDelta > 0 ? totalDelta / elapsed : 0;
            const alpha = 1 - Math.exp(-elapsed / 2.5);
            speedState.payloadEma =
              speedState.payloadEma > 0 ? speedState.payloadEma + (payloadInst - speedState.payloadEma) * alpha : payloadInst;
            speedState.ftpEma =
              speedState.ftpEma > 0 ? speedState.ftpEma + (ftpInst - speedState.ftpEma) * alpha : ftpInst;
            speedState.totalEma =
              speedState.totalEma > 0 ? speedState.totalEma + (totalInst - speedState.totalEma) * alpha : totalInst;
            speedState.lastPayloadSent = payloadSent;
            speedState.lastFtpSent = ftpSent;
            speedState.lastAt = now;
          };
          const updateProgress = (currentFile, statusOverride) => {
            updateSpeedMetrics();
            state.transferStatus = {
              ...state.transferStatus,
              run_id: runId,
              status: statusOverride || (uploadMode === 'ftp' ? 'Uploading (FTP)' : 'Uploading'),
              sent: payloadSent + ftpSent,
              total: totalSize,
              files: payloadFilesSent + ftpFilesSent,
              elapsed_secs: (Date.now() - startTime) / 1000,
              current_file: currentFile || '',
              payload_sent: Number(payloadSent),
              ftp_sent: Number(ftpSent),
              payload_speed_bps: speedState.payloadEma,
              ftp_speed_bps: speedState.ftpEma,
              total_speed_bps: speedState.totalEma,
              upload_mode: uploadMode
            };
            state.transferLastUpdate = Date.now();
          };

          state.transferStatus = { ...state.transferStatus, status: uploadMode === 'ftp' ? 'Uploading (FTP)' : 'Uploading', files: fileCount, total: Number(totalSize) };
          state.transferLastUpdate = Date.now();

          let parsed = { files: fileCount, bytes: Number(totalSize) };
          const rateLimitBps = req.bandwidth_limit_mbps ? req.bandwidth_limit_mbps * 1024 * 1024 / 8 : null; // Convert Mbps to Bps
          const missingFiles = new Set();

          const waitForPayloadRecovery = async () => {
            const deadline = Date.now() + 180000;
            while (Date.now() < deadline) {
              if (state.transferCancel || transferAbort.signal.aborted) return false;
              try {
                const status = await getPayloadStatus(req.ip, TRANSFER_PORT);
                if (status && !status.is_busy) {
                  const transfer = status.transfer || {};
                  if (Number(transfer.active_sessions || 0) === 0 && !transfer.abort_requested) {
                    return true;
                  }
                }
              } catch {
                // ignore transient status failures
              }
              await sleepMs(2000);
            }
            return false;
          };

          const filterResumeBySize = async (files) => {
            const destRoot = String(req.dest_path || '').replace(/\\/g, '/');
            try {
              const remoteIndex = await buildRemoteIndex(
                req.ip,
                TRANSFER_PORT,
                destRoot,
                files,
                () => {},
                debugLog,
                transferAbort.signal
              );
              let skipped = 0;
              const filtered = [];
              for (const file of files) {
                const rel = file.rel_path.replace(/\\/g, '/');
                const remote = remoteIndex.get(rel);
                if (remote && Number(file.size) === Number(remote.size)) {
                  skipped++;
                  continue;
                }
                filtered.push(file);
              }
              emitLog(`Auto-resume: ${skipped} file(s) already present, ${filtered.length} to upload.`, 'info');
              return filtered;
            } catch (err) {
              emitLog(`Auto-resume scan failed; retrying full payload upload. ${err.message || err}`, 'warn');
              return files;
            }
          };

          const runPayloadUpload = async (files) => {
            if (!files) return;
            if (Array.isArray(files) && files.length === 0) return;
            let attemptFiles = files;
            let recoveryAttempted = false;
            let precreatedDirs = false;
            const ensurePrecreate = async () => {
              if (precreatedDirs) return;
              precreatedDirs = true;
              if (Array.isArray(attemptFiles) && attemptFiles.length > 1) {
                await precreateRemoteDirectories(req.ip, req.dest_path, attemptFiles, {
                  cancel: { get value() { return state.transferCancel; } },
                  log: emitLog,
                });
              }
            };
            while (true) {
              try {
                // Mad Max mode is intentionally opt-in because it is aggressive and can destabilize the PS5 side.
                const madMaxEligible = req?.mad_max === true &&
                  Array.isArray(attemptFiles) &&
                  attemptFiles.length === 1 &&
                  Number(attemptFiles[0]?.size || 0) >= MAD_MAX_MIN_FILE_SIZE;
                if (madMaxEligible) {
                  const madMaxChunkSize = getMadMaxChunkSize(Number(attemptFiles[0]?.size || 0));
                  emitLog(
                    `Payload Mad Max mode: locked max profile (${clamp(MAD_MAX_WORKERS, 1, 4)} workers, ${formatBytes(madMaxChunkSize)} chunks). ` +
                    'This is aggressive and may cause instability.',
                    'warn'
                  );
                  state.transferStatus = {
                    ...state.transferStatus,
                    payload_transfer_path: 'mad_max_single',
                    payload_workers: clamp(MAD_MAX_WORKERS, 1, 4),
                  };
                  await runPayloadUploadMadMaxSingleFile(attemptFiles[0], {
                    ip: req.ip,
                    destPath: req.dest_path,
                    cancel: { get value() { return state.transferCancel; } },
                    chmodAfterUpload: !!req.chmod_after_upload,
                    log: debugLog,
                    onProgress: (sent, filesSent, currentFile) => {
                      payloadSent = BigInt(sent);
                      payloadFilesSent = filesSent;
                      updateProgress(currentFile);
                    },
                  });
                  parsed = { files: 1, bytes: Number(payloadSent) };
                  return;
                }

                const isParallelCandidate = Array.isArray(attemptFiles) && effectivePayloadConnections > 1;
                if (isParallelCandidate) {
                  if (attemptFiles.length === 1 && Number(attemptFiles[0]?.size || 0) >= LANE_MIN_FILE_SIZE) {
                    const laneChunkSize = getLaneChunkSize(Number(attemptFiles[0]?.size || 0));
	                    const laneConnections = clamp(effectivePayloadConnections || LANE_CONNECTIONS, 1, 4);
                    emitLog(
                      `Payload connection mode: ${laneConnections} connections, ${formatBytes(laneChunkSize)} chunks.`,
                      'info'
                    );
                    state.transferStatus = {
                      ...state.transferStatus,
                      payload_transfer_path: 'lane_fast_offset',
                      payload_workers: laneConnections,
                    };
                    await runPayloadUploadLaneSingleFile(attemptFiles[0], {
                      ip: req.ip,
                      destPath: req.dest_path,
                      connections: laneConnections,
                      chunkSize: laneChunkSize,
                      cancel: { get value() { return state.transferCancel; } },
                      chmodAfterUpload: !!req.chmod_after_upload,
                      log: debugLog,
                      onProgress: (sent, filesSent, currentFile) => {
                        payloadSent = BigInt(sent);
                        payloadFilesSent = filesSent;
                        updateProgress(currentFile);
                      },
                    });
                    parsed = { files: 1, bytes: Number(payloadSent) };
                    return;
                  }

                  await ensurePrecreate();
                  emitLog(`Payload binary multi-file mode: ${effectivePayloadConnections} workers.`, 'info');
                  state.transferStatus = {
                    ...state.transferStatus,
                    payload_transfer_path: 'binary_multi_file',
                    payload_workers: effectivePayloadConnections,
                  };
                  await runPayloadUploadFastMultiFile(attemptFiles, {
                    ip: req.ip,
                    destPath: req.dest_path,
                    connections: effectivePayloadConnections,
                    cancel: { get value() { return state.transferCancel; } },
                    chmodAfterUpload: !!req.chmod_after_upload,
                    onSkipFile: (file) => missingFiles.add(file.rel_path),
                    log: debugLog,
                    onProgress: (sent, filesSent, currentFile) => {
                      payloadSent = BigInt(sent);
                      payloadFilesSent = filesSent;
                      updateProgress(currentFile);
                    },
                  });
                  parsed = { files: payloadFilesSent || attemptFiles.length, bytes: Number(payloadSent) };
                  return;
                }

                emitLog('Payload binary upload (single worker).', 'info');
                state.transferStatus = {
                  ...state.transferStatus,
                  payload_transfer_path: 'binary_single',
                  payload_workers: 1,
                };
                await runPayloadUploadFastMultiFile(attemptFiles, {
                  ip: req.ip,
                  destPath: req.dest_path,
                  connections: 1,
                  cancel: { get value() { return state.transferCancel; } },
                  chmodAfterUpload: !!req.chmod_after_upload,
                  onSkipFile: (file) => missingFiles.add(file.rel_path),
                  log: debugLog,
                  onProgress: (sent, filesSent, currentFile) => {
                    payloadSent = BigInt(sent);
                    payloadFilesSent = filesSent;
                    updateProgress(currentFile);
                  },
                });
                parsed = { files: payloadFilesSent || (Array.isArray(attemptFiles) ? attemptFiles.length : fileCount), bytes: Number(payloadSent) };
              } catch (err) {
                if (!recoveryAttempted && !state.transferCancel && !transferAbort.signal.aborted) {
                  recoveryAttempted = true;
                  emitLog(`Payload error; attempting auto-recovery. ${err?.message || err}`, 'warn');
                  const recovered = await waitForPayloadRecovery();
                  if (!recovered) {
                    throw new Error('Payload did not recover within 3 minutes.');
                  }
                  let retryFiles = attemptFiles;
                  if (Array.isArray(attemptFiles)) {
                    retryFiles = await filterResumeBySize(attemptFiles);
                  } else if (typeof attemptFiles === 'function' && uploadMode === 'mix') {
                    retryFiles = await filterResumeBySize(getRemainingMixFiles());
                  }
                  if (Array.isArray(retryFiles) && retryFiles.length === 0) {
                    emitLog('Auto-recovery: all files already present after resume scan.', 'info');
                    return;
                  }
                  emitLog('Payload recovered; resuming upload...', 'info');
                  attemptFiles = retryFiles;
                  continue;
                }
                throw err;
              }
              return;
            }
          };

          const runFtpUpload = async (files) => {
            if (!files) return;
            if (Array.isArray(files) && files.length === 0) return;
            if (!ftp) throw new Error('FTP library unavailable.');
            emitLog(`FTP upload on port ${ftpPort} (${effectiveFtpConnections} connection(s))...`, 'info');
            const result = await uploadFilesViaFtp(req.ip, ftpPort, req.dest_path, files, {
              cancel: { get value() { return state.transferCancel; } },
              log: emitLog,
              connections: effectiveFtpConnections,
              throttle_ms: ftpThrottleMs,
              small_file_threshold: ftpSmallFileThreshold,
              onProgress: (totalSent, currentFile) => {
                ftpSent = BigInt(totalSent);
                updateProgress(currentFile, uploadMode === 'ftp' ? 'Uploading (FTP)' : 'Uploading');
              },
              onFile: () => {},
              onFileDone: () => {
                ftpFilesSent += 1;
                updateProgress('');
              },
              onSkipFile: (file) => missingFiles.add(file.rel_path),
            });
            ftpSent = BigInt(result.bytes);
            ftpFilesSent = result.files;
          };

          if (uploadMode === 'ftp') {
            await runFtpUpload(ftpFiles);
          } else if (uploadMode === 'mix') {
            let payloadError = null;
            let ftpError = null;
            await Promise.allSettled([
              (async () => {
                try {
                  await runPayloadUpload(payloadFiles);
                } catch (err) {
                  payloadError = err;
                }
              })(),
              (async () => {
                try {
                  await runFtpUpload(ftpFiles);
                } catch (err) {
                  ftpError = err;
                }
              })(),
            ]);

            const remainingMix = getRemainingMixFiles();
            if (ftpError && payloadError && remainingMix.length > 0) {
              emitLog('Both payload and FTP failed; cannot recover remaining files.', 'error');
            } else if (ftpError && remainingMix.length > 0) {
              emitLog(`FTP failed; falling back to payload for ${remainingMix.length} remaining file(s).`, 'warn');
              await runPayloadUpload(remainingMix);
              ftpError = null;
            } else if (payloadError && remainingMix.length > 0) {
              emitLog(`Payload upload failed; falling back to FTP for ${remainingMix.length} remaining file(s).`, 'warn');
              await runFtpUpload(remainingMix);
              payloadError = null;
            }
            if (payloadError) throw payloadError;
            if (ftpError) throw ftpError;
          } else {
            await runPayloadUpload(payloadFiles);
          }

          const elapsed = (Date.now() - startTime) / 1000;
          state.transferStatus = createTransferStatus({
            run_id: runId,
            status: 'Complete',
            sent: totalSize,
            total: totalSize,
            files: parsed.files,
            elapsed_secs: elapsed,
            current_file: ''
          });
          state.transferLastUpdate = Date.now();

          const skippedFiles = missingFiles.size;
          if (skippedFiles > 0) {
            emitLog(`Skipped ${skippedFiles} missing/unreadable file(s).`, 'warn');
          }
          emit('transfer_complete', { run_id: runId, files: parsed.files, bytes: parsed.bytes, skipped: skippedFiles });
        } catch (err) {
          emit('transfer_error', { run_id: runId, message: err.message });
          // Only update status if this is still the active transfer
          if (state.transferStatus.run_id === runId) {
            state.transferStatus = { ...state.transferStatus, status: `Error: ${err.message}` };
            state.transferLastUpdate = Date.now();
          }
        } finally {
          process.noAsar = prevNoAsar;
          if (watchdog) {
            clearInterval(watchdog);
            watchdog = null;
          }
          // Only clean up socket if it's still ours
          if (state.transferSocket && state.transferStatus.run_id === runId) {
            state.transferSocket.destroy();
            state.transferSocket = null;
          }
          if (state.transferAbort === transferAbort) {
            state.transferAbort = null;
          }
          // Only reset transferActive if this is still the active transfer
          if (state.transferStatus.run_id === runId) {
            state.transferActive = false;
          }
        }
      })().catch((err) => {
        // Handle any unhandled rejection from the async IIFE
        console.error('Unhandled transfer error:', err);
        emit('transfer_error', { run_id: runId, message: err.message || 'Unknown error' });
        // Only clean up socket if it's still ours
        if (state.transferSocket && state.transferStatus.run_id === runId) {
          state.transferSocket.destroy();
          state.transferSocket = null;
        }
        if (state.transferAbort === transferAbort) {
          state.transferAbort = null;
        }
        // Only reset transferActive if this is still the active transfer
        if (state.transferStatus.run_id === runId) {
          state.transferActive = false;
        }
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

  // Game meta (simplified)
  ipcMain.handle('game_meta_load', async (_, sourcePath) => {
    return loadGameMetaForPath(sourcePath);
  });

  ipcMain.handle('manage_rar_metadata', async (_, ip, filepath) => {
    return { meta: null, cover: null };
  });

  ipcMain.handle('games_scan', async (_, ip, storagePaths, scanPaths) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    let roots = Array.isArray(storagePaths)
      ? storagePaths.filter((value) => typeof value === 'string' && value.trim().length > 0)
      : [];
    if (roots.length === 0) {
      const locations = await listStorage(ip, TRANSFER_PORT);
      roots = (locations || []).map((item) => item.path).filter(Boolean);
    }
    return scanRemoteGames(ip, roots, scanPaths);
  });

  ipcMain.handle('games_scan_stats', async (_, ip, gamePath) => {
    if (!ip || !ip.trim()) throw new Error('Enter a PS5 address first.');
    return scanRemoteGameStats(ip, gamePath);
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
