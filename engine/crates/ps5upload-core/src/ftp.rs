//! FTP Server proxy: start/stop/status an embedded FTP server on the PS5.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtpStartRequest {
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub readonly: bool,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub pass: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtpStartResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtpStatusResponse {
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub connections: u32,
    #[serde(default)]
    pub root: String,
}

fn send_recv(
    addr: &str,
    req_type: FrameType,
    ack_type: FrameType,
    body: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut c = Connection::connect(addr)?;
    let empty: Vec<u8> = Vec::new();
    let body = body.unwrap_or(&empty);
    c.send_frame(req_type, body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected {:?}: {}",
            req_type,
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != ack_type {
        bail!("expected {:?}, got {:?}", ack_type, ft);
    }
    Ok(resp)
}

pub fn ftp_start(addr: &str, req: &FtpStartRequest) -> Result<FtpStartResponse> {
    let body = serde_json::to_vec(req)?;
    let resp = send_recv(
        addr,
        FrameType::FtpStart,
        FrameType::FtpStartAck,
        Some(&body),
    )?;
    Ok(serde_json::from_slice(&resp)?)
}

pub fn ftp_status(addr: &str) -> Result<FtpStatusResponse> {
    let resp = send_recv(addr, FrameType::FtpStatus, FrameType::FtpStatusAck, None)?;
    Ok(serde_json::from_slice(&resp)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_start_ok() {
        let json = r#"{"ok":true,"port":2121,"root":"/"}"#;
        let resp: FtpStartResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.port, 2121);
    }

    #[test]
    fn deserialize_start_err() {
        let json = r#"{"ok":false,"error":"bind_failed","port":2121}"#;
        let resp: FtpStartResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("bind_failed"));
    }

    #[test]
    fn deserialize_status_running() {
        let json = r#"{"running":true,"port":2121,"connections":2,"root":"/"}"#;
        let resp: FtpStatusResponse = serde_json::from_str(json).unwrap();
        assert!(resp.running);
        assert_eq!(resp.connections, 2);
    }

    #[test]
    fn deserialize_status_stopped() {
        let json = r#"{"running":false,"port":0,"connections":0}"#;
        let resp: FtpStatusResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.running);
    }

    #[test]
    fn start_request_with_auth() {
        let json = r#"{"port":21,"root":"/data","readonly":false,"user":"admin","pass":"secret"}"#;
        let req: FtpStartRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.port, 21);
        assert_eq!(req.root, "/data");
        assert_eq!(req.user, "admin");
        assert_eq!(req.pass, "secret");
        assert!(!req.readonly);
    }

    #[test]
    fn start_request_readonly_no_auth() {
        let json = r#"{"port":8080,"root":"/","readonly":true}"#;
        let req: FtpStartRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.port, 8080);
        assert!(req.readonly);
        assert!(req.user.is_empty());
        assert!(req.pass.is_empty());
    }

    #[test]
    fn start_request_empty() {
        let json = r#"{}"#;
        let req: FtpStartRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.port, 0);
        assert!(req.root.is_empty());
        assert!(!req.readonly);
    }

    #[test]
    fn start_response_already_running() {
        let json = r#"{"ok":false,"error":"already_running","port":2121}"#;
        let resp: FtpStartResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.port, 2121);
    }

    #[test]
    fn status_response_many_connections() {
        let json = r#"{"running":true,"port":21,"connections":100,"root":"/data/games"}"#;
        let resp: FtpStatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.connections, 100);
        assert_eq!(resp.root, "/data/games");
    }

    #[test]
    fn serialize_start_request() {
        let req = FtpStartRequest {
            port: 2121,
            root: "/".to_string(),
            readonly: true,
            user: String::new(),
            pass: String::new(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("2121"));
        assert!(json.contains("\"readonly\":true"));
    }

    #[test]
    fn start_response_socket_failed() {
        let json = r#"{"ok":false,"error":"socket_failed"}"#;
        let resp: FtpStartResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("socket_failed"));
    }

    #[test]
    fn start_response_listen_failed() {
        let json = r#"{"ok":false,"error":"listen_failed"}"#;
        let resp: FtpStartResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
    }

    #[test]
    fn start_response_thread_failed() {
        let json = r#"{"ok":false,"error":"thread_failed"}"#;
        let resp: FtpStartResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
    }

    #[test]
    fn start_request_high_port() {
        let json = r#"{"port":65535,"root":"/data","readonly":false}"#;
        let req: FtpStartRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.port, 65535);
    }

    #[test]
    fn status_response_stop_ack() {
        let json = r#"{"ok":true,"port":0,"was_running":true}"#;
        let resp: FtpStartResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.port, 0);
    }

    #[test]
    fn start_request_with_special_chars_in_pass() {
        let json = r#"{"port":21,"pass":"p@ss!w0rd#$"}"#;
        let req: FtpStartRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.pass, "p@ss!w0rd#$");
    }

    #[test]
    fn deserialize_status_zero_connections() {
        let json = r#"{"running":true,"port":2121,"connections":0,"root":"/"}"#;
        let resp: FtpStatusResponse = serde_json::from_str(json).unwrap();
        assert!(resp.running);
        assert_eq!(resp.connections, 0);
    }
}
