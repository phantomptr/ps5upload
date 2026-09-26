//! Save data + screenshot listing over FTX2.
//!
//! Both ops walk a known PS5 path tree on the payload side and return
//! per-entry metadata. The actual download/upload of save data uses
//! the existing FS_READ + transfer paths — these RPCs only do the
//! enumeration.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveEntry {
    pub title_id: String,
    pub user_id: i32,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    /// "ps5" for native, "ps4" for legacy savedata. Lets the UI
    /// group by platform without re-parsing path strings.
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveList {
    pub saves: Vec<SaveEntry>,
}

/// List save data folders. Pass `user_id == 0` to list every user's
/// saves; non-zero filters to one user.
pub fn list_saves(addr: &str, user_id: i32) -> Result<SaveList> {
    let body = serde_json::json!({ "user_id": user_id });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::ListSaves, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected LIST_SAVES: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::ListSavesAck {
        bail!("expected LIST_SAVES_ACK, got {ft:?}");
    }
    let parsed: SaveList = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotEntry {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotList {
    pub items: Vec<ScreenshotEntry>,
}

pub fn list_screenshots(addr: &str) -> Result<ScreenshotList> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::ListScreenshots, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected LIST_SCREENSHOTS: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::ListScreenshotsAck {
        bail!("expected LIST_SCREENSHOTS_ACK, got {ft:?}");
    }
    let parsed: ScreenshotList = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

/// List gameplay video clips on the console. Same wire/JSON shape as
/// `list_screenshots` (path/size/mtime) so the client reuses the row
/// renderer and the generic transfer-download path — the only difference
/// is the payload walks `/user/av_contents/video` for `.webm`/`.mp4`.
pub fn list_videos(addr: &str) -> Result<ScreenshotList> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::ListVideos, &[])?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected LIST_VIDEOS: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::ListVideosAck {
        bail!("expected LIST_VIDEOS_ACK, got {ft:?}");
    }
    let parsed: ScreenshotList = serde_json::from_slice(&resp)?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field here is required, so a payload-side rename is a parse
    /// error rather than a silent zero — which is the behaviour we want
    /// for a list the user chooses files from. These pin the shape.
    #[test]
    fn parses_a_save_list_with_both_platforms() {
        let l: SaveList = serde_json::from_str(
            r#"{"saves":[
                {"title_id":"CUSA00900","user_id":1,"path":"/user/home/1/savedata/CUSA00900",
                 "size":4096,"mtime":1786320000,"kind":"ps4"},
                {"title_id":"PPSA01325","user_id":1,"path":"/user/home/1/savedata/PPSA01325",
                 "size":8192,"mtime":1786320001,"kind":"ps5"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(l.saves.len(), 2);
        assert_eq!(l.saves[0].kind, "ps4");
        assert_eq!(l.saves[1].title_id, "PPSA01325");
    }

    #[test]
    fn parses_an_empty_save_list() {
        let l: SaveList = serde_json::from_str(r#"{"saves":[]}"#).unwrap();
        assert!(l.saves.is_empty());
    }

    /// A camelCase key is the mistake this codebase has actually made —
    /// the payload must emit snake_case. Here it fails loudly, which is
    /// the desired outcome for required fields.
    #[test]
    fn a_camel_case_key_is_a_parse_error_not_a_silent_zero() {
        let r = serde_json::from_str::<SaveList>(
            r#"{"saves":[{"titleId":"CUSA00900","user_id":1,"path":"/p",
                          "size":1,"mtime":1,"kind":"ps4"}]}"#,
        );
        assert!(r.is_err(), "titleId should not deserialize as title_id");
    }

    /// Screenshots and video clips share this shape; `items` is the
    /// field the payload emits for both.
    #[test]
    fn parses_a_capture_listing() {
        let s: ScreenshotList = serde_json::from_str(
            r#"{"items":[{"path":"/data/av_contents/x.jpg","size":204800,"mtime":1786320000}]}"#,
        )
        .unwrap();
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].path, "/data/av_contents/x.jpg");
        assert_eq!(s.items[0].size, 204800);

        let empty: ScreenshotList = serde_json::from_str(r#"{"items":[]}"#).unwrap();
        assert!(empty.items.is_empty());
    }
}
