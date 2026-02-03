#!/usr/bin/env node
/**
 * Debug script to check payload crash logs and test individual operations
 */

const net = require('net');

const TRANSFER_PORT = 9113;
const CONNECT_TIMEOUT_MS = 5000;

const ip = process.argv[2];
if (!ip) {
  console.error('Usage: node test-crash-debug.js <PS5_IP>');
  process.exit(1);
}

function log(msg) {
  console.log(`[${new Date().toISOString()}] ${msg}`);
}

function createSocket(ip, port) {
  return new Promise((resolve, reject) => {
    const socket = new net.Socket();
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error('Connection timeout'));
    }, CONNECT_TIMEOUT_MS);

    socket.on('connect', () => {
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

async function sendCommand(ip, cmd) {
  log(`Sending: ${cmd.trim()}`);
  const socket = await createSocket(ip, TRANSFER_PORT);

  return new Promise((resolve, reject) => {
    let data = Buffer.alloc(0);
    let resolved = false;

    socket.setTimeout(30000);
    socket.on('timeout', () => {
      if (!resolved) {
        resolved = true;
        socket.destroy();
        reject(new Error('Timeout'));
      }
    });

    socket.on('data', (chunk) => {
      data = Buffer.concat([data, chunk]);
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
        const str = data.toString('utf8');
        log(`Response (${data.length} bytes): ${str.slice(0, 500)}${str.length > 500 ? '...' : ''}`);
        resolve(str);
      }
    });

    socket.write(cmd);
  });
}

async function downloadRaw(ip, path) {
  log(`DOWNLOAD_RAW ${path}`);
  const socket = await createSocket(ip, TRANSFER_PORT);

  return new Promise((resolve, reject) => {
    let headerDone = false;
    let headerBuf = Buffer.alloc(0);
    let expectedSize = 0;
    const chunks = [];
    let received = 0;
    let settled = false;

    const finish = (err, value) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      if (err) {
        log(`DOWNLOAD_RAW ERROR: ${err.message}`);
        reject(err);
      } else {
        log(`DOWNLOAD_RAW OK: ${value.length} bytes`);
        resolve(value);
      }
    };

    socket.setTimeout(10000);
    socket.on('timeout', () => finish(new Error('Timeout')));

    socket.on('data', (chunk) => {
      if (!headerDone) {
        headerBuf = Buffer.concat([headerBuf, chunk]);
        const nl = headerBuf.indexOf('\n');
        if (nl === -1) return;
        const line = headerBuf.slice(0, nl).toString('utf8').trim();
        const remainder = headerBuf.slice(nl + 1);

        if (line.startsWith('ERROR')) {
          finish(new Error(line));
          return;
        }
        const match = line.match(/^READY\s+(\d+)/i);
        if (!match) {
          finish(new Error(`Unexpected: ${line}`));
          return;
        }
        expectedSize = Number(match[1]);
        headerDone = true;
        if (remainder.length > 0) {
          chunks.push(remainder);
          received += remainder.length;
        }
      } else {
        chunks.push(chunk);
        received += chunk.length;
      }

      if (headerDone && received >= expectedSize) {
        finish(null, Buffer.concat(chunks).subarray(0, expectedSize));
      }
    });

    socket.on('error', (err) => finish(err));
    socket.on('close', () => {
      if (!headerDone) {
        finish(new Error('Connection closed before header'));
      } else if (received < expectedSize) {
        finish(new Error(`Incomplete: ${received}/${expectedSize}`));
      }
    });

    socket.write(`DOWNLOAD_RAW ${path}\n`);
  });
}

async function main() {
  log('=== Crash Debug Test ===\n');

  // Test 1: Check if payload is alive
  log('--- Test 1: VERSION check ---');
  try {
    await sendCommand(ip, 'VERSION\n');
    log('Payload is alive\n');
  } catch (err) {
    log(`Payload not responding: ${err.message}`);
    log('Please restart the payload and try again');
    process.exit(1);
  }

  // Test 2: LIST_STORAGE (should work)
  log('--- Test 2: LIST_STORAGE ---');
  try {
    await sendCommand(ip, 'LIST_STORAGE\n');
    log('LIST_STORAGE works\n');
  } catch (err) {
    log(`LIST_STORAGE failed: ${err.message}\n`);
  }

  // Test 3: Check if payload still alive
  log('--- Test 3: VERSION check after LIST_STORAGE ---');
  try {
    await sendCommand(ip, 'VERSION\n');
    log('Still alive\n');
  } catch (err) {
    log(`Payload died after LIST_STORAGE: ${err.message}`);
    process.exit(1);
  }

  // Test 4: LIST_DIR (should work - same as file browser)
  log('--- Test 4: LIST_DIR /data ---');
  try {
    await sendCommand(ip, 'LIST_DIR /data\n');
    log('LIST_DIR works\n');
  } catch (err) {
    log(`LIST_DIR failed: ${err.message}\n`);
  }

  // Test 5: Check if payload still alive
  log('--- Test 5: VERSION check after LIST_DIR ---');
  try {
    await sendCommand(ip, 'VERSION\n');
    log('Still alive\n');
  } catch (err) {
    log(`Payload died after LIST_DIR: ${err.message}`);
    process.exit(1);
  }

  // Test 6: Try to read crash log first (if exists)
  log('--- Test 6: Check for crash log ---');
  try {
    const crashLog = await downloadRaw(ip, '/data/ps5upload/payload_crash.log');
    log('Crash log contents:');
    console.log(crashLog.toString('utf8'));
  } catch (err) {
    log(`No crash log or error reading it: ${err.message}\n`);
  }

  // Test 7: Check if payload still alive after DOWNLOAD_RAW attempt
  log('--- Test 7: VERSION check after DOWNLOAD_RAW crash log ---');
  try {
    await sendCommand(ip, 'VERSION\n');
    log('Still alive after DOWNLOAD_RAW!\n');
  } catch (err) {
    log(`*** PAYLOAD CRASHED on DOWNLOAD_RAW! ***`);
    log(`Error: ${err.message}`);
    log('\nThe crash is in handle_download_raw or download_raw_stream');
    process.exit(1);
  }

  // Test 8: Try DOWNLOAD_RAW on a known small file
  log('--- Test 8: DOWNLOAD_RAW on /data/ps5upload/payload.pid ---');
  try {
    const pid = await downloadRaw(ip, '/data/ps5upload/payload.pid');
    log(`PID file: ${pid.toString('utf8').trim()}\n`);
  } catch (err) {
    log(`Failed: ${err.message}\n`);
  }

  // Test 9: Final check
  log('--- Test 9: Final VERSION check ---');
  try {
    await sendCommand(ip, 'VERSION\n');
    log('Payload survived all tests!');
  } catch (err) {
    log(`Payload crashed: ${err.message}`);
    process.exit(1);
  }

  log('\n=== Debug complete ===');
}

main().catch(err => {
  log(`Fatal: ${err.message}`);
  process.exit(1);
});
