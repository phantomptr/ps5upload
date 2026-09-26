# Remote Sources Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Saved SMB / FTP / FTPS / SFTP connections that every source Browse… in the app can pick from, with the engine reading (and where possible streaming) remote files, replacing the SMB Browser and FTP Server screens.

**Architecture:** The engine owns a connections store (secrets encrypted) and a `RemoteFs` trait with one implementation per protocol. A pick is a `remote://<connection-id>/<path>` string that flows through the app unchanged; the engine resolves it — streaming for package installs (a `RemotePkg::Remote` range source) and uploads (a `SourceFs` seam in the transfer core), copying to a local folder for Convert and small inputs (a fetch job). The app gains a split `BrowseButton`, a remote browser (the generalised `LocalPathPicker`), a Connections screen and a Home card.

**Tech Stack:** Rust (axum, tokio, `smb2` 0.21, `suppaftp` 12 with `tokio-rustls-ring`, `russh` 0.63 + `russh-sftp` 3, `aes-gcm` 0.11, `keyring` 4 on macOS/Windows only, `async-trait`), React + Zustand + Vitest (node env, static-markup render tests).

**Spec:** `docs/superpowers/specs/2026-09-26-remote-sources-design.md`

## Global Constraints

- Remote path format is exactly `remote://<connection-id>/<path>`; `<path>` is `/`-separated, starts after the id's `/`, and for SMB its first segment is **not** the share (the share lives on the connection).
- Default ports: SMB 445, FTP 21, FTPS 21 (explicit TLS), SFTP 22.
- Connections file: `<data>/connections.json`; key: OS keychain (service `ps5upload`, account `connections-key`) on macOS/Windows, else `<data>/connections.key` (0600). `<data>` = `PS5UPLOAD_DATA_DIR` if set, else `$HOME` / `%USERPROFILE%` + `/.ps5upload`.
- Secrets are never in an API response (`has_secret: bool` only) or a log line; logs name the host only.
- Remote listing pages hold at most 200 entries; `cursor` is opaque to the client.
- Read retry: 3 attempts within 30 s (backoff 1 s, 4 s, 10 s), reconnecting through the pool; sign-in failures are never retried.
- Pool: sessions idle-close after 120 s.
- Pure-Rust dependencies only; the Android engine build (`cargo check -p ps5upload-engine --target aarch64-linux-android`, when the target is installed) must still compile — a protocol crate that will not cross-compile is `cfg(not(target_os = "android"))`'d with a ledger ruling.
- Only pickers that choose something to **read** get servers; save-destination pickers stay local-only.
- Client talks to `/api/remote/*` with `fetch(getEngineUrl() + …)` (the `fakelibCorpus.ts` pattern), so desktop, Android and the web build share one path.
- i18n: every new key in `client/src/i18n/locales/en.ts` (column-0 lines, never reformat) and in every locale's `missing` list in `scripts/i18n-known-missing.json`; `npm run i18n:check` from the repo root.
- Rust: `cargo fmt` after every Rust edit; `cargo clippy -p ps5upload-engine -- -D warnings` clean.
- Client gate: `cd client && npx vitest run && npx tsc --noEmit -p . && npx eslint src` — check exit codes, never grep tsc output.
- Never edit `client/src-tauri` while a console install runs (tauri dev restarts the engine).

## Review Focus

1. **A picked remote path reaches a feature that only understands local paths** (e.g. LocalImage, a save-destination) → the button there is the plain local picker, so it cannot happen; every remote-enabled site has a consumer that accepts `remote://`. Test: Task 12's call-site table test.
2. **The server goes away mid-install or mid-upload** → the read retries 3× within 30 s through the pool, then the job fails with "Lost the connection to NAS: …", and Retry works. Test in Task 5 (`read_at` retry against a flaky fake).
3. **A connection edited or deleted while a screen holds its path** → edit takes effect on the next job (pool entry dropped); delete fails the job before it starts with "The connection 'NAS' no longer exists". Test in Task 2 (pool invalidation) and Task 5.
4. **Credentials leaking** — into `GET /api/remote/connections`, error strings, the task store payload, or the bug-report bundle → never. Test in Task 2 (response + error text) and Task 12 (task payload holds the display path only).
5. **A crafted `remote://` path** (`..`, an unknown id, a share-escaping path, a URL-encoded `%2e%2e`) → rejected before any network I/O. Test in Task 1.

---

## File Structure

Engine (`engine/crates/ps5upload-engine/src/`):
- `remote/mod.rs` — `RemoteFs`, `RemoteFile`, `Entry`, `Page`, `RemoteError`, the in-memory `MemFs` fake (cfg(test) + `pub(crate)` for consumer tests).
- `remote/path.rs` — parse/format/confine `remote://` paths.
- `remote/store.rs` — connections JSON store, secrets (AES-GCM), key source.
- `remote/pool.rs` — per-connection session pool, `open_fs(id)`, retry wrapper.
- `remote/smb_fs.rs`, `remote/ftp_fs.rs`, `remote/sftp_fs.rs` — protocol implementations.
- `remote/hints.rs` — error → plain hint mapping (Windows NT status codes, TLS, host key).
- `remote/api.rs` — `/api/remote/*` handlers (connections CRUD, test, shares, list, fetch job, inspect-folder).
- `remote/range.rs` — `RemoteRangeSource` (generalised `smb_range.rs`).
- `remote/source_fs.rs` — the engine's `SourceFs` adapter over `RemoteFs` for uploads.
- Modified: `lib.rs` (mod, routes, transfer handlers), `pkg_install.rs` (`RemotePkg::Remote`, `path` = `remote://`), `pkg` parse-split handler, `Cargo.toml`.
- Removed at the end: `smb_range.rs`, the `/api/smb/*` routes and handlers in `smb.rs` (its helpers move into `remote/smb_fs.rs`).

Core (`engine/crates/ps5upload-core/src/`):
- `source_fs.rs` — `SourceFs` trait + `LocalFs`; `transfer.rs` reads through `cfg.source_fs`.

Client (`client/src/`):
- `lib/remotePath.ts` — `isRemotePath`, `parseRemotePath`, `displayRemotePath`.
- `api/remote.ts` — typed calls to `/api/remote/*`.
- `state/connections.ts` — the connections store.
- `state/localPicker.ts` (modified) — a pick request carries `source: "local" | { connectionId }`.
- `components/LocalPathPicker.tsx` (modified) — lists local or remote; paging, filters, breadcrumb, last folder per connection, error hints, context actions.
- `components/BrowseButton.tsx` — the split button; `components/PathLabel.tsx` — shows a local or remote path.
- `lib/pickPath.ts` (modified) — `remote?: boolean`, `source?` option.
- `lib/materialize.ts` — `materializeRemote(path)` for small inputs.
- `screens/Connections/{index.tsx, ConnectionForm.tsx}` — the Connections screen.
- `screens/Home/ServersCard.tsx` — the Home card.
- Modified call sites: Upload, FileSystem (add files, replace), InstallPackage (2), FpkgConvert source, Payloads SendPanel + PlaylistsPanel (2), Saves restore, Profile avatar.
- Removed: `screens/SmbBrowser`, `screens/FtpServer`, their routes, nav entries, `api/ps5.ts` smb_* / ftp_* bindings, `lib/browserInvoke.ts` cases, `client/src-tauri` smb_* commands.

---

### Task 1: Remote paths and the `RemoteFs` interface

**Files:**
- Create: `engine/crates/ps5upload-engine/src/remote/mod.rs`, `engine/crates/ps5upload-engine/src/remote/path.rs`
- Modify: `engine/crates/ps5upload-engine/src/lib.rs` (add `mod remote;` next to `mod smb;`), `engine/crates/ps5upload-engine/Cargo.toml` (`async-trait = "0.1"`)

**Interfaces:**
- Produces:

```rust
// remote/path.rs
pub struct RemotePath { pub connection_id: String, pub path: String } // path: normalised, leading '/', no '..'
pub fn is_remote(s: &str) -> bool;                       // starts with "remote://"
pub fn parse(s: &str) -> Result<RemotePath, RemoteError>; // percent-decodes, normalises, rejects escape
pub fn format(id: &str, path: &str) -> String;           // "remote://id/path"
pub fn join(base: &str, name: &str) -> Result<String, RemoteError>; // for walking; rejects '/' and '..' in name

// remote/mod.rs
#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct Entry { pub name: String, pub is_dir: bool, pub size: u64, pub mtime: Option<i64> }
#[derive(Debug, serde::Serialize)]
pub struct Page { pub entries: Vec<Entry>, pub next_cursor: Option<String> }
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("{0}")] BadPath(String),
    #[error("The connection '{0}' no longer exists")] UnknownConnection(String),
    #[error("Sign-in failed: {0}")] Auth(String),
    #[error("Can't reach {0}")] Unreachable(String),
    #[error("{0}")] NotFound(String),
    #[error("{0}")] Io(String),
}
#[async_trait::async_trait]
pub trait RemoteFile: Send + Sync {
    fn size(&self) -> u64;
    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError>;
}
#[async_trait::async_trait]
pub trait RemoteFs: Send + Sync {
    async fn list(&self, path: &str, cursor: Option<String>) -> Result<Page, RemoteError>;
    async fn stat(&self, path: &str) -> Result<Entry, RemoteError>;
    async fn open(&self, path: &str) -> Result<std::sync::Arc<dyn RemoteFile>, RemoteError>;
    /// Every regular file under `path` as (relative path, Entry), capped at `limit`.
    async fn walk(&self, path: &str, limit: usize) -> Result<Vec<(String, Entry)>, RemoteError>;
}
pub const PAGE: usize = 200;
/// Test double: an in-memory tree. `pub(crate)` so consumer tests (install, upload, fetch) use it.
pub(crate) struct MemFs { /* BTreeMap<String, Vec<u8>> of files; dirs implied */ }
impl MemFs { pub fn new(files: &[(&str, &[u8])]) -> Self; pub fn fail_next_reads(&self, n: usize); }
```

(Check `thiserror` is already a dependency with `grep thiserror engine/crates/ps5upload-engine/Cargo.toml`; if not, implement `Display` by hand instead of adding it.)

- [ ] **Step 1: Write the failing path tests** in `remote/path.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_and_normalises() {
        let p = parse("remote://nas-1/games//ps5/./Minecraft.pkg").unwrap();
        assert_eq!(p.connection_id, "nas-1");
        assert_eq!(p.path, "/games/ps5/Minecraft.pkg");
        assert_eq!(parse("remote://nas-1").unwrap().path, "/");
    }
    #[test]
    fn rejects_every_escape() {
        for bad in [
            "remote://nas-1/../etc",
            "remote://nas-1/games/../../x",
            "remote://nas-1/%2e%2e/x",
            "remote:///games",          // empty id
            "remote://a b/x",           // id must be [A-Za-z0-9_-]
            "smb://nas/share",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(join("/games", "../x").is_err());
        assert!(join("/games", "a/b").is_err());
    }
    #[test]
    fn formats_round_trip() {
        assert_eq!(format("nas-1", "/games/a b.pkg"), "remote://nas-1/games/a b.pkg");
        let s = format("nas-1", "/games/a b.pkg");
        assert_eq!(parse(&s).unwrap().path, "/games/a b.pkg");
    }
}
```

- [ ] **Step 2: Write the failing `MemFs` contract tests** in `remote/mod.rs` (these become the contract every implementation is held to):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn memfs_lists_pages_stats_reads_and_walks() {
        let many: Vec<(String, Vec<u8>)> = (0..450).map(|i| (format!("/big/f{i:03}"), vec![1])).collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let mut files = refs.clone();
        files.push(("/g/sce_sys/param.json", b"{}"));
        files.push(("/g/eboot.bin", b"0123456789"));
        let fs = MemFs::new(&files);
        let p1 = fs.list("/big", None).await.unwrap();
        assert_eq!(p1.entries.len(), PAGE);
        let p3 = fs.list("/big", fs.list("/big", p1.next_cursor).await.unwrap().next_cursor).await.unwrap();
        assert_eq!(p3.entries.len(), 50);
        assert!(p3.next_cursor.is_none());
        assert_eq!(fs.stat("/g").await.unwrap().is_dir, true);
        let f = fs.open("/g/eboot.bin").await.unwrap();
        assert_eq!(f.size(), 10);
        assert_eq!(f.read_at(3, 4).await.unwrap(), b"3456");
        let walked = fs.walk("/g", 100).await.unwrap();
        let names: Vec<_> = walked.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, ["eboot.bin", "sce_sys/param.json"]);
        assert!(matches!(fs.stat("/nope").await, Err(RemoteError::NotFound(_))));
    }
    #[tokio::test]
    async fn memfs_can_fail_reads_for_retry_tests() {
        let fs = MemFs::new(&[("/a", b"xy")]);
        fs.fail_next_reads(1);
        let f = fs.open("/a").await.unwrap();
        assert!(f.read_at(0, 2).await.is_err());
        assert_eq!(f.read_at(0, 2).await.unwrap(), b"xy");
    }
}
```

- [ ] **Step 3: Run** `cd engine && cargo test -p ps5upload-engine remote::` — Expected: FAIL (module missing).
- [ ] **Step 4: Implement** `path.rs` (split on `/`, percent-decode each segment with a small decoder, drop empty and `.`, reject `..` and segments containing `/` or `\` after decoding; id regex `^[A-Za-z0-9_-]{1,64}$`) and `mod.rs` (types above; `MemFs` over `std::sync::Mutex<BTreeMap<String, Vec<u8>>>` plus an `AtomicUsize` fail counter; cursors are the decimal index of the next entry; entries sorted by name, dirs derived from file prefixes; `walk` returns paths relative to `path`, sorted).
- [ ] **Step 5: Run** the tests — Expected: PASS. Then `cargo fmt`, `cargo clippy -p ps5upload-engine -- -D warnings`.
- [ ] **Step 6: Commit** `feat(remote): remote paths and the RemoteFs interface`.

---

### Task 2: Connections store, secrets and the CRUD API

**Files:**
- Create: `remote/store.rs`, `remote/pool.rs`, `remote/api.rs`
- Modify: `lib.rs` (routes; `AppState` gains `remote: Arc<remote::store::Store>` and `pool: Arc<remote::pool::Pool>` — follow how existing shared state fields are added), `Cargo.toml` (`aes-gcm = "0.11"`, `getrandom` (already present? check), and `[target.'cfg(any(target_os = "macos", target_os = "windows"))'.dependencies] keyring = { version = "4", features = [...] }` — run `cargo info keyring` and pick the apple/windows native store features)

**Interfaces:**
- Consumes: Task 1 types.
- Produces:

```rust
// store.rs
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol { Smb, Ftp, Ftps, Sftp }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Connection {
    pub id: String, pub name: String, pub protocol: Protocol, pub host: String, pub port: u16,
    #[serde(default)] pub share: String,           // SMB only
    #[serde(default)] pub user: String,            // empty = guest / anonymous
    #[serde(default)] pub start_path: String,
    #[serde(default)] pub host_key: Option<String>, // SFTP accepted fingerprint (SHA256:…)
}
pub enum Secret { None, Password(String), Key { pem: String, passphrase: Option<String> } }
pub struct ConnectionView { #[serde(flatten)] pub conn: Connection, pub has_secret: bool } // Serialize
impl Store {
    pub fn open(dir: &Path) -> anyhow::Result<Self>;          // loads connections.json, obtains key
    pub fn list(&self) -> Vec<ConnectionView>;
    pub fn get(&self, id: &str) -> Option<(Connection, Secret)>;
    pub fn add(&self, conn: Connection, secret: Secret) -> anyhow::Result<ConnectionView>; // assigns id
    pub fn update(&self, id: &str, conn: Connection, secret: Option<Secret>) -> anyhow::Result<ConnectionView>; // None keeps the old secret
    pub fn remove(&self, id: &str) -> anyhow::Result<()>;
    pub fn set_host_key(&self, id: &str, fp: &str) -> anyhow::Result<()>;
}
pub fn data_dir() -> Option<PathBuf>; // PS5UPLOAD_DATA_DIR, else HOME/USERPROFILE + ".ps5upload"
pub fn default_port(p: &Protocol) -> u16;

// pool.rs
pub struct Pool { /* id -> (Arc<dyn RemoteFs>, last_used) */ }
impl Pool {
    pub async fn fs(&self, store: &Store, id: &str) -> Result<Arc<dyn RemoteFs>, RemoteError>; // connects on miss
    pub fn invalidate(&self, id: &str);                  // on edit/delete
    pub fn connector(&self) -> &dyn Connector;           // test seam
}
#[async_trait] pub trait Connector: Send + Sync {
    async fn connect(&self, conn: &Connection, secret: &Secret) -> Result<Arc<dyn RemoteFs>, RemoteError>;
}
/// Delays between read attempts. `standard()` = [1 s, 4 s, 10 s]; `instant()` = zeros, for tests.
#[derive(Clone)] pub struct Backoff(pub Vec<std::time::Duration>);
impl Backoff { pub fn standard() -> Self; pub fn instant() -> Self; }
/// Wraps a file so reads retry once per Backoff delay (3 attempts within 30 s), reopening via the pool.
pub fn retrying(pool: Arc<Pool>, store: Arc<Store>, id: String, path: String, file: Arc<dyn RemoteFile>, backoff: Backoff) -> Arc<dyn RemoteFile>;
```

- API (in `api.rs`, JSON, errors via `json_err`):
  - `GET /api/remote/connections` → `{ connections: ConnectionView[] }`
  - `POST /api/remote/connections` `{ connection, password?, key_pem?, key_passphrase? }` → `ConnectionView`
  - `PUT /api/remote/connections/{id}` same body; secret fields omitted = keep
  - `DELETE /api/remote/connections/{id}`
  - `POST /api/remote/connections/{id}/test` → `{ ok: true } | { ok: false, error, hint? }`
  - `POST /api/remote/test` (unsaved form, same body as POST) → same shape

- [ ] **Step 1: Failing store tests** (`store.rs`, using `tempfile::tempdir()` — check it is a dev-dependency; the file key path is used because tests set no keychain):

```rust
#[test]
fn stores_connections_and_keeps_secrets_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open_with_file_key(dir.path()).unwrap();
    let v = s.add(conn("NAS", Protocol::Smb), Secret::Password("hunter2".into())).unwrap();
    assert!(v.has_secret);
    let raw = std::fs::read_to_string(dir.path().join("connections.json")).unwrap();
    assert!(!raw.contains("hunter2"), "secret must not be stored in the clear");
    let s2 = Store::open_with_file_key(dir.path()).unwrap();
    assert!(matches!(s2.get(&v.conn.id).unwrap().1, Secret::Password(p) if p == "hunter2"));
}
#[test]
fn an_edit_without_a_secret_keeps_the_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open_with_file_key(dir.path()).unwrap();
    let v = s.add(conn("NAS", Protocol::Smb), Secret::Password("pw".into())).unwrap();
    let mut c = v.conn.clone(); c.name = "Home NAS".into();
    s.update(&c.id, c.clone(), None).unwrap();
    assert!(matches!(s.get(&c.id).unwrap().1, Secret::Password(p) if p == "pw"));
    assert_eq!(s.list()[0].conn.name, "Home NAS");
}
#[cfg(unix)]
#[test]
fn the_key_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    Store::open_with_file_key(dir.path()).unwrap();
    let mode = std::fs::metadata(dir.path().join("connections.key")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
```

- [ ] **Step 2: Failing API tests** (`api.rs`, driving the handlers through an axum `Router` built with a `FakeConnector` that returns `MemFs`, the same way other engine route tests build a router — grep `tower::ServiceExt` in the engine for the existing pattern):

```rust
#[tokio::test]
async fn listing_never_returns_a_secret() {
    let app = test_router();
    let body = post_json(&app, "/api/remote/connections",
        json!({"connection": {"name":"NAS","protocol":"smb","host":"10.0.0.5","port":445,"share":"games"},
               "password":"hunter2"})).await;
    assert_eq!(body["has_secret"], true);
    let list = get_json(&app, "/api/remote/connections").await.to_string();
    assert!(!list.contains("hunter2"));
}
#[tokio::test]
async fn a_failed_test_never_echoes_the_password() {
    let app = test_router_with(FakeConnector::failing_auth());
    let r = post_json(&app, "/api/remote/test",
        json!({"connection":{"name":"x","protocol":"ftp","host":"h","port":21},"password":"hunter2"})).await;
    assert_eq!(r["ok"], false);
    assert!(!r.to_string().contains("hunter2"));
}
#[tokio::test]
async fn editing_or_deleting_drops_the_pooled_session() {
    let (app, pool) = test_router_and_pool();
    let id = add_nas(&app).await;
    pool.fs(&store_of(&app), &id).await.unwrap();
    assert_eq!(pool.connects(), 1);
    put_json(&app, &format!("/api/remote/connections/{id}"), json!({"connection": {"name":"NAS2","protocol":"smb","host":"10.0.0.5","port":445,"share":"games"}})).await;
    pool.fs(&store_of(&app), &id).await.unwrap();
    assert_eq!(pool.connects(), 2);
    delete(&app, &format!("/api/remote/connections/{id}")).await;
    assert!(matches!(pool.fs(&store_of(&app), &id).await, Err(RemoteError::UnknownConnection(_))));
}
```

- [ ] **Step 3: Run** `cargo test -p ps5upload-engine remote::` — Expected: FAIL.
- [ ] **Step 4: Implement.** Store: JSON `{ version: 1, connections: [{ ..Connection, secret: "<base64 nonce||ciphertext>" | null }] }`; AES-256-GCM with a random 12-byte nonce per secret; the secret plaintext is JSON of `Secret`. Key: `Store::open` tries `keyring` (macOS/Windows cfg) and falls back to `connections.key` (32 random bytes, written with 0600 on unix); `open_with_file_key` forces the file (tests, Docker, Android). Writes go to `connections.json.tmp` then rename. Ids: `format!("{}-{:04x}", slug(name), rand u16)`. Pool: `tokio::sync::Mutex<HashMap>`; a background sweep (spawned on first use) drops entries idle > 120 s. `retrying`: on `RemoteError::Io`/`Unreachable`, sleep 1/4/10 s, `invalidate`, reopen via `pool.fs(...)?.open(path)`, retry; `Auth` returns at once. The real `Connector` dispatches on protocol (SMB in Task 3, FTP Task 14, SFTP Task 15; until then it returns `RemoteError::Io("<protocol> is not available yet")`).
- [ ] **Step 5: Run** — Expected: PASS; `cargo fmt`; clippy clean.
- [ ] **Step 6: Commit** `feat(remote): saved connections with encrypted secrets`.

---

### Task 3: SMB through `RemoteFs`, listing, shares and hints

**Files:**
- Create: `remote/smb_fs.rs`, `remote/hints.rs`
- Modify: `remote/api.rs` (routes `GET /api/remote/connections/{id}/shares`, `POST /api/remote/shares` for an unsaved form, `POST /api/remote/list`), `remote/pool.rs` (real connector: SMB), `smb.rs` (move `normalize_smb_server` into `smb_fs.rs`, keep a re-export until Task 16)

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces: `SmbFs::connect(conn, secret) -> Result<SmbFs, RemoteError>` implementing `RemoteFs` (paths are relative to `conn.share`); `hints::hint_for(err: &str) -> Option<&'static str>`; `POST /api/remote/list { path, cursor? } -> Page` (plus `{ error, hint? }` on failure); `GET …/shares -> { shares: [{ name, comment }] }`.

- [ ] **Step 1: Failing hint tests** (`hints.rs`):

```rust
#[test]
fn windows_status_codes_become_plain_hints() {
    assert!(hint_for("STATUS_ACCOUNT_DISABLED (0xC0000072)").unwrap().contains("Guest account"));
    assert!(hint_for("0xc000015b").unwrap().contains("network logon"));
    assert!(hint_for("access denied 0xC0000022").unwrap().contains("permissions"));
    assert!(hint_for("server requires signing").unwrap().contains("signing"));
    assert!(hint_for("connection refused").unwrap().contains("same network"));
    assert_eq!(hint_for("something else"), None);
}
```

Hint texts (exact):
- `0xC0000072` → "The share's Guest account is disabled. Enable it on the server, or sign in with a user and password."
- `0xC000015B` → "The server does not allow this account to sign in over the network. Check its network logon policy."
- `0xC0000022` → "Access denied. Check the share's permissions and the folder's security permissions for this user."
- `signing` → "The server requires SMB signing, which a guest cannot use. Sign in with a user and password."
- `refused|timed out|unreachable|no route` → "Check the server is on and this computer is on the same network."
- `certificate|tls` → "The server's TLS certificate was not accepted."

- [ ] **Step 2: Failing SMB protocol test**, opt-in (`#[ignore]`, run with `PS5UPLOAD_SAMBA_TEST=host:port` and `-- --ignored`), in `smb_fs.rs`:

```rust
#[tokio::test]
#[ignore = "needs a Samba server: docker run -p 1445:445 dperson/samba -s 'games;/share;yes;no;yes' and PS5UPLOAD_SAMBA_TEST=127.0.0.1:1445"]
async fn smb_meets_the_remote_contract() {
    let Ok(addr) = std::env::var("PS5UPLOAD_SAMBA_TEST") else { return };
    let (host, port) = addr.split_once(':').unwrap();
    let fs = SmbFs::connect(&test_conn(host, port.parse().unwrap(), "games"), &Secret::None).await.unwrap();
    crate::remote::contract::check(&fs, "/").await; // shared contract, below
}
```

and move Task 1's MemFs assertions into `remote/contract.rs` as `pub(crate) async fn check(fs: &dyn RemoteFs, root: &str)` that writes nothing and asserts listing/stat/read semantics on whatever tree exists (non-empty list, stat of each listed file matches its size, `read_at(0, min(size,16))` returns that many bytes, `..` rejected before I/O). MemFs's test calls it too.

- [ ] **Step 3: Failing list-route test** with the `FakeConnector`/`MemFs`: `POST /api/remote/list {"path":"remote://<id>/g"}` returns the two entries; `{"path":"remote://nope/g"}` returns 404 with "no longer exists"; a connector error `"… 0xC0000072"` returns `{ error, hint }` with the Guest hint.
- [ ] **Step 4: Run** — Expected: FAIL.
- [ ] **Step 5: Implement.** `SmbFs` holds an `smb2` client + tree behind a `tokio::sync::Mutex` (one session, serialised metadata ops); `open` uses `open_file_reader` and returns a `RemoteFile` whose `read_at` calls the reader directly (reads can run concurrently, as `smb_range.rs` does). `list` reads the whole directory once per call and pages it in memory (cursor = index); `walk` is iterative with the cap. Map `smb2` errors: auth-ish → `Auth`, connect errors → `Unreachable`, "not found"/`0xC0000034` → `NotFound`, else `Io`. Guest = empty user and password.
- [ ] **Step 6: Run** the unit tests — PASS; if Docker is available, run the ignored test and record the result in the ledger.
- [ ] **Step 7: Commit** `feat(remote): SMB connections, listing and plain-language errors`.

---

### Task 4: The app's connections store and API client

**Files:**
- Create: `client/src/lib/remotePath.ts`, `client/src/lib/remotePath.test.ts`, `client/src/api/remote.ts`, `client/src/state/connections.ts`, `client/src/state/connections.test.ts`

**Interfaces:**
- Produces:

```ts
// lib/remotePath.ts
export function isRemotePath(p: string | null | undefined): boolean;         // startsWith("remote://")
export function parseRemotePath(p: string): { connectionId: string; path: string } | null;
export function remotePath(connectionId: string, path: string): string;
/** "NAS › games/ps5/Minecraft.pkg"; a local path is returned unchanged. */
export function displayPath(p: string, nameOf: (id: string) => string | undefined): string;

// api/remote.ts
export type Protocol = "smb" | "ftp" | "ftps" | "sftp";
export interface Connection { id: string; name: string; protocol: Protocol; host: string; port: number;
  share: string; user: string; start_path: string; host_key: string | null; has_secret: boolean }
export interface ConnectionInput { name: string; protocol: Protocol; host: string; port: number;
  share?: string; user?: string; start_path?: string }
export interface SecretInput { password?: string; key_pem?: string; key_passphrase?: string }
export interface RemoteEntry { name: string; is_dir: boolean; size: number; mtime: number | null }
export interface RemoteFailure { error: string; hint?: string; host_key?: string }
export const remoteApi: {
  list(): Promise<Connection[]>;
  add(c: ConnectionInput, s: SecretInput): Promise<Connection>;
  update(id: string, c: ConnectionInput, s?: SecretInput): Promise<Connection>;
  remove(id: string): Promise<void>;
  test(idOrForm: string | { connection: ConnectionInput } & SecretInput): Promise<{ ok: true } | ({ ok: false } & RemoteFailure)>;
  shares(idOrForm: string | ({ connection: ConnectionInput } & SecretInput)): Promise<{ name: string; comment: string }[]>;
  listDir(path: string, cursor?: string): Promise<{ entries: RemoteEntry[]; next_cursor: string | null }>; // throws RemoteApiError
  fetch(path: string, destDir?: string): Promise<{ job_id: string }>;
  cleanupFetched(dest: string): Promise<void>;
  acceptHostKey(id: string, fingerprint: string): Promise<void>;
};
export class RemoteApiError extends Error { hint?: string; hostKey?: string }

// state/connections.ts
export type ConnStatus = "unknown" | "reachable" | "auth-failed" | "offline";
export interface ConnectionsState {
  connections: Connection[]; loaded: boolean; status: Record<string, ConnStatus>;
  load(): Promise<void>; save(id: string | null, c: ConnectionInput, s?: SecretInput): Promise<Connection>;
  remove(id: string): Promise<void>; check(id: string): Promise<ConnStatus>;
  nameOf(id: string): string | undefined;
}
export const useConnectionsStore: UseBoundStore<StoreApi<ConnectionsState>>;
```

- [ ] **Step 1: Failing tests.** `remotePath.test.ts`:

```ts
it("recognises, parses and displays remote paths", () => {
  expect(isRemotePath("remote://nas-1/games/a.pkg")).toBe(true);
  expect(isRemotePath("/Users/me/a.pkg")).toBe(false);
  expect(parseRemotePath("remote://nas-1/games/a.pkg")).toEqual({ connectionId: "nas-1", path: "/games/a.pkg" });
  expect(remotePath("nas-1", "/games/a b.pkg")).toBe("remote://nas-1/games/a b.pkg");
  expect(displayPath("remote://nas-1/games/a.pkg", () => "NAS")).toBe("NAS › games/a.pkg");
  expect(displayPath("remote://gone/x", () => undefined)).toBe("gone › x");
  expect(displayPath("/Users/me/a.pkg", () => "NAS")).toBe("/Users/me/a.pkg");
});
```

`connections.test.ts` (mock `../api/remote`):

```ts
it("loads, saves and maps check results to a status", async () => {
  list.mockResolvedValue([nas]);
  await useConnectionsStore.getState().load();
  expect(useConnectionsStore.getState().nameOf("nas-1")).toBe("NAS");
  test.mockResolvedValueOnce({ ok: false, error: "Sign-in failed: bad password" });
  expect(await useConnectionsStore.getState().check("nas-1")).toBe("auth-failed");
  test.mockResolvedValueOnce({ ok: false, error: "Can't reach 10.0.0.5" });
  expect(await useConnectionsStore.getState().check("nas-1")).toBe("offline");
  test.mockResolvedValueOnce({ ok: true });
  expect(await useConnectionsStore.getState().check("nas-1")).toBe("reachable");
});
it("never keeps a typed password in the store", async () => {
  add.mockResolvedValue({ ...nas, id: "n2" });
  await useConnectionsStore.getState().save(null, input, { password: "hunter2" });
  expect(JSON.stringify(useConnectionsStore.getState())).not.toContain("hunter2");
});
```

- [ ] **Step 2: Run** `cd client && npx vitest run src/lib/remotePath.test.ts src/state/connections.test.ts` — FAIL.
- [ ] **Step 3: Implement** (`api/remote.ts` with `fetch(\`${getEngineUrl()}/api/remote/…\`)`, JSON bodies, non-2xx → `RemoteApiError` carrying `hint`/`host_key` from the body; status mapping: `/sign-in failed/i` → auth-failed, `/can't reach|unreachable|refused|timed out/i` → offline).
- [ ] **Step 4: Run** — PASS; `npx tsc --noEmit -p .` exit 0.
- [ ] **Step 5: Commit** `feat(remote): the app's connections store`.

---

### Task 5: Install packages straight from a server

**Files:**
- Create: `remote/range.rs`
- Modify: `pkg_install.rs` (`RemotePkg::Remote(remote::range::RemoteRangeSource)`, and in the install-start resolver: a `path` that `remote::path::is_remote` → `resolve_remote_fs_source`), the `/api/pkg/parse-split` handler (a `remote://` path reads the header through `RemoteFile`), `lib.rs` (share `store`/`pool` with the resolver — pass `AppState` pieces the resolver already receives, or a `OnceLock` handle set at startup; pick the one matching how `pkg_install` reaches shared state today and ledger it)

**Interfaces:**
- Consumes: Tasks 1–3 (`Pool::fs`, `retrying`, `RemoteFile`).
- Produces: `RemoteRangeSource::open(pool, store, remote_path: &str, backoff: Backoff) -> Result<Self, RemoteError>` with `total_size()`, `host()`, `read_range(start, end) -> io::Result<Vec<u8>>` (blocking, called from the pkg-host producer thread — same contract as `SmbRangeSource::read_range`), `origin_rate_bps()`. Install-start accepts `path: "remote://…"`; the existing `smb` request field keeps working until Task 16.

- [ ] **Step 1: Failing tests** in `range.rs` (a `MemFs` behind a test `Pool`):

```rust
#[tokio::test(flavor = "multi_thread")]
async fn serves_ranges_byte_for_byte() {
    let data: Vec<u8> = (0..(9 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();
    let (pool, store, id) = pool_with(MemFs::new(&[("/g/a.pkg", &data)]));
    let src = RemoteRangeSource::open(pool, store, &format!("remote://{id}/g/a.pkg"), Backoff::instant()).await.unwrap();
    let got = tokio::task::spawn_blocking(move || src.read_range(1000, 5 * 1024 * 1024)).await.unwrap().unwrap();
    assert_eq!(got, data[1000..=5 * 1024 * 1024]);
}
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_read_is_retried() {
    let fs = MemFs::new(&[("/a.pkg", &[7u8; 4096])]);
    fs.fail_next_reads(2);
    let (pool, store, id) = pool_with(fs);
    let src = RemoteRangeSource::open(pool, store, &format!("remote://{id}/a.pkg"), Backoff::instant()).await.unwrap();
    let got = tokio::task::spawn_blocking(move || src.read_range(0, 4095)).await.unwrap().unwrap();
    assert_eq!(got.len(), 4096);
}
#[tokio::test]
async fn a_deleted_connection_fails_before_anything_starts() {
    let (pool, store, _id) = pool_with(MemFs::new(&[]));
    let e = RemoteRangeSource::open(pool, store, "remote://gone-0000/a.pkg", Backoff::instant()).await.unwrap_err();
    assert!(e.to_string().contains("no longer exists"));
}
```

(`RemoteRangeSource::open` takes a `Backoff`; production passes `Backoff::standard()`, these tests `Backoff::instant()`, so the retry test stays fast.)

- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement** by generalising `smb_range.rs`: same `PIECE` (4 MiB) / `PIECES_IN_FLIGHT` (4) positioned reads, over `Arc<dyn RemoteFile>` wrapped in `retrying`; short reads are errors. Add the `RemotePkg::Remote` arms to `read_range` / `origin_rate_bps` (no prefetch, like SMB). In `pkg_install.rs` add `resolve_remote_fs_source` modelled on `resolve_smb_source` (reject combination with `split_root`/`remote_url`/`smb`/`local_ps5_path`; probe the header with `ps5upload_pkg::metadata_from_reader` in `spawn_blocking`; log `host` only). Parse-split: when the path is remote, open the file and run the same header parse over `read_at`.
- [ ] **Step 4: Run** the new tests and `cargo test -p ps5upload-engine pkg_install` — PASS; fmt; clippy; Android check.
- [ ] **Step 5: Commit** `feat(remote): install packages straight from a server`.

---

### Task 6: Upload to the PS5 straight from a server

**Files:**
- Create: `engine/crates/ps5upload-core/src/source_fs.rs`, `remote/source_fs.rs`
- Modify: `ps5upload-core/src/lib.rs` (`pub mod source_fs;`), `ps5upload-core/src/transfer.rs` (`TransferConfig.source_fs: Option<Arc<dyn SourceFs>>`; the non-archive read sites at ~1331, ~1383, `collect_files` ~1571, ~1721, ~1767, ~1892, ~2196, ~2621 go through `cfg.fs()`), `lib.rs` (`transfer_file_handler` / `transfer_dir_handler`: a `remote://` `src`/`src_dir` sets `cfg.source_fs` to the remote adapter)

**Interfaces:**
- Produces:

```rust
// ps5upload-core/src/source_fs.rs
pub trait ReadSeek: std::io::Read + std::io::Seek + Send {}
impl<T: std::io::Read + std::io::Seek + Send> ReadSeek for T {}
pub struct SourceMeta { pub len: u64, pub is_dir: bool, pub is_file: bool }
pub trait SourceFs: Send + Sync {
    fn open(&self, p: &Path) -> std::io::Result<Box<dyn ReadSeek>>;
    fn metadata(&self, p: &Path) -> std::io::Result<SourceMeta>;
    fn read_dir(&self, p: &Path) -> std::io::Result<Vec<(PathBuf, bool /*is_dir*/)>>;
}
pub struct LocalFs; // std::fs
impl TransferConfig { pub fn fs(&self) -> &dyn SourceFs } // source_fs or &LocalFs
```

```rust
// engine remote/source_fs.rs
/// Blocking adapter: paths are the remote path string as a PathBuf ("/games/x"); reads go
/// through RemoteFile::read_at with an 8 MiB read-ahead buffer, blocking on the runtime handle
/// (the transfer runs in spawn_blocking, never on a runtime worker).
pub struct RemoteSourceFs { /* handle, fs: Arc<dyn RemoteFs>, pool/store/id for retrying */ }
impl RemoteSourceFs { pub async fn new(pool: Arc<Pool>, store: Arc<Store>, connection_id: &str) -> Result<Self, RemoteError> }
```

- [ ] **Step 1: Failing core test** (`transfer.rs` tests or `source_fs.rs`): a `MemSourceFs` (HashMap-backed `SourceFs` in the test module) given to `collect_files_with(&fs, dir)` and to the non-packed shard reader returns the same bytes and file list as the local path for an identical tree written to a tempdir. Name: `a_source_fs_reads_exactly_what_local_disk_would`.
- [ ] **Step 2: Failing engine test** (`remote/source_fs.rs`): `RemoteSourceFs` over `MemFs` — `metadata`, `read_dir`, and `open` + `seek(SeekFrom::Start(3))` + `read_to_end` match the MemFs bytes; a 20 MiB file read in 1 MiB `read` calls equals the source (exercises the read-ahead buffer boundaries).
- [ ] **Step 3: Run** `cargo test -p ps5upload-core source_fs` and `cargo test -p ps5upload-engine remote::source_fs` — FAIL.
- [ ] **Step 4: Implement.** In core, replace only the listed non-archive sites (`std::fs::File::open(p)` → `cfg.fs().open(p)`, `std::fs::metadata` → `cfg.fs().metadata`, `collect_files(dir)` → `collect_files_with(cfg.fs(), dir)` keeping `collect_files` as a local wrapper); archive (`zip`/`rar`/`7z`) paths stay local — a remote archive upload is copied first by the fetch job (Task 7) — ledger it. Functions that do not have `cfg` in scope get it threaded through as a parameter. In the engine handlers, `remote://` + archive extension returns 400 "Pick the archive after copying it — archives upload from this computer" only if Task 7's client flow does not already copy them (it does; this is a guard).
- [ ] **Step 5: Run** `cargo test -p ps5upload-core` and `cargo test -p ps5upload-engine` — PASS (whole crates: the core change touches every upload); fmt; clippy.
- [ ] **Step 6: Commit** `feat(remote): upload to the PS5 straight from a server`.

---

### Task 7: The fetch job and remote folder inspection

**Files:**
- Modify: `remote/api.rs` (`POST /api/remote/fetch`, `POST /api/remote/inspect-folder`), `lib.rs` (routes)

**Interfaces:**
- Produces:
  - `POST /api/remote/fetch { path, dest_dir? } -> { job_id }` — copies a remote file or folder to `dest_dir` (default: `<engine temp>/ps5upload-remote/<uuid>/`) as `<dest_dir>/<basename>`, as a job (bytes progress, cancel via `/api/jobs/{id}/cancel`, `dest` = the local path on done); refuses with 507 "Not enough space: needs X, Y free" before copying when the destination volume lacks the total size + 1 GiB (use the free-space helper Convert's estimate already uses — grep `available_space`/`free_space` in `fpkg_api.rs`).
  - `POST /api/remote/fetch/cleanup { dest }` — deletes a copy this engine's fetch jobs made (the engine keeps the set of fetch destinations; any other path is refused with 400), so Convert can remove its copied source.
  - `POST /api/remote/inspect-folder { path } -> same JSON as /api/local/inspect-folder` — copies only `sce_sys/param.json`, `sce_sys/param.sfo`, `sce_sys/icon0.png` (those that exist) into a temp mirror and runs the local folder inspection on the mirror; the result's paths are rewritten to the remote path.

- [ ] **Step 1: Failing tests** (router + `MemFs`): fetch of `/g` (two files) ends `done` with `dest` whose files byte-match; cleanup of that `dest` removes it, and cleanup of any other path (e.g. the user's home) returns 400 and deletes nothing; fetch into a dir on a fake 1 KiB-free volume (inject the free-space function) returns 507 before creating anything; inspect-folder of a tree with `sce_sys/param.json` `{"titleId":"PPSA01234",...}` returns `meta_source != "none"` and the title id; a cancelled fetch leaves no partial file behind.
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement** with the job pattern of `ffpfsc_compress_handler` (`fpkg_api.rs`: `register_transfer_cancel`, `set_job` Running/Done/Failed, a ticker over an `AtomicU64`); copy files with 8 MiB `read_at` chunks to `<name>.partial` then rename; on cancel or error remove the partials.
- [ ] **Step 4: Run** — PASS; fmt; clippy.
- [ ] **Step 5: Commit** `feat(remote): copy from a server as a job, and inspect remote game folders`.

---

### Task 8: The remote browser

**Files:**
- Modify: `client/src/state/localPicker.ts`, `client/src/components/LocalPathPicker.tsx`
- Create: `client/src/components/LocalPathPicker.test.tsx`

**Interfaces:**
- Consumes: Task 4 (`remoteApi.listDir`, `RemoteApiError`, `useConnectionsStore`, `remotePath`).
- Produces: `pickLocalPath(opts: { mode: "file" | "folder"; title?: string; filters?: { name: string; extensions: string[] }[]; source?: "local" | { connectionId: string } })` (unchanged default: local). For a remote source the picker lists via `remoteApi.listDir`, starts at the last folder used for that connection (localStorage key `ps5upload.remoteLastDir.<id>`, try/catch) or its `start_path`, shows a breadcrumb, a "Load more" row while `next_cursor` is set, filters files by extension (folders always shown), and resolves with `remotePath(id, chosen)`. Errors render the message, the hint, and Retry / Edit connection (Edit navigates to `/connections?edit=<id>`). Context actions per file row: **Install** for `.pkg` (calls an injected `onInstall(path)`), **Send to PS5** (injected `onSend(path)`) — shown only when the picker was opened from the Connections screen's Browse (`opts.actions`).

- [ ] **Step 1: Failing render tests** (static markup; mock `useTr` as in `RunCard.test.tsx`; render the view part — split `LocalPathPicker` into a pure `PickerView` taking `{ entries, cwd, crumbs, loading, error, hint, hasMore, mode, filters, actions }` so it can be rendered without effects):

```tsx
it("filters files by extension but always shows folders", () => {
  const out = html({ entries: [dir("ps5"), file("a.pkg"), file("notes.txt")], mode: "file", filters: [{ name: "PKG", extensions: ["pkg"] }] });
  expect(out).toContain("ps5"); expect(out).toContain("a.pkg"); expect(out).not.toContain("notes.txt");
});
it("offers Load more while the server has more", () => {
  expect(html({ entries: [file("a.pkg")], hasMore: true })).toContain("Load more");
});
it("shows the hint and a way to fix the connection", () => {
  const out = html({ error: "Sign-in failed: …", hint: "The share's Guest account is disabled." });
  expect(out).toContain("Guest account is disabled"); expect(out).toContain("Edit connection");
});
it("offers Install on a .pkg only when opened for browsing", () => {
  expect(html({ entries: [file("a.pkg")], actions: true })).toContain("Install");
  expect(html({ entries: [file("a.pkg")] })).not.toContain(">Install<");
});
```

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS; tsc; eslint.
- [ ] **Step 5: Commit** `feat(remote): browse a server in the in-app picker`.

---

### Task 9: `BrowseButton`, `PathLabel` and `pickPath`

**Files:**
- Create: `client/src/components/BrowseButton.tsx`, `client/src/components/PathLabel.tsx`, `client/src/components/BrowseButton.test.tsx`
- Modify: `client/src/lib/pickPath.ts`, `client/src/components/index.ts` (export)

**Interfaces:**
- Produces:

```ts
// lib/pickPath.ts
export interface PickPathOptions { mode: "file" | "folder"; title?: string; filters?: …;
  /** A saved connection to browse instead of this computer. */ source?: { connectionId: string } }
// pickPath: source set → pickLocalPath({ ...opts, source }); else unchanged.
```

```tsx
// BrowseButton: main click → onPick(await pickPath(opts)); ▾ → servers (from useConnectionsStore,
// status dot, greyed + "not reachable" when offline) and "Add a connection…" (navigate /connections?add=1).
export function BrowseButton(props: { mode: "file" | "folder"; title?: string; filters?: …;
  remote?: boolean; label?: string; disabled?: boolean; onPick: (path: string) => void }): JSX.Element;
export function PathLabel(props: { path: string }): JSX.Element; // server icon + displayPath for remote
```

- [ ] **Step 1: Failing tests:** with `remote` false the markup has no menu toggle; with `remote` true and two connections the menu markup (render the open menu via a `menuOpen` test prop, or export the pure `BrowseMenu`) lists "NAS (SMB)" and "Seedbox (SFTP)" and "Add a connection…", an offline one carries "not reachable"; `PathLabel` renders "NAS › games/a.pkg" for a remote path and the raw path for a local one.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement** (menu closes on outside click and Escape; keyboard: ArrowDown opens). **Step 4: Run** — PASS; tsc; eslint.
- [ ] **Step 5: Commit** `feat(remote): a Browse button that also reaches your servers`.

---

### Task 10: The Connections screen

**Files:**
- Create: `client/src/screens/Connections/index.tsx`, `client/src/screens/Connections/ConnectionForm.tsx`, `client/src/screens/Connections/ConnectionForm.test.tsx`
- Modify: `client/src/App.tsx` (route `/connections`), `client/src/layout/navItems.ts` (replace the SMB Browser item with `{ to: "/connections", key: "connections_title", fallback: "Connections", icon: Network }`), `client/src/layout/TabNav.tsx` (`/connections` in the files group's `matches`), `en.ts`, `scripts/i18n-known-missing.json`

**Interfaces:**
- Consumes: Tasks 4, 8.
- Produces: a pure `ConnectionFormView({ value, onChange, shares, testResult, busy, onTest, onSave, onCancel, onAcceptHostKey })` and the connected screen; query `?add=1` opens the form empty, `?edit=<id>` opens it for that connection.

Form rules (exact): protocol tabs SMB / FTP / FTPS / SFTP; switching protocol resets the port to its default only if the port still equals the previous protocol's default; Share (SMB) is a combobox — the listed shares plus free text; **Guest / anonymous** toggle hides user + secret; SFTP shows Password / Key file tabs (key file read with the platform picker, contents sent as `key_pem`); plain FTP shows the label "Unencrypted — use FTPS or SFTP where the server supports it"; **Save** is enabled only after a passing **Test** of the current values (any edit clears the result); a Test result carrying `host_key` shows "First connection to this server. Its key fingerprint is SHA256:… — accept it?" with Accept, and a changed key shows "This server's key has changed since you last connected" with Accept new key.

- [ ] **Step 1: Failing tests** (form view):

```tsx
it("keeps a custom port when switching protocol", () => { /* SMB 445 → FTP 21; SMB 4450 → FTP stays 4450 */ });
it("labels plain FTP as unencrypted", () => { expect(html({ protocol: "ftp" })).toContain("Unencrypted"); });
it("asks for the share only on SMB and hides credentials for guests", () => { … });
it("enables Save only after a passing test", () => {
  expect(saveDisabled(html({ testResult: null }))).toBe(true);
  expect(saveDisabled(html({ testResult: { ok: true } }))).toBe(false);
});
it("asks to accept a new SFTP host key", () => {
  expect(html({ protocol: "sftp", testResult: { ok: false, error: "unknown host key", host_key: "SHA256:abc" } })).toContain("SHA256:abc");
});
```

(`saveDisabled` finds the Save `<button …>` tag in the markup and checks for a `disabled=""` attribute — not a class name.)

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement** the form and the screen (list rows: name, protocol, host, status dot + text, buttons Browse / Test / Edit / Delete — Delete confirms via `ConfirmDialog`; Browse opens the picker with `actions: true`, `onInstall` → the Install Package stream install, `onSend` → the Upload screen with the path preselected via the upload store's `pickFile`/`pickFolder`). i18n keys for every string.
- [ ] **Step 4: Run** — PASS; full client gate; `npm run i18n:check`.
- [ ] **Step 5: Commit** `feat(remote): the Connections screen`.

---

### Task 11: Servers on Home

**Files:**
- Create: `client/src/screens/Home/ServersCard.tsx`, `client/src/screens/Home/ServersCard.test.tsx`
- Modify: `client/src/screens/Home/index.tsx` (add the card to the grid; replace the "Start FTP server" quick action with "Connect a server" → `/connections?add=1`), `en.ts`, i18n json

**Interfaces:**
- Produces: `ServersCardView({ connections, status, onBrowse, onAdd })` + connected `ServersCard`.

- [ ] **Step 1: Failing tests:** with none → "Connect a NAS or server" and an Add button; with two → both names, protocol, a Browse button each, and "Add a server".
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement** (card uses the existing `Card` + `SectionHeading` pattern in Home; `xl:col-span-5`). **Step 4: Run** — PASS; gate; i18n.
- [ ] **Step 5: Commit** `feat(remote): your servers on Home`.

---

### Task 12: Every source picker reaches your servers

**Files:**
- Create: `client/src/lib/materialize.ts`, `client/src/lib/materialize.test.ts`, `client/src/lib/sourcePickers.test.ts`
- Modify: `screens/Upload/index.tsx` (file + folder), `state/upload.ts` (`pickFolder` → `/api/remote/inspect-folder` for remote; `pickFile` .pkg → parse-split works for remote), `screens/FileSystem/index.tsx` (add files, replace), `screens/InstallPackage/index.tsx` (both pickers; a remote pick forces the stream method), `state/pkgLibrary.ts` (`installStream` accepts a `remote://` string source: task payload `{ remotePath: displayPath }`, never credentials; label uses the basename), `screens/Payloads/SendPanel.tsx`, `screens/Payloads/PlaylistsPanel.tsx` (2), `screens/Saves/index.tsx` (restore zip), `screens/Profile/index.tsx` (avatar)

**Interfaces:**
- Consumes: Tasks 5–7, 9.
- Produces: `materializeRemote(path: string, opts?: { destDir?: string; onProgress?: (done: number, total: number) => void; signal?: AbortSignal }): Promise<string>` — returns `path` unchanged when local; otherwise runs `/api/remote/fetch`, polls the job, resolves with the local `dest`. Wrapped with `trackTask({ kind: "download", origin: "remote.fetch", label: "Copy <name> from <server>" })` so the activity bar shows it.

Call-site table (the test in Step 1 pins it — each row: file, what it picks, `remote` on/off, consumer):

| Site | Remote | Consumer |
|---|---|---|
| Upload choose file / folder | on | engine upload (streams; archives go through `materializeRemote` first) |
| FileSystem add files / replace | on | engine upload |
| InstallPackage ×2 | on | `installStream(remotePath)` |
| FpkgConvert source | on | Task 13 |
| Payloads SendPanel, Playlists ×2 | on | `materializeRemote` at send time (a playlist stores the remote path) |
| Saves restore zip | on | `materializeRemote` before unzip |
| Profile avatar | on | `materializeRemote` before upload |
| LocalImage open | off | reads the image in place — ledger: random access over the network is out of scope |
| All save destinations (Screenshots, Videos, BugReport, Settings, Search, Logs, Stats, Convert output, Library download/backup, Saves backup, FileSystem download, FFPKG extract) | off | — |

- [ ] **Step 1: Failing tests.** `materialize.test.ts`: a local path resolves unchanged with no fetch; a remote path posts `/api/remote/fetch`, polls until done, resolves with `dest`, and registers a task that ends done; a failed job rejects with the job's error and ends the task failed. `pkgLibrary` test: `installStream("remote://nas-1/games/a.pkg", host)` registers a task whose `JSON.stringify(payload)` contains "NAS › games/a.pkg" and no `remote://` connection internals beyond the display path, and the invoke to `pkg_install_start` carries `path: "remote://nas-1/games/a.pkg"` with `smb: null`. `sourcePickers.test.ts`: reads each file in the table and asserts the source sites render `<BrowseButton` with `remote` and the destination sites do not (a source-text check, the way `browserInvokeCoverage.test.ts` checks coverage — read the files with `fs.readFileSync`).
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement** the helper and each call site; keep each site's existing Android / browser-build branches (`isAndroid() || !isTauriEnv()` → `pickLocalPath`) — `BrowseButton`'s main click calls `pickPath`, which already routes those.
- [ ] **Step 4: Run** the full client gate — PASS.
- [ ] **Step 5: Commit** `feat(remote): pick from your servers wherever you pick a source`.

---

### Task 13: Convert from a server

**Files:**
- Modify: `client/src/state/fpkgConversion.ts` (a `copy` stage before `check` when the source is remote), `client/src/screens/FpkgConvert/stages.ts` (+ `copy` row, weight 25 — rebalance so weights still sum to 100: copy 25, check 1, plan 1, compress 45, write 16, verify 5, send 5, install 2), `client/src/screens/FpkgConvert/index.tsx` (inspect via the copy? — no: for a remote source the Game card shows the remote name and "Copied to this computer when you start", and inspect runs after the copy), tests in `fpkgConversion.test.ts` and `stages.test.ts`

**Interfaces:**
- Consumes: Task 12 `materializeRemote` (with `destDir: <outputDir>/.ps5upload-source`).
- Produces: `PipelineStage` gains `"copy"`; `STAGE_LABEL.copy = "Copy from server"`.

- [ ] **Step 1: Failing tests:** a remote `start()` enters `copy` first with byte progress from the fetch job, then builds from the returned local path (assert `fpkg.build` got the local path); a failed copy ends the run failed at `copy` with the fetch error; after a successful build the copied source is removed through `remoteApi.cleanupFetched(dest)` (`POST /api/remote/fetch/cleanup`, Task 7), and after a failed build it is kept so Retry does not copy again; `stageRows` for a remote convert lists Copy first.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS; client gate.
- [ ] **Step 5: Commit** `feat(remote): convert a game that lives on your server`.

---

### Task 14: FTP and FTPS

**Files:**
- Create: `remote/ftp_fs.rs`
- Modify: `remote/pool.rs` (connector arms `Ftp`, `Ftps`), `Cargo.toml` (`suppaftp = { version = "12", default-features = false, features = ["tokio-rustls-ring"] }` — confirm with `cargo info suppaftp`)

**Interfaces:**
- Produces: `FtpFs::connect(conn, secret, tls: bool)` implementing `RemoteFs`. Anonymous = user `anonymous`, password empty. Listing uses `MLSD` and falls back to `LIST` parsing (suppaftp's `list` parser) when the server lacks MLSD. `read_at` opens a data connection with `REST <offset>` + `RETR`, reads `len` bytes, then aborts the transfer (`ABOR`) — one control session per concurrent reader, so `open` returns a file that owns a small pool of control connections (max 4) for positioned reads.

- [ ] **Step 1: Failing opt-in contract test** (`#[ignore]`, `PS5UPLOAD_FTP_TEST=host:port`, `PS5UPLOAD_FTPS_TEST=host:port`; e.g. `docker run -p 2121:21 -p 30000-30009:30000-30009 delfer/alpine-ftp-server` with a known user) calling `contract::check`; and unit tests for the LIST-line fallback parser on a Unix `ls -l` line and a Windows `DIR` line.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** unit tests — PASS; run the opt-in tests if Docker is available and ledger the result; fmt; clippy; Android check.
- [ ] **Step 5: Commit** `feat(remote): FTP and FTPS servers`.

---

### Task 15: SFTP

**Files:**
- Create: `remote/sftp_fs.rs`
- Modify: `remote/pool.rs` (connector arm `Sftp`), `remote/api.rs` (`POST /api/remote/connections/{id}/host-key { fingerprint }`), `Cargo.toml` (`russh = "0.63"`, `russh-sftp = "3"` — check with `cargo info` which crypto backend feature avoids `aws-lc-rs` (prefer `ring`) so Windows/Android cross-builds need no C toolchain beyond what ring needs)

**Interfaces:**
- Produces: `SftpFs::connect(conn, secret)` implementing `RemoteFs`; host-key check: no stored `host_key` → fail with `RemoteError::Auth("unknown host key")` and include `host_key: "SHA256:<base64>"` in the API error body; stored but different → `"the server's host key has changed"` + the new fingerprint; equal → proceed. `read_at` uses SFTP `read` at offset (concurrent requests on one session).

- [ ] **Step 1: Failing tests:** unit test of the host-key decision function `check_host_key(stored: Option<&str>, presented: &str) -> Result<(), HostKeyProblem>` for the three cases; opt-in contract test (`PS5UPLOAD_SFTP_TEST=host:port` against `docker run -p 2222:22 atmoz/sftp user:pass:::games`) calling `contract::check`, with the fingerprint accepted first through the store.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS (+ opt-in, ledgered); fmt; clippy; Android check (ruling + cfg if russh will not cross-compile).
- [ ] **Step 5: Commit** `feat(remote): SFTP servers with host-key checking`.

---

### Task 16: Retire SMB Browser and FTP Server

**Files:**
- Delete: `client/src/screens/SmbBrowser/`, `client/src/screens/FtpServer/`, `engine/crates/ps5upload-engine/src/smb_range.rs`
- Modify: `client/src/App.tsx` (remove the two routes; add redirects `/smb-browser` → `/connections`, `/ftp-server` → `/connections` so old bookmarks and favourites land somewhere), `layout/navItems.ts` (remove FTP Server), `layout/TabNav.tsx` (remove `/smb`, `/smb-browser`, `/ftp-server` matches), `api/ps5.ts` (remove smb_* and ftp_* bindings), `lib/browserInvoke.ts` (+ its coverage test lists), `client/src-tauri/src/lib.rs` + `commands` (remove `smb_list_shares`, `smb_list_dir`, `smb_download_file`, `smb_transfer`), engine `lib.rs` (remove `/api/smb/*` routes, `smb_transfer_handler`, `ftp_start_handler`/`ftp_status_handler` routes — the payload's FTP stays), `smb.rs` (delete what `smb_fs.rs` no longer uses), `pkg_install.rs` (drop `RemotePkg::Smb` and the `smb` request field; `state/pkgLibrary.ts` drops `SmbStreamSource`), any favourites/nav tests that list those routes; i18n: leave the old keys (ledger)

- [ ] **Step 1: Failing test:** extend the nav/route test that lists routes (grep `navItems` tests) — `/smb-browser` and `/ftp-server` are not nav items, and navigating to them redirects to `/connections` (assert the `<Navigate>` elements in `App.tsx` via a source-text check if the route tree is not unit-renderable).
- [ ] **Step 2: Run** — FAIL. **Step 3: Remove** (Rust first: `cargo build -p ps5upload-engine`, fmt, clippy; then client). **Step 4: Run** the full engine tests, the full client gate, `npm run i18n:check`, and `cargo check` for `client/src-tauri` — PASS.
- [ ] **Step 5: Commit** `refactor(remote): retire SMB Browser and FTP Server`.

---

### Task 17: End to end

- [ ] **Step 1: Full gate:** `cd engine && cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`; `cd client && npx vitest run && npx tsc --noEmit -p . && npx eslint src`; `npm run i18n:check`. Expected: green.
- [ ] **Step 2: Protocol containers** (if Docker is available): Samba, vsftpd/alpine-ftp (FTP + FTPS), atmoz/sftp — run the ignored contract tests. Record results in the ledger.
- [ ] **Step 3: By hand (user):** over the Ethernet adapter, add the NAS (or the Samba container on the Mac) in Connections → Test → Save; from Install Package ▾ → NAS, stream-install a package to the Phat (FW 5.10) and note the MB/s against today's ~110 MB/s SMB ceiling; from Upload ▾ → NAS, upload a small game folder; from Convert ▾ → NAS, convert a small title (copy stage visible in the activity bar); pull the network cable mid-install for 5 s → the install continues after the retry.
- [ ] **Step 4: Commit fixes**, if any.
