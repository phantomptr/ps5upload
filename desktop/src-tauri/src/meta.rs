use serde::Serialize;
use tauri::AppHandle;

use ps5upload_core::game_meta::{load_cover_image_from_bytes, load_game_meta_for_path, parse_game_meta_from_param_bytes};
use ps5upload_core::message::{CoverImage, GameMeta};
use ps5upload_core::protocol::probe_rar_metadata;

const TRANSFER_PORT: u16 = 9113;

#[derive(Clone, Serialize)]
pub struct GameMetaPayload {
    pub title: String,
    pub title_id: String,
    pub content_id: String,
    pub version: String,
}

#[derive(Clone, Serialize)]
pub struct CoverPayload {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Serialize)]
pub struct GameMetaResponse {
    pub meta: Option<GameMetaPayload>,
    pub cover: Option<CoverPayload>,
}

fn map_meta(meta: GameMeta) -> GameMetaPayload {
    GameMetaPayload {
        title: meta.title,
        title_id: meta.title_id,
        content_id: meta.content_id,
        version: meta.version,
    }
}

fn map_cover(cover: CoverImage) -> CoverPayload {
    CoverPayload {
        pixels: cover.pixels,
        width: cover.width,
        height: cover.height,
    }
}

fn build_response(meta: Option<GameMeta>, cover: Option<CoverImage>) -> GameMetaResponse {
    GameMetaResponse {
        meta: meta.map(map_meta),
        cover: cover.map(map_cover),
    }
}

#[tauri::command]
pub fn game_meta_load(path: String) -> GameMetaResponse {
    let (meta, cover) = load_game_meta_for_path(&path);
    build_response(meta, cover)
}

#[tauri::command]
pub fn manage_rar_metadata(ip: String, path: String, _app: AppHandle) -> Result<GameMetaResponse, String> {
    if ip.trim().is_empty() {
        return Err("Enter a PS5 address first.".to_string());
    }
    if path.trim().is_empty() {
        return Err("Select a file first.".to_string());
    }
    tauri::async_runtime::block_on(async {
        let (param, cover_bytes) = probe_rar_metadata(&ip, TRANSFER_PORT, &path)
            .await
            .map_err(|err| err.to_string())?;
        let meta = param
            .as_deref()
            .and_then(parse_game_meta_from_param_bytes);
        let cover = cover_bytes
            .as_deref()
            .and_then(|bytes| load_cover_image_from_bytes(bytes, 160));
        Ok(build_response(meta, cover))
    })
}
