#!/usr/bin/env node
/**
 * Test script to replicate the Games Tab scan code path
 * This simulates exactly what the desktop app does when scanning for games
 *
 * Usage:
 *   node test-games-scan.js <PS5_IP>              - Full games scan (default)
 *   node test-games-scan.js <PS5_IP> --stress     - Stress test DOWNLOAD_RAW with non-existent files
 *   node test-games-scan.js <PS5_IP> --listdir    - Test LIST_DIR only (like file browser)
 *   node test-games-scan.js <PS5_IP> --sequential - Games scan without concurrency
 */

const net = require('net');

const TRANSFER_PORT = 9113;
const CONNECT_TIMEOUT_MS = 5000;
const READ_TIMEOUT_MS = 30000;

// Parse args
const args = process.argv.slice(2);
const ip = args.find(a => !a.startsWith('--'));
const MODE = args.includes('--stress') ? 'stress'
  : args.includes('--listdir') ? 'listdir'
  : args.includes('--sequential') ? 'sequential'
  : 'full';

// Same defaults as desktop app
const DEFAULT_GAMES_SCAN_PATHS = ['etaHEN/games', 'homebrew'];
const COVER_CANDIDATES = ['cover.jpg', 'cover.png', 'icon0.png', 'tile0.png', 'pic0.png', 'pic1.png'];

function log(msg) {
  const ts = new Date().toISOString();
  console.log(`[${ts}] ${msg}`);
}

function createSocketWithTimeout(ip, port, timeoutMs = CONNECT_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    const socket = new net.Socket();
    let connected = false;

    const timer = setTimeout(() => {
      if (!connected) {
        socket.destroy();
        reject(new Error(`Connection timeout to ${ip}:${port}`));
      }
    }, timeoutMs);

    socket.on('connect', () => {
      connected = true;
      clearTimeout(timer);
      resolve(socket);
    });

    socket.on('error', (err) => {
      clearTimeout(timer);
      reject(err);
    });

    socket.connect(port, ip);
  });
}

// LIST_STORAGE command
async function listStorage(ip, port) {
  log(`LIST_STORAGE`);
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      if (!resolved) {
        resolved = true;
        socket.destroy();
        reject(new Error('Read timed out'));
      }
    });

    socket.on('data', (chunk) => {
      if (resolved) return;
      data = Buffer.concat([data, chunk]);
      const str = data.toString('utf8');
      if (str.startsWith('ERROR:')) {
        resolved = true;
        socket.destroy();
        reject(new Error(str.trim()));
        return;
      }
      if (str.includes('\n]\n') || str.endsWith('\n]') || str.endsWith(']\n')) {
        resolved = true;
        socket.destroy();
        try {
          const jsonEnd = str.lastIndexOf(']');
          const jsonStr = str.substring(0, jsonEnd + 1);
          const entries = JSON.parse(jsonStr);
          log(`LIST_STORAGE returned ${entries.length} storage locations`);
          resolve(entries);
        } catch (e) {
          reject(new Error(`Invalid JSON: ${e.message}`));
        }
      }
    });

    socket.on('error', (err) => {
      if (!resolved) {
        resolved = true;
        reject(err);
      }
    });

    socket.on('close', () => {
      if (!resolved) {
        resolved = true;
        if (data.length > 0) {
          try {
            const str = data.toString('utf8');
            const jsonEnd = str.lastIndexOf(']');
            const jsonStr = str.substring(0, jsonEnd + 1);
            resolve(JSON.parse(jsonStr));
          } catch (e) {
            reject(new Error(`Invalid JSON on close: ${e.message}`));
          }
        } else {
          reject(new Error('Connection closed before response'));
        }
      }
    });

    socket.write('LIST_STORAGE\n');
  });
}

// LIST_DIR command (same as file browser uses)
async function listDir(ip, port, dirPath) {
  log(`LIST_DIR ${dirPath}`);
  const socket = await createSocketWithTimeout(ip, port);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;
    const MAX_RESPONSE_SIZE = 10 * 1024 * 1024;

    socket.setTimeout(READ_TIMEOUT_MS);
    socket.on('timeout', () => {
      if (!resolved) {
        resolved = true;
        socket.destroy();
        reject(new Error('Read timed out'));
      }
    });

    socket.on('data', (chunk) => {
      if (resolved) return;
      data = Buffer.concat([data, chunk]);
      if (data.length > MAX_RESPONSE_SIZE) {
        resolved = true;
        socket.destroy();
        reject(new Error('Response too large'));
        return;
      }
      const str = data.toString('utf8');
      if (str.startsWith('ERROR:') || str.startsWith('ERROR ')) {
        resolved = true;
        socket.destroy();
        reject(new Error(str.trim()));
        return;
      }
      if (str.includes('\n]\n') || str.endsWith('\n]') || str.endsWith(']\n')) {
        resolved = true;
        socket.destroy();
        try {
          const jsonEnd = str.lastIndexOf(']');
          const jsonStr = str.substring(0, jsonEnd + 1);
          const entries = JSON.parse(jsonStr);
          log(`LIST_DIR ${dirPath} returned ${entries.length} entries`);
          resolve(entries);
        } catch (e) {
          reject(new Error(`Invalid JSON: ${e.message}`));
        }
      }
    });

    socket.on('error', (err) => {
      if (!resolved) {
        resolved = true;
        reject(err);
      }
    });

    socket.on('close', () => {
      if (!resolved) {
        resolved = true;
        if (data.length > 0) {
          try {
            const str = data.toString('utf8');
            if (str.startsWith('ERROR')) {
              reject(new Error(str.trim()));
              return;
            }
            const jsonEnd = str.lastIndexOf(']');
            const jsonStr = str.substring(0, jsonEnd + 1);
            resolve(JSON.parse(jsonStr));
          } catch (e) {
            reject(new Error(`Invalid JSON on close`));
          }
        } else {
          reject(new Error('Connection closed before response'));
        }
      }
    });

    socket.write(`LIST_DIR ${dirPath}\n`);
  });
}

// DOWNLOAD_RAW command - this is what games scan uses to fetch param.json and covers
async function downloadRaw(ip, port, remotePath, maxBytes = 8 * 1024 * 1024) {
  log(`DOWNLOAD_RAW ${remotePath}`);
  const socket = await createSocketWithTimeout(ip, port);
  socket.setTimeout(0); // No timeout during download

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
        log(`DOWNLOAD_RAW ${remotePath} ERROR: ${err.message}`);
        reject(err);
        return;
      }
      log(`DOWNLOAD_RAW ${remotePath} OK (${value.length} bytes)`);
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
        const match = line.match(/^READY\s+(\d+)/i);
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
          finish(new Error(`File too large: ${remotePath} (${expectedSize} > ${maxBytes})`));
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
        finish(new Error(`File exceeded limit: ${remotePath}`));
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

    socket.write(`DOWNLOAD_RAW ${remotePath}\n`);
  });
}

// Same path joining as desktop app
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

// Map with concurrency limit (same as desktop)
async function mapWithConcurrency(items, limit, fn) {
  const results = [];
  const executing = [];

  for (let i = 0; i < items.length; i++) {
    const p = Promise.resolve().then(() => fn(items[i], i));
    results.push(p);

    if (limit <= items.length) {
      const e = p.then(() => executing.splice(executing.indexOf(e), 1));
      executing.push(e);
      if (executing.length >= limit) {
        await Promise.race(executing);
      }
    }
  }

  return Promise.all(results);
}

// Load game metadata - exactly as desktop does
async function loadRemoteGameMetaForPath(ip, gamePath) {
  const paramCandidates = [
    joinRemoteScanPath(gamePath, 'sce_sys', 'param.json'),
    joinRemoteScanPath(gamePath, 'param.json')
  ];

  let markerFile = null;
  let meta = null;

  for (const paramPath of paramCandidates) {
    try {
      const bytes = await downloadRaw(ip, TRANSFER_PORT, paramPath, 512 * 1024);
      const parsed = JSON.parse(bytes.toString('utf8'));
      // Simple validation
      if (parsed && (parsed.titleId || parsed.contentId || parsed.title)) {
        markerFile = paramPath;
        meta = parsed;
        break;
      }
    } catch {
      // Continue to next candidate
    }
  }

  if (!meta) {
    return { marker_file: null, meta: null, cover: null };
  }

  // Try to load cover (this triggers many DOWNLOAD_RAW calls)
  let cover = null;
  if (markerFile) {
    const isSceSysParam = markerFile.toLowerCase().includes('/sce_sys/param.json');
    const sceBase = isSceSysParam
      ? markerFile.slice(0, markerFile.toLowerCase().lastIndexOf('/param.json'))
      : joinRemoteScanPath(gamePath, 'sce_sys');

    for (const name of COVER_CANDIDATES) {
      const candidates = [
        joinRemoteScanPath(sceBase, name),
        joinRemoteScanPath(gamePath, name)
      ];
      for (const candidate of candidates) {
        try {
          const bytes = await downloadRaw(ip, TRANSFER_PORT, candidate, 8 * 1024 * 1024);
          if (bytes && bytes.length > 0) {
            cover = `<${bytes.length} bytes>`;
            break;
          }
        } catch {
          // Try next
        }
      }
      if (cover) break;
    }
  }

  return { marker_file: markerFile, meta, cover };
}

// Main scan function - replicates scanRemoteGames exactly
async function scanRemoteGames(ip, storagePaths = [], scanPaths = []) {
  const scannedStorage = [];
  const skippedStorage = [];
  const scannedGamesDirs = [];
  const games = [];

  const roots = Array.isArray(storagePaths)
    ? storagePaths.filter((value) => typeof value === 'string' && value.trim().length > 0)
    : [];

  const scanSubpaths = scanPaths.length > 0 ? scanPaths : DEFAULT_GAMES_SCAN_PATHS;

  log(`Scanning ${roots.length} storage roots with subpaths: ${scanSubpaths.join(', ')}`);

  for (const storagePath of roots) {
    log(`\n=== Checking storage: ${storagePath} ===`);
    try {
      await listDir(ip, TRANSFER_PORT, storagePath);
      scannedStorage.push(storagePath);
    } catch (err) {
      log(`Storage ${storagePath} not accessible: ${err.message}`);
      skippedStorage.push(storagePath);
      continue;
    }

    for (const subpath of scanSubpaths) {
      const gamesDir = joinRemoteScanPath(storagePath, subpath);
      log(`\n--- Scanning games dir: ${gamesDir} ---`);

      let folderEntries = [];
      try {
        folderEntries = await listDir(ip, TRANSFER_PORT, gamesDir);
        scannedGamesDirs.push(gamesDir);
      } catch (err) {
        log(`Games dir ${gamesDir} not accessible: ${err.message}`);
        continue;
      }

      const gameFolders = folderEntries.filter((entry) => entry.type === 'dir' && entry.name);
      log(`Found ${gameFolders.length} potential game folders`);

      // THIS IS THE KEY PART - concurrency of 4, multiple DOWNLOAD_RAW per folder
      const found = await mapWithConcurrency(gameFolders, 4, async (entry) => {
        const folderName = String(entry.name);
        const gamePath = joinRemoteScanPath(gamesDir, folderName);
        log(`\nProcessing game folder: ${gamePath}`);
        try {
          const details = await loadRemoteGameMetaForPath(ip, gamePath);
          if (!details.marker_file && !details.meta) {
            log(`No game metadata found in ${gamePath}`);
            return null;
          }
          log(`Found game: ${details.meta?.title || folderName}`);
          return {
            storage_path: storagePath,
            games_path: gamesDir,
            path: gamePath,
            folder_name: folderName,
            marker_file: details.marker_file || null,
            meta: details.meta || null,
            cover: details.cover || null
          };
        } catch (err) {
          log(`Error processing ${gamePath}: ${err.message}`);
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

// Stress test - hammer payload with DOWNLOAD_RAW for non-existent files
async function stressTestDownloadRaw(ip) {
  log('=== STRESS TEST: DOWNLOAD_RAW for non-existent files ===');
  log('This simulates what happens when games scan tries to find param.json/covers\n');

  const nonExistentPaths = [];
  // Generate many paths that don't exist (like games scan does)
  for (let i = 0; i < 50; i++) {
    nonExistentPaths.push(`/data/etaHEN/games/fake_game_${i}/sce_sys/param.json`);
    nonExistentPaths.push(`/data/etaHEN/games/fake_game_${i}/param.json`);
    nonExistentPaths.push(`/data/etaHEN/games/fake_game_${i}/sce_sys/cover.jpg`);
    nonExistentPaths.push(`/data/etaHEN/games/fake_game_${i}/sce_sys/icon0.png`);
  }

  log(`Testing ${nonExistentPaths.length} non-existent paths with concurrency 4...\n`);

  let successCount = 0;
  let errorCount = 0;

  await mapWithConcurrency(nonExistentPaths, 4, async (path) => {
    try {
      await downloadRaw(ip, TRANSFER_PORT, path, 512 * 1024);
      successCount++;
    } catch (err) {
      // Expected - files don't exist
      if (err.message.includes('ERROR') || err.message.includes('Not a file')) {
        errorCount++;
      } else {
        log(`Unexpected error for ${path}: ${err.message}`);
        throw err;
      }
    }
  });

  log(`\nStress test complete: ${successCount} found, ${errorCount} not found (expected)`);
}

// Test LIST_DIR only (like file browser)
async function testListDir(ip) {
  log('=== TEST: LIST_DIR only (like file browser) ===\n');

  const storage = await listStorage(ip, TRANSFER_PORT);
  const storagePaths = storage.map(s => s.path).filter(Boolean);

  for (const storagePath of storagePaths) {
    try {
      const entries = await listDir(ip, TRANSFER_PORT, storagePath);
      log(`${storagePath}: ${entries.length} entries`);

      // Try a few subdirectories
      const dirs = entries.filter(e => e.type === 'dir').slice(0, 3);
      for (const dir of dirs) {
        try {
          const subPath = `${storagePath}/${dir.name}`;
          const subEntries = await listDir(ip, TRANSFER_PORT, subPath);
          log(`  ${subPath}: ${subEntries.length} entries`);
        } catch (err) {
          log(`  ${storagePath}/${dir.name}: ${err.message}`);
        }
      }
    } catch (err) {
      log(`${storagePath}: ${err.message}`);
    }
  }

  log('\nLIST_DIR test complete');
}

// Sequential games scan (no concurrency)
async function scanRemoteGamesSequential(ip, storagePaths = [], scanPaths = []) {
  const scannedStorage = [];
  const skippedStorage = [];
  const scannedGamesDirs = [];
  const games = [];

  const roots = Array.isArray(storagePaths)
    ? storagePaths.filter((value) => typeof value === 'string' && value.trim().length > 0)
    : [];

  const scanSubpaths = scanPaths.length > 0 ? scanPaths : DEFAULT_GAMES_SCAN_PATHS;

  log(`Sequential scan of ${roots.length} storage roots`);

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

      const gameFolders = folderEntries.filter((entry) => entry.type === 'dir' && entry.name);

      // SEQUENTIAL - no concurrency, one at a time
      for (const entry of gameFolders) {
        const folderName = String(entry.name);
        const gamePath = joinRemoteScanPath(gamesDir, folderName);
        log(`Processing: ${gamePath}`);
        try {
          const details = await loadRemoteGameMetaForPath(ip, gamePath);
          if (details.marker_file || details.meta) {
            games.push({
              path: gamePath,
              folder_name: folderName,
              meta: details.meta || null
            });
          }
        } catch {
          // ignore
        }
      }
    }
  }

  return { games, scanned_storage: scannedStorage, scanned_games_dirs: scannedGamesDirs, skipped_storage: skippedStorage };
}

// Main entry point
async function main() {
  if (!ip) {
    console.error('Usage: node test-games-scan.js <PS5_IP> [--stress|--listdir|--sequential]');
    console.error('');
    console.error('Modes:');
    console.error('  (default)     Full games scan with concurrency 4');
    console.error('  --stress      Stress test DOWNLOAD_RAW with non-existent files');
    console.error('  --listdir     Test LIST_DIR only (like file browser)');
    console.error('  --sequential  Games scan without concurrency (one request at a time)');
    process.exit(1);
  }

  log(`Starting test against ${ip}:${TRANSFER_PORT} (mode: ${MODE})\n`);

  try {
    if (MODE === 'stress') {
      await stressTestDownloadRaw(ip);
    } else if (MODE === 'listdir') {
      await testListDir(ip);
    } else if (MODE === 'sequential') {
      log('=== Sequential Games Scan (no concurrency) ===\n');
      const storage = await listStorage(ip, TRANSFER_PORT);
      const storagePaths = storage.map(s => s.path).filter(Boolean);
      const result = await scanRemoteGamesSequential(ip, storagePaths);
      log(`\nComplete: ${result.games.length} games found`);
    } else {
      // Full scan (default)
      log('=== Full Games Scan (concurrency 4) ===\n');
      const storage = await listStorage(ip, TRANSFER_PORT);
      const storagePaths = storage.map(s => s.path).filter(Boolean);
      log(`Found storage paths: ${storagePaths.join(', ')}\n`);

      const result = await scanRemoteGames(ip, storagePaths);

      log('\n=== SCAN COMPLETE ===');
      log(`Scanned storage: ${result.scanned_storage.join(', ')}`);
      log(`Skipped storage: ${result.skipped_storage.join(', ')}`);
      log(`Scanned game dirs: ${result.scanned_games_dirs.join(', ')}`);
      log(`Games found: ${result.games.length}`);

      for (const game of result.games) {
        log(`  - ${game.meta?.title || game.folder_name} @ ${game.path}`);
      }
    }

    log('\n✓ Test completed without crashing the payload!');
  } catch (err) {
    log(`\n✗ FATAL ERROR: ${err.message}`);
    log(err.stack);
    process.exit(1);
  }
}

main();
