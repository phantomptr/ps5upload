# ps5upload vs elf-arsenal — Gap Analysis & Fix Plan

**Scope:** Cross-reference the elf-arsenal reference repo
(`git.earthonion.com/soniciso/elf-arsenal`, v1.6.21) against the current
ps5upload HEAD (v3.3.24) to find concrete, code-level fixes for the open
user-reported issues:

- #164 — FW 12.40: remote operations not working (PKG installs, folder
  uploads, connection losses), plus feature requests (web UI, persistent
  fan speed, NP fake sign-in / user-ID management).
- #152 — Patch (update) PKGs can't be installed; the on-PS5 `ps5upload`
  helper process dies ~4–7 s after the first install attempt is rejected.
- #81 — Direct/streaming install request (avoid double-storage).

Each item below states: **Symptom → Root cause → elf-arsenal's proven
technique (with file:line) → ps5upload gap → Concrete fix → Priority &
risk.** Priorities: P0 = data-loss / crash, P1 = the headline bug users
hit, P2 = feature parity, P3 = polish.

---

## 0. Status of the three "headline" bugs (already merged)

Before pulling new fixes, confirm what's **already** in v3.3.24 so we
don't redo work:

| Bug | Status | Commit / code |
| --- | --- | --- |
| **A. Early staging-delete** (engine deletes the staged `.pkg` the instant the first install attempt is rejected, so the fallback has nothing to install) | **FIXED** | `engine/.../pkg_install.rs` `install_verdict` (L977–1020), delete gate (L1370), guarded-patch preservation (L668–700); commit `ad74098` + the verdict rewrite. Pinned by 9 unit tests (L2092–2279). |
| **B. `pt_mmap` treats kernel errno as a valid pointer** (ShellUI scratch mmap returns e.g. `-ENOMEM` → dereferenced → ShellUI crash → watchdog kills helper) | **FIXED** | `payload/src/ptrace_remote.c:355–368` now collapses `[-4095,-1]` → `-1`; callers in `shellui_rpc.c` (L399, 438, 486, 765) check `scratch == -1 \|\| scratch == 0`. Commit `a9fe56d` (#173). |
| **C. Installed-Apps / Library poll every few seconds on the same mgmt channel as a big upload → speed collapses → "upload failed"** | **FIXED** | `client/src/lib/ps5Transfers.ts` `transferScreenBusy` (L33–50), gates in `InstalledApps/index.tsx:494`, `RunningAppsPanel.tsx:155`, `pkgLibrary.ts` (L1231, 1386, 1699, 1822). Commit `3fe35e2` (#164). |

**These need hardware confirmation on FW 10.40 / 12.40 / 12.70 (the
issue threads are explicitly waiting on that), but the code-level fixes
are in.** The remaining work is the **gap analysis below** — things the
reference does that ps5upload still doesn't, which explain the residual
"helper dies" and "patch won't apply" reports **even after** A/B/C.

---

## P0-1 — DPI daemon has no `sceAppInstUtilInitialize` + no authid escalation

### Symptom (#152, the part still open after fix B)
On FW 10.40 the in-process `sceAppInstUtilInstallByPackage` rejects a
PS4 patch (`package_type=PS4DP`) with `0x80B21106` and returns cleanly.
The engine then hands the staged path to the standalone DPI daemon
(`payload/dpi/ezremote_dpi.c`). The helper goes unreachable ~4 s later.
The DPI fallback runs with **zero diagnostics** (only fixed for logging
by #178; the *behavior* is still broken).

### Root cause (read `ezremote_dpi.c` end-to-end)
`ezremote_dpi.c:65` `int main(void)` does **three things missing from
the reference**:

1. **Never calls `sceAppInstUtilInitialize()`** before
   `sceAppInstUtilInstallByPackage` (L158). The reference initializes
   exactly once, mutex-guarded (`elf-arsenal src/homebrew.c:515–519`),
   and *retries init on every request* if startup init failed
   (`payloads-src/dpi/main.c:150–165`). On FW 10.40/12.x Sony's
   installer can return `0x80B21106` purely because IPMI was never
   brought up in this process. Calling `InstallByPackage` cold leaves
   the IPMI state machine half-wedged, and the next install-service
   event trips the watchdog.
2. **Never escalates its own credentials.** The reference calls
   `jb_escalate_pid(getpid())` at the top of the install worker
   (`homebrew.c:720`) and the DPI v1 payload is spawned pre-escalated
   (`src/ps5/sys.c:1280–1521`). `ezremote_dpi.c` relies entirely on the
   elfldr loader's pristine ucred — which on later FW is **not** an
   install-service authid. The main ps5upload payload sidesteps this
   with an authid-swap window (`bgft.c:518`
   `authid_acquire("InstallByPackage", install_authid)`); the DPI daemon
   has no such window.
3. **No boot-timing wait.** Reference DPI v1 sleeps 25 s before init to
   let kstuff kernel patches land (`payloads-src/dpi/main.c:103–117`),
   with a 10 s timeout on init itself (`timed_init`, L83–100).
   `ezremote_dpi.c` binds and serves immediately.

### elf-arsenal's proven pattern (`payloads-src/dpi/main.c:83–186`)
```c
static int timed_init(void) {
    pthread_create(&tid, NULL, init_thread, NULL);   /* sceAppInstUtilInitialize */
    pthread_detach(tid);
    for (int i = 0; i < INIT_TIMEOUT * 10; i++) {
        if (g_init_done) return g_init_rc;
        nanosleep(&(struct timespec){0,100000000}, NULL);   /* 100 ms */
    }
    return -0xDEAD;                                       /* timeout sentinel */
}
/* per request, retry init if startup failed: */
if (init_rc != 0) { init_rc = timed_init(); }
if (init_rc != 0) { send(cl, "error:init:0x%08X", ...); continue; }
```

### Concrete fix (ps5upload)
In `payload/dpi/ezremote_dpi.c`, before the `accept` loop:
- Add a `jb_escalate_pid(getpid())` (port the 45-line `jb.c` from the
  reference, or reuse the main payload's existing escalate path — it's
  already linked into `ps5upload.elf`). Set authid `0x4801000000000013`,
  privcaps `0xff`, uid/gid = 0.
- Sleep 25 s in 500 ms steps (kstuff patch window), then call
  `sceAppInstUtilInitialize()` on a detached thread with a 10 s timeout.
- On **every** accepted request, if `init_rc != 0`, retry `timed_init()`
  once; on timeout reply `error:init:timeout` instead of calling
  `InstallByPackage` cold.
- Reply with the **hex** `0x%08X` form on error (the engine's
  `dpi_install_handler` at `pkg_install.rs:1628` already logs hex, but
  the daemon currently replies **decimal** — `snprintf(resp, "%d", ret)`
  at L167. The reference replies `error:0x%08X`. Pick one; hex is what
  the rest of the codebase logs).

### Priority / risk
**P0** — this is the most likely remaining cause of the "helper dies
~4 s after the rejected install" symptom on FW 10.40 that #152 is
explicitly still open on. Risk: low — the change is confined to the
177-line standalone daemon and mirrors a battle-tested reference. No
change to the main payload or engine.

### Test
- Existing `tests/install-fallback-hw.mjs` (hardware-gated) covers the
  cascade end-to-end; extend it to assert the daemon's reply is hex and
  that init is called (via a klog grep for `sceAppInstUtilInitialize`).
- Add a host-side unit test that a cold `InstallByPackage` returning
  `0x80B21106` from the daemon triggers the engine's "stage preserved
  for DPI" branch (already covered by the staging-preservation test in
  `pkg_install.rs:2092+`, just need the daemon to actually succeed).

---

## P0-2 — No "wait for app.db row" verification after `InstallByPackage` returns 0

### Symptom
#81 comment from `Constantine-HD`: *"Installed, but via a fallback that
may not launch on this firmware."* Also #152's RDR2 `ce-109596-0`. Sony's
`sceAppInstUtilInstallByPackage` returns `0` the instant BGFT **queues**
the task — not when the install completes, and not even guaranteed to
mean "will succeed." On FW 11/12 a queue-accept can still fail
asynchronously (authid gate fires later, PlayGo reject, disk full), and
the host reports success prematurely.

### Root cause
ps5upload's `install_verdict` (`pkg_install.rs:977`) treats
"InstallByPackage returned 0" as the trigger to **delete staging** and
report Complete. There is no cross-check that the title actually landed
in Sony's app database. The reference polls `app.db` for up to 90 s.

### elf-arsenal's proven pattern (`src/homebrew.c:692–714, 728–745`)
```c
static int wait_for_install_row(const char *title_id, int timeout_sec) {
    sqlite3 *db; sqlite3_stmt *st;
    sqlite3_open_v2("/system_data/priv/mms/app.db", &db,
                    SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX, NULL);
    sqlite3_prepare_v2(db,
        "SELECT 1 FROM tbl_contentinfo WHERE titleId=?1 LIMIT 1", ...);
    /* poll every 500 ms up to timeout_sec */
}
/* after install_pkg_at_path returns 0: */
if (rc == 0 && content_id[0]) {
    extract_title_id(content_id, tid, sizeof tid);   /* "IV9999-NPXS40047_00-..." → "NPXS40047" */
    if (!strcmp(tid, "FAKE00000")) rc = -1;          /* placeholder never rewritten */
    else if (wait_for_install_row(tid, 90) != 0) rc = -1;
}
```
The placeholder `FAKE00000` rejection is the crucial bit — Sony's
installer can accept a fake content_id and then silently drop it.

### Concrete fix (ps5upload)
Two-layer:

1. **Payload side** (`payload/src/bgft.c` near `appinst_task_register`):
   after a successful `InstallByPackage` / `AppInstallPkg`, extract the
   title_id from the content_id and poll
   `/system_data/priv/mms/app.db` `tbl_contentinfo` for up to 60 s.
   Expose the result through the existing install-status RPC so the
   engine's `install_verdict` can consume a new `registered` tri-state:
   `Confirmed` (row appeared), `Pending` (still polling),
   `Rejected` (timeout / placeholder). This makes "Complete" mean
   *actually installed*, not *queued*.
2. **Engine side** (`pkg_install.rs`): extend `InstallVerdict` so
   `Complete` requires `registered == Confirmed` on FW that supports
   the app.db read; fall back to the existing byte-settle path on FW
   where the db path isn't readable. Keep the current `Stalled` →
   keep-staging behaviour for `Pending`.

### Priority / risk
**P0** for the "may not launch" correctness issue; directly resolves
the false-success reports. Risk: medium — touches both payload and
engine; the db read needs the escalated authid from P0-1. Guard the
sqlite open behind a capability probe (some FW may not expose app.db to
the payload).

---

## P1-3 — Path-rewrite `/data/` → `/user/data/` missing for the DPI path

### Symptom
DPI daemon accepts the path string but `InstallByPackage` still fails
with a generic error; or the staged pkg is invisible to Sony's install
service (sandbox).

### Root cause
The engine's `dpi_install_handler` passes `req.local_ps5_path` verbatim
to the daemon (`pkg_install.rs:1620`). If the host staged the pkg at
`/data/ps5upload/...`, Sony's install service — which runs in a sandbox
that sees `/data/` as `/user/data/` — can't find the file. The reference
**always** rewrites the path before handing it to any installer
(`elf-arsenal src/homebrew.c:425–436`).

### elf-arsenal pattern
```c
static void rewrite_path_for_install(const char *in, char *out, size_t n) {
    if (strncmp(in, "/data/", 6) == 0) { snprintf(out, n, "/user%s", in); return; }
    strncpy(out, in, n - 1);
}
```

### Concrete fix
In `pkg_install.rs::dpi_install_handler`, rewrite the path before
sending: if it starts with `/data/`, send `/user/data/...`. Better:
do it inside the daemon so both the local and URL paths are correct
regardless of caller. One-line helper, pure function, trivially tested.

### Priority / risk
**P1**, low risk. Likely fixes a subset of "DPI fallback rejected"
reports that aren't the authid issue.

---

## P1-4 — Settle time + delete-retry between batch installs (the "Install all" pile-up)

### Symptom (#81)
`Constantine-HD`'s screenshot: queue installs base, update, DLC out of
order, multi-DLC stalls during the FW12 black screen, and
`fs_delete_failed` when the install service is still busy with the
previous title.

### Root cause
The engine's "Install all" (#174, commit `775487b`) batches staged
pkgs but doesn't (a) enforce base → update → DLC ordering, (b) wait
a settle window between installs, or (c) retry `fs_delete` on
`fs_delete_failed`.

### elf-arsenal's proven pattern
- **Ordering:** reference scans and queues base titles first, then
  patches, then DLC, by content_id suffix (`…GP` base / `…DP` patch /
  `…AC` DLC). See `homebrew.c:825–935`.
- **Settle:** `sleep(3)` between sequential installs
  (`homebrew.c:869`).
- **Delete retry:** the reference doesn't delete mid-batch at all; it
  defers cleanup. ps5upload's `fs_delete_with_timeout`
  (`pkg_install.rs:1389`) has a 10 s cap but no retry on
  `fs_delete_failed`.

### Concrete fix
1. **Sort the batch** in `pkg_install.rs` before issuing installs:
   parse the package_type suffix from the pkg header (already parsed
   by `ps5upload-pkg`) and order `GP` → `DP` → `AC` → everything else.
2. **Inject a configurable settle** (default 3 s) between the
   `Complete` of one session and the `install_start` of the next.
3. **Retry `fs_delete`** up to 3 times with a 2 s backoff on
   `fs_delete_failed` — the on-PS5 service is briefly busy post-install.

### Priority / risk
**P1**, low risk. Directly addresses the #81 thread's ordering
complaint and the `fs_delete_failed` report.

---

## P1-5 — DPI v2 / etaHEN-compatible HTTP bridge (direct/streaming install)

### Symptom (#81)
User wants to install a `.pkg` by **streaming** it from the PC, never
staging the whole file on PS5 storage first (the "double storage"
problem: pkg + installed game both take space).

### Root cause
Feature gap — the engine can already serve a pkg over HTTP (`/pkg-host/`,
referenced in `ezremote_dpi.c` comments L16), but there's no UI/API
path that points the PS5's installer at that URL for a streaming
one-pass install.

### elf-arsenal's proven pattern
Two tiers:
- **DPI v1** (`payloads-src/dpi/main.c`): plain TCP :9040, send URL,
  get `ok`/`error:0x...`. ps5upload already has this.
- **DPI v2** (`payloads-src/dpiv2/main.c`): HTTP bridge on :12800,
  etaHEN-compatible, forwards to the main installer on :6969 with
  `?sync=1` so the HTTP response reflects *actual* install success
  (polls `last-install.json`, `homebrew.c:1406–1459`). This is the
  front-door external tools call.

### Concrete fix (the "Direct install (beta)" the owner already promised in #81)
1. **Engine** (`pkg_install.rs`): add a new route
   `/api/pkg/direct-install` that:
   - Validates the local pkg path.
   - Builds an `http://<engine-host>:<port>/pkg-host/<session-id>/<file>`
     URL (the engine already serves `/pkg-host/`).
   - Sends that URL to the DPI daemon on :9040 (the daemon already
     accepts http:// URIs — `ezremote_dpi.c:152` `metainfo.uri = buffer`).
   - Tracks install status via the same `install_verdict` machinery,
     but with `staging_path = None` (nothing to delete — the file
     lives on the PC).
2. **Client:** add a "Direct install (beta)" toggle on the Install
   dialog, clearly labelled with the caveats from the owner's #81 reply
   (PC must stay connected, FW-dependent reliability).
3. **Honest fallback:** if the direct path returns `0x80B22404`
   (PlayGo HTTP pre-flight reject) or any non-zero, automatically stage
   + retry via the normal upload-then-install path. Never silently
   fail.

### Priority / risk
**P1** (the owner already committed to shipping this beta). Risk:
medium — long-running HTTP install needs the engine's transfer-port
keepalive logic reused; a flaky LAN can drop the stream. Mitigate with
the auto-fallback.

---

## P1-6 — Connection-loss / helper-unreachable during long operations

### Symptom (#164)
"Often, I find my app disconnected from the console while I use it to
monitor the temperature and fan speed." Also #152's "helper goes
unreachable."

### Root cause (the pieces that aren't already fixed)
Fix B (mmap) addresses the ShellUI-crash cascade. Two residual causes:
1. **`recv_exact`/`read_exact` frames** (`connection.rs:397`,
   `runtime.c:1943`) surface as `"read frame header: failed to fill
   whole buffer"` with no retry — a single dropped TCP frame during a
   long-running install status poll kills the whole session.
2. **No watchdog keepalive on the mgmt channel.** The reference's
   helper doesn't have one either, but the app's polling every few
   seconds *is* the keepalive — and fix C paused that polling during
   uploads, so during a long folder upload the mgmt channel can go
   idle long enough for Sony's network stack to drop it.

### Concrete fix
1. **Distinguish fatal vs transient on the read path.**
   `connection.rs::recv_header` (L394) should classify `UnexpectedEof`
   + `ConnectionReset` as *session-recoverable* (the engine already has
   `is_retryable_transfer_error` in `transfer.rs:380–414` for the
   transfer port — extend the same idea to the mgmt port). On a
   recoverable error during an install-status poll, **mark the session
   "reconnecting"** rather than failing the install.
2. **Add a mgmt-channel liveness ping** that runs independent of the
   paused-during-upload pollers. A 30 s NOOP frame costs nothing and
   keeps the TCP path warm. The payload already handles NOOP
   (`runtime.c` `handle_binary_frame`).
3. **Re-ARM the auto-reconnect** (the engine already has
   `payload_lifecycle.rs` for the loader port) to cover the case where
   the helper genuinely crashed: detect unreachable :9114, re-send the
   payload via the loader, restore the in-flight install session from
   the engine's session map.

### Priority / risk
**P1**, medium risk (touches the connection layer). The
session-restore is the delicate part — guard it so a *real* crash
doesn't get papered over (correlate with the bug-bundle klog).

---

## P2-7 — Persistent fan threshold on payload launch (autorun)

### Symptom (#164 feature request)
"a way to permanently set the fan speed every time the payload launches
… so it can be put in the payload manager autorun."

### Root cause
Feature gap. ps5upload sets the fan threshold live via `/dev/icc_fan`
ioctls (`0xC01C8F07` threshold, `0xC0068F06` duty) but doesn't persist
it or re-apply it.

### elf-arsenal's proven pattern (`src/fan.c`)
- `atomic_int g_pinned_threshold_c` — persists across config changes.
- `FAN_REAPPLY_SEC = 15` — watcher thread re-applies the threshold
  every 15 s so Sony's thermal manager can't undo it.
- Config is loaded at boot from `/data/elf-arsenal/...` and the watcher
  starts unconditionally — so any payload launch (including from an
  autorun) inherits the pinned threshold.

### Concrete fix
1. **Payload** (`payload/src/`): add a fan watcher thread started from
   `main.c` boot that reads a pinned threshold from a config file
   (`/data/ps5upload/fan.json` or an RPC-set value) and re-applies it
   every 15 s. Re-uses the existing ioctl code.
2. **Engine + client:** add a "Fan" settings panel with "Pin threshold"
   that writes the config and pushes it live. The threshold then
   survives payload relaunch / autorun because the watcher reads it
   from disk at boot.

### Priority / risk
**P2**, low risk. Self-contained payload + engine feature.

---

## P2-8 — NP fake sign-in / local-user ID management (Offact-equivalent)

### Symptom (#164 feature request)
"a way to add, modify, and activate the user ID for the local accounts
(like Offact + NP Fake sign in)."

### Root cause
Feature gap. ps5upload has no NP/offline-account surface.

### elf-arsenal's proven pattern
- `src/offact.c` (117 lines): offline activation via `sceRegMgr*`
  registry writes. Per-slot key derivation
  (`slot_key(slot, base, magic1, magic2)`), 4 keys per account
  (name/id/type/flags). ID generation: FNV-style hash with magic
  `0x5EAF00D / 0xCA7F00D`:
  ```c
  uint64_t offact_gen_id(const char *name) {
      uint64_t h = 0x5EAF00D / 0xCA7F00D;
      while (*name) h = 0x100000001B3ULL * (h ^ (uint8_t)*name++);
      return h;
  }
  ```
- `src/np.c` (141 lines): web endpoints to set the fake NP signed-in
  state.
- Exposed through `/api/np*` and `/api/offact*` routes.

### Concrete fix
Port the offact registry-write logic + the NP fake-sign-in RPC into
the ps5upload payload, expose mgmt-port RPCs, add an engine route
`/api/np/account` and a client panel "Accounts". Clearly warn that
this writes to the system registry and to back up first (the reference
takes a registry snapshot).

**Caveat (owner's call):** this is the feature most likely to "cause
kernel panics" the user is trying to avoid — ps5upload should treat it
as opt-in beta with a confirm dialog and a registry backup step.

### Priority / risk
**P2**, **higher risk** — registry writes can panic. Port the
reference's snapshot-first pattern (`fpkg_db.c` registry snapshots) and
gate behind a settings flag.

---

## P2-9 — Web-based UI (already in flight — track to ship)

### Symptom (#164 feature request)
"a web-based interface."

### Status
**Already done, shipping.** Commit `530af17` (#171) — engine serves the
full React client over HTTP. The Docker/`ghcr.io` image
(`3e9aafe` + `615b50a` + the Makefile docker targets) is the
self-hosted path. The only remaining work is **discoverability**: the
README and the in-app Connection screen should advertise the engine URL
so users know they can point a browser at it instead of installing the
Tauri app.

### Concrete fix
Docs/UX only — add a "Web UI" section to README.md and a hint in the
Connection screen. No code change.

### Priority / risk
**P3**, trivial.

---

## P3-10 — DPI daemon: plain protocol hardening

### Symptom
The daemon's protocol (`ezremote_dpi.c:120–170`) reads up to 4095 bytes
then `strtok`s on `\r`/`\n`. A malformed or partial send (e.g. the
engine's `dpi_send` splitting the write — `pkg_install.rs:1596–1597`
does two `write_all`s) could in principle interleave, though TCP stream
ordering makes this safe in practice.

### Concrete fix (defensive)
- Add a `Content-Length`-or-`\n`-terminated read loop (the reference
  reads until newline with a total cap).
- Reject paths containing `..` (`pkg_filename_is_safe`,
  `homebrew.c:1497–1511`) before passing to Sony's installer.
- Reply in the `error:0x%08X` / `ok` form the reference uses, so the
  engine's `dpi_install_handler` can parse a success/reject/timeout
  tri-state rather than just numeric rc.

### Priority / risk
**P3**, low risk, defence-in-depth.

---

## Implementation order (suggested)

1. **P0-1** (DPI escalate + init) — unblocks the #152 "helper dies"
   confirmation. Standalone daemon only.
2. **P1-3** (path rewrite) — one-liner, do alongside P0-1.
3. **P0-2** (app.db verify) — the false-success root cause; needs
   payload + engine.
4. **P1-4** (batch ordering + settle + delete retry) — the #81 pile-up.
5. **P1-5** (direct/streaming install beta) — the headline #81 ask;
   owner already promised it.
6. **P1-6** (connection-loss resilience) — the #164 reliability thread.
7. **P2-7 / P2-8** (fan persistence, NP accounts) — feature requests.
8. **P3-10 / P2-9** (polish).

Each is independently shippable. P0-1 + P1-3 together are the smallest
unit that could meaningfully move the #152 confirmation forward.

---

## Reference file index (elf-arsenal)

| Area | File | Key lines |
| --- | --- | --- |
| Install cascade | `src/homebrew.c` | 484–654 (cascade), 692–745 (app.db verify), 425–436 (path rewrite) |
| Async install + status | `src/homebrew.c` | 657–781 (worker, status file) |
| Bulk scan + order | `src/homebrew.c` | 825–935 |
| DPI v1 daemon | `payloads-src/dpi/main.c` | 83–186 (timed_init + retry) |
| DPI v2 HTTP bridge | `payloads-src/dpiv2/main.c` | :12800, `?sync=1` |
| Auth escalation | `src/jb.c` | 1–45 (jb_escalate_pid) |
| Fan control | `src/fan.c` | 15 s reapply, atomic pin |
| Offact / NP | `src/offact.c`, `src/np.c` | registry writes, FNV id |
| FPKG DB snapshots | `src/fpkg_db.c` | 1759+ (snapshot rotation) |
| Filename safety | `src/homebrew.c` | 1497–1511 |

## Reference file index (ps5upload, current)

| Area | File | Key lines |
| --- | --- | --- |
| Install verdict (delete gate) | `engine/.../pkg_install.rs` | 977–1020, 1370 |
| Guarded-patch staging keep | `engine/.../pkg_install.rs` | 668–700 |
| DPI send (host side) | `engine/.../pkg_install.rs` | 1585–1641 |
| DPI daemon (PS5 side) | `payload/dpi/ezremote_dpi.c` | 65–177 (whole file) |
| Install dispatch (payload) | `payload/src/bgft.c` | 1344–1466 (cascade), 1392–1416 (patch guard) |
| ptrace mmap (fixed) | `payload/src/ptrace_remote.c` | 355–368 |
| ShellUI scratch alloc | `payload/src/shellui_rpc.c` | 399, 438, 486, 765 |
| Connection read | `engine/.../connection.rs` | 394–404 |
| Polling pause (fixed) | `client/src/lib/ps5Transfers.ts` | 33–50 |
| Web UI | `engine/.../webui.rs` | spa_fallback |
