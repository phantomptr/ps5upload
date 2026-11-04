//! The Connection screen's "Find PS5s on the network". See `ps5upload_core::discover`.

use std::time::Duration;

use ps5upload_core::discover::{discover, DiscoverOptions, DiscoverResult};

/// `timeout_secs` is clamped to 1..=10; 0 or absent is the default.
#[tauri::command]
pub async fn discover_ps5(timeout_secs: Option<u64>) -> Result<DiscoverResult, String> {
    // Android filters multicast frames (224.0.0.251:5353) unless a MulticastLock is held, and
    // discovery then finds nothing by mDNS. Best effort: the LAN sweep still works without it.
    #[cfg(target_os = "android")]
    {
        if let Err(e) = crate::commands::acquire_multicast_lock().await {
            eprintln!("[discover] multicast lock acquire failed: {e}");
        }
    }

    let opts = DiscoverOptions {
        timeout: timeout_secs.map(Duration::from_secs),
        extra_subnets: Vec::new(),
    };
    let result = tokio::task::spawn_blocking(move || discover(&opts))
        .await
        .map_err(|e| format!("discover task: {e}"));

    #[cfg(target_os = "android")]
    {
        if let Err(e) = crate::commands::release_multicast_lock().await {
            eprintln!("[discover] multicast lock release failed: {e}");
        }
    }

    result
}
