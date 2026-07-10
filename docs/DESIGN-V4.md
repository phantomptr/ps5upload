# ps5upload v4.x.x — Design & Roadmap

Consolidates the codebase quality audit, the elf-arsenal feature gap
analysis, and new feature ideas into a single forward-looking plan.

Each item is tagged **P0** (must-do before v4.0), **P1** (v4.0–4.1),
or **P2** (v4.2+) so release scoping is explicit.

---

## Part 1 — Codebase Quality Audit Remediation

### 1.1 Centralize version string (P0) — ✅ Done (v3.4.0)

The version `3.3.26` is hardcoded in **6 locations** that must be
manually kept in sync on every release:

| File | Mechanism |
|------|-----------|
| `VERSION` | Single-line canonical source (read by release scripts) |
| `payload/include/config.h:8` | `#define PS5UPLOAD2_VERSION "3.3.26"` |
| `engine/Cargo.toml:16` | `version = "3.3.26"` (workspace root) |
| `client/src-tauri/Cargo.toml:3` | Desktop crate version |
| `client/package.json:4` | `"version": "3.3.26"` |
| `client/src-tauri/tauri.conf.json:4` | `"version": "3.3.26"` |

**Plan:** Add a `scripts/sync-version.sh` that reads `VERSION` and patches
all 5 secondary locations. Wire into `make release` (or the existing
release script) so it runs automatically. The C header can `#include` a
generated `.ver` file or use a `-DPS5UPLOAD2_VERSION="..."` build flag
from the Makefile.

### 1.2 Fix bare `RwLock::unwrap()` in Tauri engine bridge (P0) — ✅ Done (v3.4.0)

`client/src-tauri/src/engine.rs:675,687` use bare `.unwrap()` on a
`RwLock` read/write — panics if the lock is poisoned. The rest of the
codebase uses `.unwrap_or_else(|e| e.into_inner())` to survive poisoning
(e.g., `pkg_install.rs:109`, `lib.rs:903`).

**Plan:** Align these two sites with the established pattern.

### 1.3 Remove unused JS-side npm packages (P1) — ✅ Done (v3.4.0)

`@tauri-apps/plugin-fs` and `@tauri-apps/plugin-shell` in
`client/package.json` have zero imports in `client/src/`. Their Rust
counterparts (`tauri-plugin-fs`, `tauri-plugin-shell`) are still
initialized in `lib.rs:126-127` and must stay.

**Plan:** Drop the two JS entries from `dependencies`, run
`npm install` to update the lock file, verify build.

### 1.4 Deduplicate `DEFAULT_ENGINE_URL` (P1) — ✅ Done (v4.0.0)

`"http://127.0.0.1:19113"` is defined in both `engine.rs:21` and
`engine_mobile.rs:20`.

**Plan:** Extract to a shared `const` in a common module imported by both.

### 1.5 Prune weakest `#[allow(dead_code)]` items (P2) — ✅ Resolved

- `ufs2.rs` — module removed entirely during refactor.
- `lib.rs` — `auto_chmod_uploaded_tree` removed during lib.rs refactor (now 92 lines).
- Remaining `#[allow(dead_code)]` in `transfer.rs:537` (`ApplyProgress.total_files`) is documented as intentional (surfaced via cfg sink).

### 1.6 Add diagnostic logging to silent staging cleanups (P2) — ✅ Done (v4.0.0)

`transfer.rs:4074,4084` silently ignore `remove_dir_all` failures in
archive staging. A failure here means stale data may corrupt the next
extraction. Log the error so root cause is diagnosable.

---

## Part 2 — elf-arsenal Feature Gap Analysis

elf-arsenal is a **console-resident** payload (runs ON the PS5 with an
embedded web/FTP/DNS server). ps5upload is a **host-side** tool (React +
Tauri frontend → Rust engine → C payload on PS5). Some elf-arsenal
features map naturally to ps5upload's architecture; others don't.

### Gaps to implement (fit ps5upload's host-side role)

| ID | Feature | elf-arsenal impl | ps5upload approach | Priority |
|----|---------|------------------|--------------------|----------|
| G1 | **Backup & restore** (app.db, registry, saves) | `backup.c` — snapshots `/system_data/priv/app.db`, `/user/home/<id>/savedata/`, registry via `sceRegMgr` | New payload module + engine route + client screen. Payload reads/streams backup sets to host via existing transfer channel. | P1 |
| G2 | **Drive SMART/temp sensors** | `drive_sensors.c` — SCSI LOG SENSE via CAM pass-through | Payload module that reads `daN` temp via CAM ioctl, reports via STATUS frame. Surface in Dashboard next to CPU/SoC temps. | P1 |
| G3 | **SDK/version info per title** | `gamemeta.c` — reads `param.json` from `/user/appmeta/<id>/` | Engine already has `/api/ps5/game-meta`. Extend payload to extract `sdkVersion` + `requiredSystemSoftwareVersion` from param.json. | P2 |
| G4 | **Remote Play PIN generation** | `remoteplay.c` — ptrace into SceShellUI to call `sceRemoteplayGeneratePinCode` | Payload already has `shellui_rpc.c` ptrace infrastructure. Add a new RPC to extract the PIN and display it in the client. | P2 |
| G5 | **Offline account activation** | `offact.c` — `sceRegMgr` slot manipulation to activate PSN accounts offline | Payload module using existing `sys_registry.c` infrastructure. Client screen to select slot + activate. | P2 |
| G6 | **Cheat code manager** | `cheats.c` — zip-based cheat packs, patches memory via ptrace | Engine route to upload cheat zip, payload applies via `ptrace_remote.c`. Client screen for browse + toggle. | P2 |
| G7 | **Fan translation overlay** | `translate.c` — patches `param.json` + overlay files for i18n | Engine route to upload translation pack, payload applies overlay. Client screen for manage translations. | P2 |
| G8 | **App/game dumper** | `dumper.c` — bridges to ps5-app-dumper, dumps installed games to USB | Payload module to orchestrate dump, stream to host via transfer. Client screen for game selection + dump. | P2 |

### Gaps to skip (don't fit host-side model)

| Feature | Why skip |
|---------|----------|
| Embedded web server (`websrv.c`) | ps5upload IS the web UI — runs on host, not console |
| Embedded FTP server | Transfer protocol is purpose-built; FTP adds attack surface with no benefit |
| Embedded DNS server (`mdns.c`) | Already have a NanoDns screen; the payload-side DNS redirect is a different use case |
| Linux loader (`linux_loader.c`) | Niche feature; would require kernel-level patching that doesn't fit the management payload model |
| Kernel patch tables | ps5upload's payload is loaded post-jailbreak by kstuff/elfldr; kernel patching is a different tool's job |
| Payload autorun config | ps5upload already handles payload deployment from the host side |
| QR code connection | Tauri desktop/mobile app doesn't need QR pairing — manual IP entry works |
| Offline pack bundler | The host app IS the "offline pack" — it bundles all payloads in the Tauri binary |

---

## Part 3 — v4.x.x Feature Ideas & Architecture Improvements

### 3.1 Multi-console management (P1)

**Problem:** Current architecture is single-console — global state for
one PS5 connection. Users with multiple consoles must disconnect +
reconnect.

**Design:** Already documented in `docs/multi-console-redesign.md`.
Implement the roster-based connection manager with per-console state
isolation.

### 3.2 Save data management with resigning (P1)

**Problem:** Users can list saves but can't backup, restore, or resign
them. This is a top community request.

**Design:** New payload module (`save_mgr.c`) that:
- Lists save data per user + title (`/user/home/<uid>/savedata/<title_id>/`)
- Backs up save to host via transfer
- Restores save from host
- Resigns save to a different account (patch `savedata_param.sfo`)

Engine routes: `/api/ps5/saves/backup`, `/api/ps5/saves/restore`
Client screen: extend existing `Saves` screen with backup/restore/resign

### 3.3 Permanent fan speed setting (P1)

**Problem:** Users can set a fan threshold but it resets on reboot.
Issue #164 explicitly requests a permanent setting.

**Design:** Payload module that writes the fan threshold to registry
(via `sceRegMgrSetInt`) so it persists. Registry key:
`/system/NP.env/AutoPowerOff` (or the ICC fan config key). Client
"Hardware" screen gets a "Permanent fan threshold" toggle.

### 3.4 User ID management (P1)

**Problem:** Users can list PS5 user accounts but can't create, delete,
or rename them. Issue #164 requests user ID management.

**Design:** Payload module wrapping `sceUserService*` APIs:
- Create user (creates a new slot + avatar)
- Delete user (clears slot + saves)
- Rename user
- Set avatar

Engine routes under `/api/ps5/users/`. Client screen extends existing
Profile screen.

### 3.5 PKG install queue with priority (P2)

**Problem:** Users stage multiple PKGs and use "Install all" but can't
reorder, pause, or prioritize.

**Design:** Engine-side job queue with priority field. Client shows
draggable queue list. Pause/resume individual items.

### 3.6 Transfer throughput telemetry + auto-tuning (P2)

**Problem:** Multi-stream upload uses a fixed stream count. Optimal
count varies by network conditions and file composition.

**Design:** Engine tracks per-stream throughput in real-time. If stream
N consistently underperforms, redistribute its remaining files to other
streams. Surface a live throughput graph in the Upload screen.

### 3.7 Payload hot-reload (P2)

**Problem:** Updating the payload requires a full reconnect.

**Design:** Engine route `/api/ps5/payload/reload` that sends a new
ELF to the loader port and re-establishes the management connection
without dropping the client session.

### 3.8 Audit log search + export (P2)

**Problem:** The Audit Log screen lists events but has no search or
export. Users reporting bugs must screenshot manually.

**Design:** Add search filter (by event type, timestamp range, free
text) and a "Export as JSON/CSV" button.

### 3.9 Notification inbox viewer (P2)

**Problem:** PS5 stores system notifications in an inbox DB. No way to
view them from the host.

**Design:** Payload module reads the notification inbox SQLite DB,
streams entries to host. New "Notifications" screen.

### 3.10 Screenshot/video batch export (P2)

**Problem:** Screenshots and videos can be browsed but only downloaded
one at a time.

**Design:** Multi-select + batch download with progress bar. Auto-
organize into folders by game title + date.

---

## Part 4 — Release Scoping

### v4.0.0 — Quality + Stability
- All P0 audit items (version centralization, RwLock fix)
- All P1 audit items (unused deps, constant dedup)
- G2: Drive SMART/temp sensors (high-impact, low-risk)
- G1: Backup & restore (most-requested community feature)
- 3.2: Save data management with resigning
- 3.3: Permanent fan speed setting
- 3.4: User ID management

### v4.1.0 — Multi-console + UX
- 3.1: Multi-console management
- 3.8: Audit log search + export
- 3.10: Screenshot/video batch export
- G3: SDK/version info per title

### v4.2.0 — Advanced features
- G4: Remote Play PIN generation
- G5: Offline account activation
- G6: Cheat code manager
- G7: Fan translation overlay
- 3.5: PKG install queue with priority
- 3.6: Transfer throughput auto-tuning
- 3.7: Payload hot-reload
- 3.9: Notification inbox viewer
- G8: App/game dumper

---

## Appendix A — Issue Mapping

| Issue | v4.x.x item |
|-------|-------------|
| #164 — FW 12.40 remote operations broken | 3.3 (permanent fan), 3.4 (user ID mgmt), already fixed DPI/polling |
| #152 — PKG update files fail to install | Already fixed (authid gate, settle fraction) |
| #116 — Play-time + find unused games | Already implemented (v3.3.25) |
| #165 — Rest mode after uploads | Already implemented (v3.3.25) |
