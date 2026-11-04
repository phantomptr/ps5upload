//! Which third-party tools answer on a PS5. See `ps5upload_core::companions`.

use ps5upload_core::companions::{probe_companions, CompanionStatus};

#[tauri::command]
pub async fn companion_probe(host: String) -> Result<Vec<CompanionStatus>, String> {
    tokio::task::spawn_blocking(move || probe_companions(&host))
        .await
        .map_err(|e| format!("companion probe task: {e}"))
}
