//! `/api/remote/*`: saved connections and what is on them.

use std::sync::Arc;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use super::hints::hint_for;
use super::path;
use super::pool::{global, Remote};
use super::store::{default_port, Connection, ConnectionView, Protocol, Secret};
use super::RemoteError;

#[derive(Deserialize)]
pub struct ConnectionInput {
    pub name: String,
    pub protocol: Protocol,
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub share: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub start_path: String,
    /// SFTP/FTP: paths from the server's `/` ([`Connection::from_root`]).
    #[serde(default)]
    pub from_root: bool,
    /// WebDAV over HTTPS: accept a self-signed certificate.
    #[serde(default)]
    pub insecure_tls: bool,
}

#[derive(Deserialize)]
pub struct ConnectionBody {
    /// Editing a saved connection: a form without a new secret borrows the saved one.
    #[serde(default)]
    pub id: Option<String>,
    pub connection: ConnectionInput,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub key_pem: Option<String>,
    #[serde(default)]
    pub key_passphrase: Option<String>,
}

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

/// Validate the form and split it into the connection and its secret. A connection with no
/// user is a guest/anonymous one and never keeps a secret.
fn parse_body(body: ConnectionBody) -> Result<(Connection, Option<Secret>), Box<Response>> {
    let c = body.connection;
    if c.name.trim().is_empty() || c.host.trim().is_empty() {
        return Err(Box::new(err(
            StatusCode::BAD_REQUEST,
            "A name and a server address are required.",
        )));
    }
    // SMB: a whole address pasted into Server (`\\\\nas\\Media\\Games`, `smb://nas/Media`) keeps
    // only the server there; its share and folders fill Share when that was left empty.
    let (host, share_raw) = if c.protocol == Protocol::Smb {
        let (host, path) = split_smb_host(&c.host);
        let share = if c.share.trim().is_empty() {
            path
        } else {
            c.share.clone()
        };
        (host, share)
    } else if c.protocol == Protocol::Nfs {
        // `nas:/volume1/games`, as a mount command spells it: the export goes in its place.
        match c.host.trim().split_once(":/") {
            Some((host, export)) if c.share.trim().is_empty() => {
                (host.to_string(), export.to_string())
            }
            Some((host, _)) => (host.to_string(), c.share.clone()),
            None => (c.host.trim().to_string(), c.share.clone()),
        }
    } else {
        (c.host.trim().to_string(), c.share.clone())
    };
    // WebDAV: a pasted address keeps only the server there; its port, folder and scheme go where
    // they belong. `nas:5006` was dialled as the IPv6 address `[nas:5006]`.
    let (host, typed_port, typed_path, protocol) =
        if matches!(c.protocol, Protocol::Webdav | Protocol::Webdavs) {
            let t = super::webdav_fs::parse_host(&host);
            let protocol = match t.https {
                Some(true) => Protocol::Webdavs,
                Some(false) => Protocol::Webdav,
                None => c.protocol.clone(),
            };
            (t.host, t.port, t.path, protocol)
        } else {
            (host, None, String::new(), c.protocol.clone())
        };
    if host.is_empty() {
        return Err(Box::new(err(
            StatusCode::BAD_REQUEST,
            "A name and a server address are required.",
        )));
    }
    if c.protocol == Protocol::Smb && share_raw.trim_matches(['/', '\\', ' ']).is_empty() {
        return Err(Box::new(err(
            StatusCode::BAD_REQUEST,
            "An SMB connection needs a share.",
        )));
    }
    let port = typed_port
        .or(c.port.filter(|p| *p != 0))
        .unwrap_or_else(|| default_port(&protocol));
    let user = c.user.trim().to_string();
    // NFS has no passwords: its user field is a user and group ID.
    let secret = if user.is_empty() || c.protocol == Protocol::Nfs {
        Some(Secret::None)
    } else if let Some(pem) = body.key_pem.filter(|k| !k.trim().is_empty()) {
        Some(Secret::Key {
            pem,
            passphrase: body.key_passphrase.filter(|p| !p.is_empty()),
        })
    } else {
        body.password
            .filter(|p| !p.is_empty())
            .map(|password| Secret::Password { password })
    };
    let insecure_tls = protocol == Protocol::Webdavs && c.insecure_tls;
    let (share, start_path) = if c.protocol == Protocol::Smb {
        split_share(&share_raw, c.start_path.trim())
    } else {
        (
            share_raw.trim().trim_matches(['/', '\\']).to_string(),
            join_typed_path(&typed_path, c.start_path.trim()),
        )
    };
    let from_root =
        c.from_root && matches!(c.protocol, Protocol::Sftp | Protocol::Ftp | Protocol::Ftps);
    Ok((
        Connection {
            id: String::new(),
            name: c.name.trim().to_string(),
            protocol,
            host,
            port,
            share,
            user,
            start_path,
            from_root,
            host_key: None,
            insecure_tls,
        },
        secret,
    ))
}

/// A folder that came with a pasted server address, in front of the start folder (unless the start
/// folder already says it).
fn join_typed_path(typed: &str, start: &str) -> String {
    if typed.is_empty() || start.starts_with(typed) {
        start.to_string()
    } else if start.is_empty() {
        typed.to_string()
    } else {
        format!("{typed}/{}", start.trim_start_matches('/'))
    }
}

/// An SMB share typed with folders after it (`Media/Games/PS5`, or a pasted
/// `\\nas\Media\Games`): the share is the first name, the folders go in front of the start
/// folder. The server only knows the share's own name ("STATUS_BAD_NETWORK_NAME" otherwise).
pub(crate) fn split_share(raw: &str, start: &str) -> (String, String) {
    let original = start;
    let raw = raw.trim();
    let (raw, url) = strip_smb_scheme(raw);
    let unc = url || raw.starts_with("\\\\") || raw.starts_with("//");
    let mut segs: Vec<&str> = raw
        .split(['/', '\\'])
        .filter(|s| !s.trim().is_empty())
        .collect();
    if unc && segs.len() >= 2 {
        segs.remove(0);
    }
    let Some((share, rest)) = segs.split_first() else {
        return (String::new(), start.to_string());
    };
    let start = start.trim_matches(['/', '\\']);
    let path: Vec<&str> = rest
        .iter()
        .copied()
        .chain(start.split(['/', '\\']).filter(|s| !s.is_empty()))
        .collect();
    if rest.is_empty() {
        return (share.trim().to_string(), original.to_string());
    }
    (share.trim().to_string(), format!("/{}", path.join("/")))
}

/// `smb://` or `cifs://` in front of an address, dropped (any letter case): the rest, and
/// whether there was one (then the first name after it is the server).
fn strip_smb_scheme(raw: &str) -> (&str, bool) {
    for scheme in ["smb://", "cifs://"] {
        // By `get`, not `[..n]`: a name in Chinese (3 bytes a letter) has no letter boundary at
        // byte 6 or 7, and slicing there panicked: every such share failed from 11.0.2 (#663).
        if raw
            .get(..scheme.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(scheme))
        {
            return (&raw[scheme.len()..], true);
        }
    }
    (raw, false)
}

/// The Server field of an SMB form as the server alone and whatever path came after it:
/// `\\\\nas\\Media\\Games` → (`nas`, `Media/Games`), `smb://nas:445/Media` → (`nas:445`, `Media`),
/// `nas` → (`nas`, ``).
pub(crate) fn split_smb_host(raw: &str) -> (String, String) {
    let (rest, _) = strip_smb_scheme(raw.trim());
    let rest = rest.trim_start_matches(['/', '\\']);
    match rest.find(['/', '\\']) {
        Some(i) => (
            rest[..i].trim().to_string(),
            rest[i + 1..]
                .split(['/', '\\'])
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join("/"),
        ),
        None => (rest.trim().to_string(), String::new()),
    }
}

/// `{ error, hint? }` with a status that says what kind of failure it was.
pub(crate) fn remote_err(e: &RemoteError) -> Response {
    let code = match e {
        RemoteError::UnknownConnection(_) | RemoteError::NotFound(_) => StatusCode::NOT_FOUND,
        RemoteError::BadPath(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    };
    let msg = e.to_string();
    (
        code,
        Json(json!({ "error": msg, "hint": hint_for(&msg), "host_key": host_key_of(e) })),
    )
        .into_response()
}

/// `{ ok: false, error, hint?, host_key? }`: a failure answered with a 200, for the routes whose
/// callers read `ok`.
pub(crate) fn failure_body(e: &RemoteError) -> serde_json::Value {
    let msg = e.to_string();
    json!({
        "ok": false,
        "error": msg,
        "hint": hint_for(&msg),
        "host_key": host_key_of(e),
    })
}

/// The error's text with its plain-language hint after it, for a job's error or a transfer's
/// refusal, which have one line to say it in.
pub(crate) fn error_with_hint(e: &RemoteError) -> String {
    let msg = e.to_string();
    match hint_for(&msg) {
        Some(hint) => format!("{}. {hint}", msg.trim_end_matches('.')),
        None => msg,
    }
}

/// The fingerprint to offer for acceptance when the server's identity is unknown or changed.
fn host_key_of(e: &RemoteError) -> Option<&str> {
    match e {
        RemoteError::HostKey { fingerprint, .. } => Some(fingerprint),
        _ => None,
    }
}

pub(crate) async fn list_connections(r: &Remote) -> Response {
    Json(json!({ "connections": r.store.list() })).into_response()
}

pub(crate) async fn add_connection(r: &Remote, body: ConnectionBody) -> Response {
    let (conn, secret) = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    match r.store.add(conn, secret.unwrap_or(Secret::None)) {
        Ok(v) => Json(login_start(r, v).await).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// A connection that browses the whole server, saved without a start folder, starts in the
/// folder the server signs in to (as an SSH or FTP client does), asked of the server once
/// here. Unreachable or not yet trusted: it stays empty, and browsing starts at `/`.
async fn login_start(r: &Remote, v: ConnectionView) -> ConnectionView {
    let id = v.conn.id.clone();
    if !v.conn.from_root || !v.conn.start_path.is_empty() {
        return v;
    }
    let Some((conn, secret)) = r.store.get(&id) else {
        return v;
    };
    let fs = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        r.pool.connector().connect(&conn, &secret),
    )
    .await;
    let Some(dir) = fs.ok().and_then(|f| f.ok()).and_then(|f| f.login_dir()) else {
        return v;
    };
    let mut c = conn;
    c.start_path = dir;
    r.store.update(&id, c, None).unwrap_or(v)
}

pub(crate) async fn update_connection(r: &Remote, id: &str, body: ConnectionBody) -> Response {
    let (conn, secret) = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    if r.store.get(id).is_none() {
        return remote_err(&RemoteError::UnknownConnection(id.to_string()));
    }
    r.pool.invalidate(id);
    match r.store.update(id, conn, secret) {
        Ok(v) => Json(login_start(r, v).await).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

pub(crate) async fn delete_connection(r: &Remote, id: &str) -> Response {
    r.pool.invalidate(id);
    match r.store.remove(id) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(_) => remote_err(&RemoteError::UnknownConnection(id.to_string())),
    }
}

/// Sign in and list the start folder: `{ ok: true }` or `{ ok: false, error }`.
async fn try_connection(r: &Remote, conn: &Connection, secret: &Secret) -> Response {
    let result = async {
        let fs = r.pool.connector().connect(conn, secret).await?;
        let start = if conn.start_path.is_empty() {
            "/"
        } else {
            conn.start_path.as_str()
        };
        fs.list(start, None).await.map(|_| ())
    }
    .await
    .map_err(|e| super::pool::scrub(e, secret));
    match result {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => Json(failure_body(&e)).into_response(),
    }
}

/// One page of a folder on a saved server.
pub(crate) async fn list_dir(r: &Remote, remote: &str, cursor: Option<String>) -> Response {
    let result = async {
        let p = path::parse(remote)?;
        r.pool
            .with_fs(&r.store, &p.connection_id, |fs| {
                let (dir, cursor) = (p.path.clone(), cursor.clone());
                async move { fs.list(&dir, cursor).await }
            })
            .await
    }
    .await;
    match result {
        Ok(page) => Json(page).into_response(),
        Err(e) => remote_err(&e),
    }
}

/// The shares an SMB server offers (for the connection form's share picker).
async fn shares_of(conn: &Connection, secret: &Secret) -> Response {
    if conn.protocol != Protocol::Smb {
        return err(StatusCode::BAD_REQUEST, "Only SMB servers have shares.");
    }
    match super::smb_fs::list_shares(conn, secret).await {
        Ok(shares) => Json(json!({ "shares": shares })).into_response(),
        Err(e) => remote_err(&super::pool::scrub(e, secret)),
    }
}

/// Trust the server key (SFTP host key or FTPS certificate) the user just accepted.
pub(crate) async fn accept_host_key(r: &Remote, id: &str, fingerprint: &str) -> Response {
    if r.store.get(id).is_none() {
        return remote_err(&RemoteError::UnknownConnection(id.to_string()));
    }
    if !fingerprint.starts_with("SHA256:") {
        return err(StatusCode::BAD_REQUEST, "That is not a key fingerprint.");
    }
    r.pool.invalidate(id);
    match r.store.set_host_key(id, fingerprint) {
        Ok(()) => {
            // Now trusted: a whole-server connection saved without a start folder can learn it.
            if let Some(v) = r.store.list().into_iter().find(|v| v.conn.id == id) {
                login_start(r, v).await;
            }
            Json(json!({ "ok": true })).into_response()
        }
        Err(_) => remote_err(&RemoteError::UnknownConnection(id.to_string())),
    }
}

pub(crate) async fn test_saved(r: &Remote, id: &str) -> Response {
    match r.store.get(id) {
        Some((conn, secret)) => try_connection(r, &conn, &secret).await,
        None => remote_err(&RemoteError::UnknownConnection(id.to_string())),
    }
}

/// The form's secret, or — for an edit that did not retype it — the saved one, but only while
/// the form still points at the server that secret was saved for. Otherwise editing the host
/// and pressing Test would send the NAS password to whatever host was typed. Stricter than Save,
/// which keeps a password across a port fix: this one sends it now, to a port nobody has saved yet.
fn form_secret(r: &Remote, id: Option<&str>, conn: &Connection, secret: Option<Secret>) -> Secret {
    if let Some(s) = secret {
        return s;
    }
    id.and_then(|id| r.store.get(id))
        .filter(|(saved, _)| super::store::same_machine(saved, conn) && saved.port == conn.port)
        .map(|(_, s)| s)
        .unwrap_or(Secret::None)
}

pub(crate) async fn test_form(r: &Remote, body: ConnectionBody) -> Response {
    let id = body.id.clone();
    let (conn, secret) = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let secret = form_secret(r, id.as_deref(), &conn, secret);
    try_connection(r, &conn, &secret).await
}

/// Saved connections (SMB shares, FTP, SFTP, WebDAV) for a bug report: protocol, server and share
/// names, whether a user and a password are saved. Never the password, user name or paths.
pub(crate) fn diag_summary() -> serde_json::Value {
    match global() {
        Ok(r) => json!({
            "connections": r.store.list().iter().map(|v| json!({
                "protocol": v.conn.protocol,
                "host": v.conn.host,
                "port": v.conn.port,
                "share": v.conn.share,
                "has_user": !v.conn.user.is_empty(),
                "has_secret": v.has_secret,
                "insecure_tls": v.conn.insecure_tls,
            })).collect::<Vec<_>>(),
        }),
        Err(e) => json!({ "error": e.to_string() }),
    }
}

// ─── axum handlers over the engine's store ──────────────────────────────────

macro_rules! with_remote {
    ($r:ident => $body:expr) => {
        match global() {
            Ok($r) => $body,
            Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    };
}

pub async fn list_handler() -> Response {
    with_remote!(r => list_connections(&r).await)
}
pub async fn add_handler(Json(body): Json<ConnectionBody>) -> Response {
    with_remote!(r => add_connection(&r, body).await)
}
pub async fn update_handler(Path(id): Path<String>, Json(body): Json<ConnectionBody>) -> Response {
    with_remote!(r => update_connection(&r, &id, body).await)
}
pub async fn delete_handler(Path(id): Path<String>) -> Response {
    with_remote!(r => delete_connection(&r, &id).await)
}
pub async fn test_saved_handler(Path(id): Path<String>) -> Response {
    with_remote!(r => test_saved(&r, &id).await)
}
pub async fn test_form_handler(Json(body): Json<ConnectionBody>) -> Response {
    with_remote!(r => test_form(&r, body).await)
}

#[derive(Deserialize)]
pub struct ListBody {
    pub path: String,
    #[serde(default)]
    pub cursor: Option<String>,
}

pub async fn list_dir_handler(Json(body): Json<ListBody>) -> Response {
    with_remote!(r => list_dir(&r, &body.path, body.cursor).await)
}

pub async fn shares_saved_handler(Path(id): Path<String>) -> Response {
    with_remote!(r => match r.store.get(&id) {
        Some((conn, secret)) => shares_of(&conn, &secret).await,
        None => remote_err(&RemoteError::UnknownConnection(id)),
    })
}

pub async fn shares_form_handler(Json(mut body): Json<ConnectionBody>) -> Response {
    // Listing shares happens before one is chosen, so the form may not name one yet.
    if body.connection.share.trim().is_empty() {
        body.connection.share = "IPC$".into();
    }
    let id = body.id.clone();
    with_remote!(r => match parse_body(body) {
        Ok((conn, secret)) => {
            let secret = form_secret(&r, id.as_deref(), &conn, secret);
            shares_of(&conn, &secret).await
        }
        Err(resp) => *resp,
    })
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_smb_share_typed_with_folders_is_split() {
        assert_eq!(split_share("Media", ""), ("Media".into(), String::new()));
        assert_eq!(
            split_share("/Media/", "games"),
            ("Media".into(), "games".into())
        );
        assert_eq!(
            split_share("Media/Games/PS5", ""),
            ("Media".into(), "/Games/PS5".into())
        );
        assert_eq!(
            split_share("\\\\nas\\Media\\Games", ""),
            ("Media".into(), "/Games".into())
        );
        assert_eq!(
            split_share("//nas/Media", "/x"),
            ("Media".into(), "/x".into())
        );
        assert_eq!(
            split_share("Media/Games", "/PS5/"),
            ("Media".into(), "/Games/PS5".into())
        );
        assert_eq!(
            split_share("smb://nas/Media/Games/PS5", ""),
            ("Media".into(), "/Games/PS5".into())
        );
        assert_eq!(
            split_share("SMB://nas/Media", ""),
            ("Media".into(), String::new())
        );
        assert_eq!(
            split_share("\\\\Media\\Games\\", ""),
            ("Games".into(), String::new())
        );
        assert_eq!(
            split_share("Media\\Games\\PS5", "x"),
            ("Media".into(), "/Games/PS5/x".into())
        );
    }

    /// #663: names in Chinese (3 bytes a letter) were sliced mid-letter and panicked.
    #[test]
    fn smb_names_in_any_script_go_through() {
        assert_eq!(
            split_share("共享文件夹", ""),
            ("共享文件夹".into(), String::new())
        );
        assert_eq!(
            split_share("共享文件夹/游戏/PS5", ""),
            ("共享文件夹".into(), "/游戏/PS5".into())
        );
        assert_eq!(split_share("ゲーム", ""), ("ゲーム".into(), String::new()));
        assert_eq!(split_smb_host("我的NAS"), ("我的NAS".into(), String::new()));
        assert_eq!(
            split_smb_host("\\\\群晖\\共享文件夹"),
            ("群晖".into(), "共享文件夹".into())
        );
        let (c, _) = parse_body(smb_form("群晖", "共享文件夹/游戏", ""))
            .ok()
            .unwrap();
        assert_eq!(
            (c.host.as_str(), c.share.as_str(), c.start_path.as_str()),
            ("群晖", "共享文件夹", "/游戏")
        );
    }

    #[test]
    fn a_whole_smb_address_in_server_is_split() {
        assert_eq!(split_smb_host("nas"), ("nas".into(), String::new()));
        assert_eq!(
            split_smb_host(" 10.0.0.5 "),
            ("10.0.0.5".into(), String::new())
        );
        assert_eq!(
            split_smb_host("\\\\nas\\Media\\Games"),
            ("nas".into(), "Media/Games".into())
        );
        assert_eq!(
            split_smb_host("smb://nas:445/Media/Games/"),
            ("nas:445".into(), "Media/Games".into())
        );
        assert_eq!(
            split_smb_host("//nas/Media"),
            ("nas".into(), "Media".into())
        );
    }

    fn smb_form(host: &str, share: &str, start: &str) -> ConnectionBody {
        serde_json::from_value(json!({
            "connection": {
                "name": "NAS", "protocol": "smb", "host": host,
                "share": share, "start_path": start,
            }
        }))
        .unwrap()
    }

    fn webdav_form(host: &str, port: Option<u16>, start: &str) -> ConnectionBody {
        let mut body = smb_form(host, "", start);
        body.connection.protocol = Protocol::Webdavs;
        body.connection.port = port;
        body
    }

    #[test]
    fn a_webdav_form_takes_port_scheme_and_folder_from_a_pasted_address() {
        let parsed = |host: &str, port: Option<u16>, start: &str| {
            parse_body(webdav_form(host, port, start))
                .map_err(|_| "refused")
                .unwrap()
                .0
        };
        // A Synology port typed in Server, over the form's default.
        let c = parsed("nas:5006", Some(443), "");
        assert_eq!((c.host.as_str(), c.port), ("nas", 5006));
        // A whole address: the scheme picks the protocol, the path the start folder.
        let c = parsed("http://nas:8080/remote.php/dav/", None, "");
        assert_eq!((c.host.as_str(), c.port), ("nas", 8080));
        assert_eq!(c.protocol, Protocol::Webdav);
        assert_eq!(c.start_path, "/remote.php/dav");
        // No port anywhere: the protocol's own.
        let c = parsed("https://nas/dav", None, "/dav/files");
        assert_eq!((c.host.as_str(), c.port), ("nas", 443));
        assert_eq!(c.protocol, Protocol::Webdavs);
        assert_eq!(c.start_path, "/dav/files");
        let c = parsed("https://nas/dav", None, "games");
        assert_eq!(c.start_path, "/dav/games");
        // An IPv6 address is still one.
        let c = parsed("fe80::1", Some(5006), "");
        assert_eq!((c.host.as_str(), c.port), ("fe80::1", 5006));
        // Only an address's scheme is no address.
        assert!(parse_body(webdav_form("https://", None, "")).is_err());
    }

    #[test]
    fn a_failure_says_what_to_do_on_paths_with_one_line() {
        let e = RemoteError::Io("STATUS_BAD_NETWORK_NAME during tree connect".into());
        let text = error_with_hint(&e);
        assert!(
            text.starts_with("STATUS_BAD_NETWORK_NAME during tree connect. "),
            "{text}"
        );
        assert!(text.contains("first folder name"), "{text}");
        let plain = RemoteError::Io("something else.".into());
        assert_eq!(error_with_hint(&plain), "something else.");
        let body = failure_body(&e);
        assert_eq!(body["ok"], false);
        assert!(body["hint"].as_str().unwrap().contains("first folder name"));
    }

    #[test]
    fn an_smb_form_takes_the_share_from_a_pasted_address() {
        let (c, _) = parse_body(smb_form("\\\\nas\\Media\\Games\\PS5", "", ""))
            .ok()
            .unwrap();
        assert_eq!(
            (c.host.as_str(), c.share.as_str(), c.start_path.as_str()),
            ("nas", "Media", "/Games/PS5")
        );
        let (c, _) = parse_body(smb_form("smb://nas/Media", "", ""))
            .ok()
            .unwrap();
        assert_eq!(
            (c.host.as_str(), c.share.as_str(), c.start_path.as_str()),
            ("nas", "Media", "")
        );
        // Share typed: it wins, and the server keeps only its own name.
        let (c, _) = parse_body(smb_form("smb://nas/Other", "XXX/YYY/ZZZ", ""))
            .ok()
            .unwrap();
        assert_eq!(
            (c.host.as_str(), c.share.as_str(), c.start_path.as_str()),
            ("nas", "XXX", "/YYY/ZZZ")
        );
        let (c, _) = parse_body(smb_form("nas", "/XXX/YYY/", "")).ok().unwrap();
        assert_eq!((c.share.as_str(), c.start_path.as_str()), ("XXX", "/YYY"));
        // Discord: `share/sub/folder` (either slash) failed with BAD_NETWORK_NAME in other tools'
        // style; the first name is the share and the rest goes in front of Start folder.
        for typed in [
            "share/sub/folder",
            "share\\sub\\folder",
            "share\\sub/folder\\",
        ] {
            let (c, _) = parse_body(smb_form("nas", typed, "/inner")).ok().unwrap();
            assert_eq!(
                (c.share.as_str(), c.start_path.as_str()),
                ("share", "/sub/folder/inner"),
                "{typed}"
            );
        }
        assert!(parse_body(smb_form("nas", "", "")).is_err());
        assert!(parse_body(smb_form("smb://nas/", " / ", "")).is_err());
    }

    use super::*;
    use crate::remote::pool::testing::remote_with;
    use crate::remote::MemFs;

    async fn body(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn form(v: serde_json::Value) -> ConnectionBody {
        serde_json::from_value(v).unwrap()
    }

    fn nas_form() -> ConnectionBody {
        form(
            json!({"connection": {"name":"NAS","protocol":"smb","host":"10.0.0.5","share":"games","user":"me"},
                    "password":"hunter2"}),
        )
    }

    /// SFTP/FTP saved to browse the whole server start in the sign-in folder when no start
    /// folder is given; a typed one is kept; other protocols never browse from the root.
    #[tokio::test]
    async fn a_whole_server_connection_starts_in_the_sign_in_folder() {
        let r = remote_with(MemFs::new(&[("/a", b"x")]), None);
        let sftp = |start: &str| {
            form(
                json!({"connection": {"name":"Box","protocol":"sftp","host":"10.0.0.6","user":"me",
                "start_path": start, "from_root": true}, "password":"pw"}),
            )
        };
        let added = body(add_connection(&r, sftp("")).await).await;
        assert_eq!(added["from_root"], true);
        assert_eq!(added["start_path"], "/home/me");
        let typed = body(add_connection(&r, sftp("/mnt/games")).await).await;
        assert_eq!(typed["start_path"], "/mnt/games");
        let smb = form(
            json!({"connection": {"name":"NAS","protocol":"smb","host":"10.0.0.5",
            "share":"games","from_root": true}}),
        );
        let smb = body(add_connection(&r, smb).await).await;
        assert_eq!(smb["from_root"], false);
        assert_eq!(smb["start_path"], "");
    }

    /// A connection saved before `from_root` existed keeps its sign-in-folder paths, so links
    /// saved under them stay valid.
    #[test]
    fn a_connection_saved_before_reads_as_not_from_root() {
        let old: Connection = serde_json::from_value(json!({"id":"abc","name":"Box",
            "protocol":"sftp","host":"h","port":22,"user":"me","start_path":"/games"}))
        .unwrap();
        assert!(!old.from_root);
    }

    #[tokio::test]
    async fn listing_never_returns_a_secret() {
        let r = remote_with(MemFs::new(&[("/a", b"x")]), None);
        let added = body(add_connection(&r, nas_form()).await).await;
        assert_eq!(added["has_secret"], true);
        assert_eq!(added["port"], 445);
        let list = body(list_connections(&r).await).await;
        assert_eq!(list["connections"][0]["name"], "NAS");
        assert!(!list.to_string().contains("hunter2"));
        assert!(!added.to_string().contains("hunter2"));
    }

    #[tokio::test]
    async fn a_failed_test_never_echoes_the_password() {
        let r = remote_with(
            MemFs::new(&[]),
            Some(|s| RemoteError::Auth(format!("server said no to {:?}", s))),
        );
        let out = body(test_form(&r, nas_form()).await).await;
        assert_eq!(out["ok"], false);
        assert!(out["error"].as_str().unwrap().contains("Sign-in failed"));
        assert!(!out.to_string().contains("hunter2"), "{out}");
    }

    #[tokio::test]
    async fn a_passing_test_lists_the_start_folder() {
        let r = remote_with(MemFs::new(&[("/games/a.pkg", b"x")]), None);
        let out = body(test_form(&r, nas_form()).await).await;
        assert_eq!(out["ok"], true);
    }

    #[tokio::test]
    async fn editing_or_deleting_drops_the_pooled_session() {
        let r = remote_with(MemFs::new(&[("/a", b"x")]), None);
        let id = body(add_connection(&r, nas_form()).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        r.pool.fs(&r.store, &id).await.unwrap();
        let edit = form(
            json!({"connection": {"name":"NAS2","protocol":"smb","host":"10.0.0.5","share":"games","user":"me"}}),
        );
        let edited = body(update_connection(&r, &id, edit).await).await;
        assert_eq!(
            edited["has_secret"], true,
            "an edit without a password keeps it"
        );
        r.pool.fs(&r.store, &id).await.unwrap();
        assert_eq!(r.pool.connects(), 2);
        assert_eq!(delete_connection(&r, &id).await.status(), StatusCode::OK);
        assert!(matches!(
            r.pool.fs(&r.store, &id).await,
            Err(RemoteError::UnknownConnection(_))
        ));
        assert_eq!(
            delete_connection(&r, &id).await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_guest_connection_holds_no_secret() {
        let r = remote_with(MemFs::new(&[]), None);
        let g = form(
            json!({"connection": {"name":"Share","protocol":"smb","host":"pc","share":"PS5PKG"},
                             "password":"ignored"}),
        );
        let v = body(add_connection(&r, g).await).await;
        assert_eq!(v["has_secret"], false);
        let bad = form(json!({"connection": {"name":"","protocol":"ftp","host":""}}));
        assert_eq!(
            add_connection(&r, bad).await.status(),
            StatusCode::BAD_REQUEST
        );
        let no_share = form(json!({"connection": {"name":"x","protocol":"smb","host":"pc"}}));
        assert_eq!(
            add_connection(&r, no_share).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn lists_a_folder_by_remote_path() {
        let r = remote_with(MemFs::new(&[("/g/a.pkg", b"x"), ("/g/b.pkg", b"yy")]), None);
        let id = body(add_connection(&r, nas_form()).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let out = body(list_dir(&r, &format!("remote://{id}/g"), None).await).await;
        let names: Vec<_> = out["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["a.pkg", "b.pkg"]);
        assert!(out["next_cursor"].is_null());
        let gone = list_dir(&r, "remote://nope-0000/g", None).await;
        assert_eq!(gone.status(), StatusCode::NOT_FOUND);
        assert!(body(gone).await["error"]
            .as_str()
            .unwrap()
            .contains("no longer exists"));
        let bad = list_dir(&r, &format!("remote://{id}/../x"), None).await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_failure_carries_a_plain_hint() {
        let r = remote_with(
            MemFs::new(&[]),
            Some(|_| RemoteError::Auth("STATUS_ACCOUNT_DISABLED (0xC0000072)".into())),
        );
        let id = body(add_connection(&r, nas_form()).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let out = body(list_dir(&r, &format!("remote://{id}/"), None).await).await;
        assert!(out["hint"].as_str().unwrap().contains("Guest account"));
        let tested = body(test_saved(&r, &id).await).await;
        assert!(tested["hint"].as_str().unwrap().contains("Guest account"));
    }

    #[tokio::test]
    async fn testing_an_edit_borrows_the_saved_password() {
        let r = remote_with(
            MemFs::new(&[("/a", b"x")]),
            Some(|s| match s {
                Secret::Password { password } if password == "hunter2" => {
                    RemoteError::Io("signed in".into())
                }
                _ => RemoteError::Auth("no password".into()),
            }),
        );
        let id = body(add_connection(&r, nas_form()).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let edit = form(
            json!({"id": id, "connection": {"name":"Home NAS","protocol":"smb","host":"10.0.0.5","share":"games","user":"me"}}),
        );
        let out = body(test_form(&r, edit).await).await;
        assert!(
            out["error"].as_str().unwrap().contains("signed in"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn an_unknown_server_key_comes_back_to_accept() {
        let r = remote_with(
            MemFs::new(&[]),
            Some(|_| RemoteError::HostKey {
                fingerprint: "SHA256:abc".into(),
                changed: false,
            }),
        );
        let out = body(test_form(&r, nas_form()).await).await;
        assert_eq!(out["ok"], false);
        assert_eq!(out["host_key"], "SHA256:abc");
        let id = body(add_connection(&r, nas_form()).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let listed = body(list_dir(&r, &format!("remote://{id}/"), None).await).await;
        assert_eq!(listed["host_key"], "SHA256:abc");
    }

    #[tokio::test]
    async fn accepting_a_host_key_saves_it_and_signs_in_again() {
        let r = remote_with(MemFs::new(&[("/a", b"x")]), None);
        let id = body(add_connection(&r, nas_form()).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        r.pool.fs(&r.store, &id).await.unwrap();
        let resp = accept_host_key(&r, &id, "SHA256:abc").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            r.store.get(&id).unwrap().0.host_key.as_deref(),
            Some("SHA256:abc")
        );
        r.pool.fs(&r.store, &id).await.unwrap();
        assert_eq!(r.pool.connects(), 2, "the old session goes");
        assert_eq!(
            accept_host_key(&r, "nope", "x").await.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_dead_session_is_replaced_instead_of_failing_every_browse() {
        let mem = std::sync::Arc::new(MemFs::new(&[("/g/a.pkg", b"x")]));
        let r = crate::remote::pool::testing::remote_with_shared(std::sync::Arc::clone(&mem), None);
        let id = body(add_connection(&r, nas_form()).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        r.pool.fs(&r.store, &id).await.unwrap();
        mem.fail_next_lists(1); // the NAS rebooted under the pooled session
        let out = list_dir(&r, &format!("remote://{id}/g"), None).await;
        assert_eq!(out.status(), StatusCode::OK);
        assert_eq!(r.pool.connects(), 2, "signed in again");
    }

    #[tokio::test]
    async fn a_saved_password_goes_only_to_the_server_it_was_saved_for() {
        let r = remote_with(
            MemFs::new(&[]),
            Some(|s| {
                RemoteError::Auth(format!(
                    "got {}",
                    if matches!(s, Secret::None) {
                        "none"
                    } else {
                        "a password"
                    }
                ))
            }),
        );
        let id = body(add_connection(&r, nas_form()).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let same = form(
            json!({"id": id, "connection": {"name":"NAS","protocol":"smb","host":"10.0.0.5","share":"games","user":"me"}}),
        );
        assert!(body(test_form(&r, same).await).await["error"]
            .as_str()
            .unwrap()
            .contains("a password"));
        // Only the host's capitals changed: still that server.
        let mut named = nas_form();
        named.connection.host = "NAS.local".into();
        let named = body(add_connection(&r, named).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let respelt = form(
            json!({"id": named, "connection": {"name":"NAS","protocol":"smb","host":" nas.LOCAL ","share":"games","user":"me"}}),
        );
        assert!(body(test_form(&r, respelt).await).await["error"]
            .as_str()
            .unwrap()
            .contains("a password"));
        for other in [
            json!({"name":"NAS","protocol":"smb","host":"203.0.113.9","share":"games","user":"me"}),
            json!({"name":"NAS","protocol":"ftp","host":"10.0.0.5","user":"me"}),
            json!({"name":"NAS","protocol":"smb","host":"10.0.0.5","port":4445,"share":"games","user":"me"}),
        ] {
            let edit = form(json!({"id": id, "connection": other}));
            let out = body(test_form(&r, edit).await).await;
            assert!(out["error"].as_str().unwrap().contains("none"), "{out}");
        }
    }
}

pub async fn fetch_handler(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    Json(body): Json<super::fetch::FetchBody>,
) -> Response {
    with_remote!(r => super::fetch::start_fetch(
        r,
        super::fetch::FetchDeps {
            jobs: Arc::clone(&state.jobs),
            events_tx: state.events_tx.clone(),
            free_bytes: ps5upload_fpkg::build::free_bytes,
            backoff: super::pool::Backoff::standard(),
        },
        body,
    )
    .await)
}

#[derive(Deserialize)]
pub struct CleanupBody {
    pub dest: String,
}

pub async fn fetch_cleanup_handler(Json(body): Json<CleanupBody>) -> Response {
    super::fetch::cleanup(&body.dest).await
}

#[derive(Deserialize)]
pub struct InspectBody {
    pub path: String,
}

pub async fn inspect_folder_handler(Json(body): Json<InspectBody>) -> Response {
    with_remote!(r => super::fetch::inspect_folder(&r, &body.path).await)
}

#[derive(Deserialize)]
pub struct HostKeyBody {
    pub fingerprint: String,
}

pub async fn host_key_handler(Path(id): Path<String>, Json(body): Json<HostKeyBody>) -> Response {
    with_remote!(r => accept_host_key(&r, &id, &body.fingerprint).await)
}
