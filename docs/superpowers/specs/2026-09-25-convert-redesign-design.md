# Convert to FPKG — screen redesign

Date: 2026-09-25. Sub-project 1 of 3 (then: unified activity bar, then remote sources).

## Goal

Converting a game and getting it onto the console should be one obvious flow: drop a game,
press **Convert & install**, watch each stage, and end with a clear result and the right next
actions. No settings the user cannot reason about, and no way to act on a stale package.

What the user asked for (2026-09-25):

- Drag and drop for the game source, like Upload.
- Remember the output folder.
- No Minimum firmware field: the package should simply take the lowest firmware the game can
  run on, backport included.
- A better compression control.
- After converting, an install step; also a single **Convert & install**.
- Progress for convert and install, with the status of each stage and the result.
- A workflow with no ordering mistakes: after one game is converted and installed, starting
  the next must not leave buttons acting on the old one.
- Keep the converted `.pkg` after install, with a **Delete package** button.

## Layout

One column of three numbered cards (chosen over a two-panel layout):

1. **① Game** — a drop zone ("Drop a game folder, .exfat or .ffpkg here") with **Folder…** and
   **Image…** buttons and the path field. Once checked it shows the title, title id, size, file
   count, "Runs on FW x.xx+ (backported)" and "N checks passed", with failed checks listed.
2. **② Options** — output folder (remembered, with **Change**) and compression as three tiles:
   ⚡ Fast, ⚖️ Balanced (recommended, default), 🗜️ Smallest. Each tile shows the estimated
   package size and time *for this game on this machine*.
3. **③ Build & install** — **Convert & install** (primary) and **Convert only**. While a job
   runs, and after it ends, this card becomes the stage list and then the result (below).
   For an `.exfat` / `.ffpkg` source, **Compress to .ffpfsc** stays here as a secondary action.

The long explanation moves into a collapsed **About** section; the beta notice becomes one line.

## Card ③ states

**Running.** One line per stage: Check source, Plan package, Compress, Write package, Verify,
then for Convert & install: Send to PS5, Install on PS5. Done stages show ✅ and their time; the
active stage shows its own bar, speed, bytes (input → output for Compress) and time left; the
rest are pending. Below: overall percent, overall time left, **Cancel**. Cards ① and ② are
locked while it runs. The job also appears in the bottom bar (sub-project 2 consumes the same
stage data).

**Done.** "Installed on PS5 — *title*" (or "Package written" for Convert only), package size and
ratio to the game, convert and install durations, the package path. Actions: **Launch on PS5**
(install only), **Show in folder** (desktop), **Install again**, **Delete package**,
**＋ Convert another game**.

**Failed.** Finished stages stay ✅; the failed one shows ❌ and a plain-language reason. If the
package was written, the card says it was kept, and offers **Retry install** (no rebuild). Always
**Show details** (the engine error text).

**Next game.** A new drop, a new path, or **＋ Convert another game** clears the result and every
button bound to it, re-checks the new source, and keeps the output folder and compression. The
previous package stays on disk.

## Engine changes

### Job stages

`JobState::Running` gains an optional `stage` object shared by all jobs:
`{ id, index, count, done, total }` — `id` one of `check`, `plan`, `compress`, `write`,
`verify` for a build; `done`/`total` in bytes for the stage. The fpkg build reports it at each
phase boundary and during the byte-heavy stages; the existing overall `bytes_sent`/`total_bytes`
stay. Clients that ignore `stage` behave as today.

### Minimum firmware, automatically

`inspect` reports `min_firmware`: the highest firmware any executable needs — the SDK pair of
`eboot.bin` and each `sce_module/*.prx`, read from the module's process/module param segment and
mapped through the known SDK pair table (FW 4 pair → 4.00, and so on) — but never higher than
the `requiredSystemSoftwareVersion` the game declares. A module whose pair is unknown or
unreadable leaves the declared value in force. The build writes `min_firmware` into the packaged
`param.json` by default. `BuildRequest.firmware` (and the API's `firmware`) stays as an explicit
override for scripts; the UI field is removed.

Spider-Man 2's dump declares 10.20 while its backported modules carry the FW 4 pair: its package
will declare 4.00.

### Per-game estimates

`inspect` gains `estimates: { fast, balanced, smallest }`, each `{ bytes, seconds }`: it reads
about 60 blocks spread across the game's files, compresses each at every level, and scales the
ratio and measured throughput to the whole game and this machine's cores. It adds a few seconds
to the check and never fails the inspection (estimates are omitted on error).

### Delete package

`POST /api/fpkg/delete { path }` removes a package only if it is a `.pkg` this engine process
built (recorded when a build job finishes). Anything else is refused.

## Client changes

- `state/fpkgConversion.ts` becomes the pipeline: `idle → checking → ready → converting(stage)
  → installing(stage) → done | failed{ stage, message, packagePath? }`. The install step calls
  the existing `installStream(packagePath, host)`; its task progress supplies the Send / Install
  stages. **Retry install** re-enters `installing` with the kept `packagePath`.
- Persisted preferences (per viewer): last output folder, last compression level.
- Drag and drop on desktop via the webview drag-drop events, as in Upload, routed to Convert only
  while this screen is mounted (a dropped `.pkg` still goes to Install Package app-wide). Browser
  and Android builds keep the pickers.
- `screens/FpkgConvert/` split into the three cards plus the state card.

## Guard rails

- Nothing starts while a job is running; ① and ② are locked meanwhile.
- A new source resets the result; every result button carries the exact package path it acts on.
- No PS5 connected: **Convert only** is enabled and **Convert & install** shows "Connect to a PS5
  to install". A lost connection mid-install fails the install stage with that reason and keeps
  the package.
- **Delete package** asks for confirmation and removes the result card's install actions.

## Testing

- Rust: firmware derivation (a backported and an unbackported module; unknown pair falls back),
  the build's stage sequence, estimate sampling on a small tree, delete refusing foreign paths.
- Client: pipeline transitions, reset on a new source, retry-install reusing the package, the
  no-console state.
- On hardware: Convert & install Minecraft end to end on the FW 5.10 console.

## Out of scope

The bottom activity bar (sub-project 2) and remote SMB/FTP/SFTP sources (sub-project 3).
