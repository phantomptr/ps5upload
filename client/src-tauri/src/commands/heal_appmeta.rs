//! Restore a title's missing `/user/appmeta/<TITLE_ID>/` files. See
//! `ps5upload_core::appmeta_heal`.

use ps5upload_core::appmeta_heal::{heal, HealResult};

#[tauri::command]
pub async fn heal_appmeta(
    addr: String,
    title_id: String,
    source_path: String,
) -> Result<HealResult, String> {
    tokio::task::spawn_blocking(move || heal(&addr, &title_id, &source_path))
        .await
        .map_err(|e| format!("heal_appmeta task: {e}"))?
}
