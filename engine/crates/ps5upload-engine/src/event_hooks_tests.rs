//! What the engine journals for a finished job (bug-report spec §1.2).
use crate::job_event;
use ps5upload_core::events::{Cat, Level};

#[test]
fn failed_job_maps_to_error_event_with_console() {
    let st = serde_json::json!({
        "status": "failed",
        "error": "session ended",
        "error_console": "10.0.0.9:9120",
        "elapsed_ms": 5
    });
    let e = job_event("abc", &st).unwrap();
    assert_eq!(
        (e.cat, e.level, e.code.as_deref()),
        (Cat::Transfer, Level::Error, Some("job_failed"))
    );
    assert_eq!(e.console.as_deref(), Some("10.0.0.9"));
    assert!(e.msg.contains("session ended"));
}

#[test]
fn done_job_maps_to_info_event() {
    let e = job_event("abc", &serde_json::json!({"status": "done"})).unwrap();
    assert_eq!(
        (e.level, e.code.as_deref()),
        (Level::Info, Some("job_done"))
    );
}

#[test]
fn running_job_maps_to_nothing() {
    assert!(job_event("abc", &serde_json::json!({"status": "running"})).is_none());
}
