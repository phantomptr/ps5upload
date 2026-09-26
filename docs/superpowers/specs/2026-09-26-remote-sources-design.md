# Remote sources — SMB, FTP and SFTP servers everywhere you browse

Date: 2026-09-26. Sub-project 3 of 3 (after the Convert redesign and the activity bar).

## Goal

The user asked to "get rid of SMB Browser, and FTP server, instead … support more like the client,
let user connect to those smb or ftp or ssh servers, and able to get folders and file from there …
connect to those in the home, and maybe also a view just for user to do those connection setup,
make sure we can remember those connections … for every place there is a browse folder or browse
file."

So: saved connections to the user's own servers (a NAS, a PC share, a seedbox), set up from Home
and a Connections screen, and usable as a source from every Browse… in the app. Decisions taken
while designing:

- **Sources now, destinations later** — pick games, packages and files from a server; saving to a
  server is a later project, and the design leaves room for it.
- **The engine owns remote access** — it is the only process that reads the files (uploads,
  installs, conversions), it runs the same in the desktop app, Android and the Docker web build, and
  only it can stream a 100 GB game without routing it through the app.
- **Split Browse button** — the main click stays the system dialog; a ▾ lists saved servers.

## What exists today

- **SMB Browser** (`screens/SmbBrowser`, 597 lines): a server + credentials typed each time, share
  listing, browse, download to this computer, send to the PS5, install a `.pkg`. Engine side:
  `smb.rs` (pure-Rust `smb2` crate, routes `/api/smb/list-shares|list-dir|download|transfer`) and
  `smb_range.rs` (stream-install from a share in concurrent positioned reads, no staging).
  `pkgLibrary.ts` carries an `SmbStreamSource { server, share, user, password, path }`.
- **FTP Server** (`screens/FtpServer`): starts an FTP server *on the PS5* (payload feature) for an
  outside FTP client. It does not connect to the user's servers.
- **Browse…**: ~20 screens call `lib/pickPath.ts` (`pickPath`, `pickPaths`), which opens the
  system dialog on desktop and the in-app `LocalPathPicker` on Android. Both know only local disk.

## Design

### Remote paths

A remote pick is a string `remote://<connection-id>/<path>` (e.g.
`remote://nas-1f3a/games/ps5/Minecraft.pkg`). It flows through every screen and store unchanged —
they already pass paths as strings — and only the engine interprets it. Local paths are untouched.
A client helper formats it for display as "NAS › games/ps5/Minecraft.pkg" with the protocol icon.

### Engine: the connections store

A connection: `id`, `name`, `protocol` (`smb` | `ftp` | `ftps` | `sftp`), `host`, `port` (default
per protocol: 445 / 21 / 21 / 22), `share` (SMB only), `user` (empty = guest/anonymous), secret
(password, or an SFTP private key + optional passphrase), `start_path`, and for SFTP the accepted
host-key fingerprint.

- Stored as JSON in `~/.ps5upload/connections.json`; secrets encrypted (AES-GCM) with a per-install
  key. The key lives in the OS keychain (`keyring` crate) where there is one, otherwise in
  `~/.ps5upload/connections.key` (Docker volume, Android app storage).
- API: `GET /api/remote/connections`, `POST` (add), `PUT /…/{id}` (edit; an omitted secret keeps the
  old one), `DELETE /…/{id}`, `POST /…/{id}/test` and `POST /api/remote/test` (unsaved form).
  Responses carry `has_secret: true`, never the secret. Logs never print a secret.

### Engine: one interface for all protocols

```rust
trait RemoteFs: Send + Sync {
    async fn list(&self, path: &str, cursor: Option<String>) -> Result<Page>; // ≤ 200 entries + next cursor
    async fn stat(&self, path: &str) -> Result<Entry>;                        // name, is_dir, size, mtime
    async fn open(&self, path: &str) -> Result<Box<dyn RemoteFile>>;          // positioned reads: read_at(off, len)
    async fn walk(&self, path: &str, limit: usize) -> Result<Vec<Entry>>;     // a folder tree, capped
}
```

- Implementations: SMB (moved from `smb.rs` / `smb_range.rs`), FTP and FTPS (`suppaftp`, rustls),
  SFTP (`russh` + `russh-sftp`). All pure Rust — no native libraries for Windows, Android or Docker.
- A per-connection pool keeps sessions open for browsing and streaming (idle close after 2 min).
- SMB share listing: `GET /api/remote/connections/{id}/shares`, and the share field also accepts a
  typed name, because Windows can refuse listing while allowing the share.
- Browse route: `POST /api/remote/list { path, cursor }` with `path` a `remote://` string.
- Room for destinations: write methods (`create`, `write_at`, `mkdir`, `rename`, `delete`) are added
  to the trait later; nothing in this design assumes read-only beyond not having them yet.

### Engine: path safety

- `remote://` resolves only against saved connections — an unknown id is an error, so a crafted
  path cannot point the engine at an arbitrary host.
- Paths are normalised; `..` that leaves the share (SMB) or the root is rejected.
- The new routes sit behind the same origin checks as the rest of `/api`.

### How each job consumes a remote path

| Job | How |
|---|---|
| Install a `.pkg` (stream install) | streamed in ranges through `RemoteFile::read_at` — `smb_range.rs` generalised to every protocol |
| Upload a file or folder to the PS5 | streamed: the transfer pipeline's reader takes a `RemoteFile` / walked tree instead of `std::fs` |
| Convert to FPKG / Compress to .ffpfsc | copied to a temp folder first (the builder reads out of order), after the same free-space check Convert runs today; the copy is a stage ("Copy from NAS") in the Convert card and the activity bar |
| Small inputs (payloads, cheat files, save zips, backport packs, fakelib imports) | copied to temp, then used as today |

A remote read that fails retries through the pool with backoff (3 attempts within 30 s), then the
job fails with the reason; Retry in the activity bar works, and stream installs resume by range.

### App: the picker

- `pickPath` / `pickPaths` gain `remote?: boolean` (default false) in their options. A new
  `BrowseButton` component is the split button: main click → the system dialog as today; ▾ → the
  saved servers ("NAS (SMB)", "Seedbox (SFTP)") and "Add a connection…". With `remote` false it
  renders the plain button.
- Picking a server opens the remote browser: `LocalPathPicker` generalised to list either local
  disk (Android, as today) or a connection through `/api/remote/list`, with file/folder mode, the
  caller's extension filters, paging, a breadcrumb, and the last folder remembered per connection.
- Every Browse… that reads a source switches to `BrowseButton remote`. Pickers that choose where to
  *save* (Convert output, backups, bug report, downloads) stay local-only until destinations exist.
- The field shows a remote pick as "NAS › games/ps5/Minecraft.pkg" with the protocol icon.
- The remote browser's context actions carry over SMB Browser's: **Install** (a `.pkg`) and
  **Send to PS5** (file or folder).

### App: Connections screen and Home

- **Connections** replaces SMB Browser in the navigation: each server with protocol and status
  (reachable / sign-in failed / offline), and Add, Edit, Test, Delete, Browse.
- The form: protocol tabs (SMB, FTP, FTPS, SFTP); host; port (prefilled); share (SMB, with a list
  or typed name); **Guest / anonymous** toggle, else user + password, or for SFTP a key file +
  passphrase; start folder; **Test** before **Save**. Plain FTP is labelled "unencrypted".
- SFTP host keys: trust on first use — the first connect shows the fingerprint to accept; a changed
  key blocks with a warning until the user accepts again in Edit.
- **Home** gets a **Servers** card: each connection with a Browse shortcut and an "Add a server"
  button; with none saved it is one "Connect a NAS or server" prompt.

### Errors the user sees

- Server unreachable: the server is greyed in the ▾ menu ("not reachable"); browsing says "Can't
  reach NAS (192.168.86.20:445) — check it is on and this computer is on the same network", with
  Retry and Edit.
- Sign-in failed: "Sign-in failed" + Edit; never retried automatically (NAS lockouts).
- Windows SMB errors map to plain hints: account disabled (`0xC0000072`), logon type not granted
  (`0xC000015B`), access denied (`0xC0000022`), signing required — each with what to change.
- A job whose connection was deleted fails before it starts: "The connection 'NAS' no longer
  exists."
- Not enough space for a Convert copy: refused before copying.

### What goes away

- `screens/SmbBrowser`, its route and nav entry; `/api/smb/list-shares|list-dir|download|transfer`
  (replaced by `/api/remote/*`). `SmbStreamSource` becomes a `remote://` path.
- `screens/FtpServer`, its route and nav entry. The payload's FTP server stays in the payload; the
  app no longer drives it. Removing it from the payload is a separate change.

## Build order

One plan, landing in steps that each work on their own:

1. Engine: connections store, secrets, `RemoteFs`, SMB implementation (moving `smb.rs` /
   `smb_range.rs` behind it), `/api/remote/*`.
2. App: remote browser, `BrowseButton`, Connections screen, Home card — SMB only.
3. Consumers: stream install, upload, Convert copy, small inputs.
4. FTP/FTPS and SFTP implementations — they then work everywhere.
5. Remove SMB Browser and FTP Server.

## Testing

- **Engine:** `RemoteFs` contract tests against an in-memory fake (list + paging, stat, `read_at`,
  walk, `..` confinement, unknown connection id); protocol tests against Samba, vsftpd (FTP + FTPS)
  and OpenSSH SFTP containers — opt-in like the hardware tests; secrets never in a response or a log
  line; the store survives a restart and an edit that omits the secret keeps it.
- **Consumers:** upload and stream install from `remote://` against the fake, byte-compared with the
  source; the Convert copy and its free-space refusal; a read that drops mid-stream retries, then
  fails with the reason.
- **App:** connections store tests (add, edit, test, delete); render tests for `BrowseButton` (servers
  listed, plain when `remote` is off), the remote browser (paging, filters, error states), the
  Connections screen and the Home card; SMB Browser and FTP Server gone from routes and navigation.
- **By hand:** a real share (the user's NAS or a Samba container) over the Ethernet adapter —
  stream-install one package and upload one game folder to the Phat, and compare throughput with
  today's SMB streaming (~110 MB/s ceiling).

## Out of scope

Destinations (saving to a server); a package title/icon cache for remote shares; removing the FTP
server from the payload; network discovery of servers.
