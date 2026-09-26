#!/usr/bin/env node
// Best-effort cleanup of stale ps5upload-desktop / ps5upload-engine processes
// plus any Vite dev server holding :1420. Invoked as a `make run-client`
// dependency so a previously mis-terminated session doesn't fail the next
// launch with "Port 1420 already in use" or two engines fighting for ports.
//
// Cross-platform via native tooling:
//   - Linux/macOS: pkill, lsof, ps, kill (POSIX)
//   - Windows:     taskkill, netstat, PowerShell Get-CimInstance
//
// The POSIX branch uses the `[p]` regex bracket trick to keep pkill from
// matching its own argv. Without it, `pkill -f ps5upload-desktop` matches
// the calling shell's `/proc/<pid>/cmdline` (which contains the literal
// substring) and SIGTERMs the make recipe — see Makefile _kill-stale-client.

import { execSync } from 'node:child_process';

const isWin = process.platform === 'win32';
const PORT = 1420;

function run(cmd) {
  return execSync(cmd, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] });
}

function silent(cmd) {
  try { execSync(cmd, { stdio: 'ignore' }); } catch { /* expected: no match */ }
}

if (isWin) {
  silent('taskkill /IM ps5upload-desktop.exe /F');
  silent('taskkill /IM ps5upload-engine.exe /F');
} else {
  silent("pkill -f '[p]s5upload-desktop'");
  silent("pkill -f '[p]s5upload-engine'");
}

function pidsOnPort(port) {
  try {
    if (isWin) {
      const out = run('netstat -ano -p tcp');
      const pids = new Set();
      const re = new RegExp(`\\s\\S+:${port}\\s+\\S+\\s+LISTENING\\s+(\\d+)`);
      for (const line of out.split(/\r?\n/)) {
        const m = line.match(re);
        if (m) pids.add(m[1]);
      }
      return [...pids];
    }
    const out = run(`lsof -ti :${port}`);
    return out.trim().split(/\s+/).filter(Boolean);
  } catch {
    return [];
  }
}

function looksLikeOurVite(pid) {
  try {
    if (isWin) {
      const out = run(
        `powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter 'ProcessId=${pid}').CommandLine"`,
      );
      return /ps5upload[\s\S]*vite/i.test(out);
    }
    const out = run(`ps -o command= -p ${pid}`);
    return /ps5upload.*node_modules.*vite/.test(out);
  } catch {
    return false;
  }
}

for (const pid of pidsOnPort(PORT)) {
  if (looksLikeOurVite(pid)) {
    silent(isWin ? `taskkill /PID ${pid} /F` : `kill -9 ${pid}`);
    console.log(`✓ killed stale Vite on :${PORT} (pid ${pid})`);
  }
}

// Brief settle so freed sockets transition out of TIME_WAIT before tauri dev
// reopens the port. Equivalent to the previous `sleep 1` in the Makefile.
await new Promise((resolve) => setTimeout(resolve, 1000));
