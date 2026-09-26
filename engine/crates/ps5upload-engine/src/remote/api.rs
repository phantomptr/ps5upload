//! `/api/remote/*`: saved connections and what is on them.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use super::pool::{global, Remote};
use super::store::{default_port, Connection, Protocol, Secret};
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
}

#[derive(Deserialize)]
pub struct ConnectionBody {
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
    if c.protocol == Protocol::Smb && c.share.trim().is_empty() {
        return Err(Box::new(err(
            StatusCode::BAD_REQUEST,
            "An SMB connection needs a share.",
        )));
    }
    let port = c
        .port
        .filter(|p| *p != 0)
        .unwrap_or_else(|| default_port(&c.protocol));
    let user = c.user.trim().to_string();
    let secret = if user.is_empty() {
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
    Ok((
        Connection {
            id: String::new(),
            name: c.name.trim().to_string(),
            protocol: c.protocol,
            host: c.host.trim().to_string(),
            port,
            share: c.share.trim().trim_matches(['/', '\\']).to_string(),
            user,
            start_path: c.start_path.trim().to_string(),
            host_key: None,
        },
        secret,
    ))
}

fn remote_err(e: &RemoteError) -> Response {
    let code = match e {
        RemoteError::UnknownConnection(_) | RemoteError::NotFound(_) => StatusCode::NOT_FOUND,
        RemoteError::BadPath(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::BAD_GATEWAY,
    };
    err(code, e.to_string())
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
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
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
        Ok(v) => Json(v).into_response(),
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
        Err(e) => Json(json!({ "ok": false, "error": e.to_string() })).into_response(),
    }
}

pub(crate) async fn test_saved(r: &Remote, id: &str) -> Response {
    match r.store.get(id) {
        Some((conn, secret)) => try_connection(r, &conn, &secret).await,
        None => remote_err(&RemoteError::UnknownConnection(id.to_string())),
    }
}

pub(crate) async fn test_form(r: &Remote, body: ConnectionBody) -> Response {
    let (conn, secret) = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    try_connection(r, &conn, &secret.unwrap_or(Secret::None)).await
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

#[cfg(test)]
mod tests {
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
}
