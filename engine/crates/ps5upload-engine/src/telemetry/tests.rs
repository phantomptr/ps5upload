use super::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ps5up-telem-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn done_state() -> Value {
    json!({
        "status": "done", "started_at_ms": 1000, "completed_at_ms": 61000, "elapsed_ms": 60000,
        "tx_id_hex": "00", "bytes_sent": 5_000_000, "dest": "/mnt/usb0/homebrew/Game",
        "files_sent": 3, "skipped_files": 1, "skipped_bytes": 10,
        "commit_ack": {"protocol": "ava1", "files": 3, "bytes": 5_000_000},
    })
}

fn failed_state(reason: Option<&str>, error: &str) -> Value {
    let mut v = json!({
        "status": "failed", "started_at_ms": 1000, "completed_at_ms": 9000, "elapsed_ms": 8000,
        "error": error,
    });
    if let Some(r) = reason {
        v["error_reason"] = r.into();
    }
    v
}

fn ava1_notes() -> Value {
    json!({
        "console": "0123456789abcdef", "attempts": 2, "resumed": true,
        "shares": {"ticks": 10, "credit_starved_pct": 80.0, "source_starved_pct": 5.0,
                   "receiver_bound_pct": 71.0, "receiver_bottleneck": "console drive"},
        "lanes_avg": 4.0, "lanes_max": 6, "chunk_avg_kib": 4096.0,
        "history": [[1, 2, 4096], [2, 4, 4096]], "slow_drive_switch": true,
        "settle_ms": 1200, "unswept_peak": 30, "resent_bytes": 0,
        "bytes_durable": 5_000_000, "files_durable": 3, "skipped_files": 1, "skipped_bytes": 10,
        "console_line": "apply: 3 files",
    })
}

#[test]
fn a_successful_job_has_a_complete_record() {
    let id = Uuid::new_v4();
    let r = build_record(id, "dir", &done_state(), Some(&ava1_notes()), 99);
    assert_eq!(r["schema"], 1);
    assert_eq!(r["type"], "job_summary");
    assert_eq!(r["job_id"], id.to_string());
    assert_eq!(r["kind"], "dir");
    assert_eq!(r["result"], "done");
    assert_eq!(r["code"], Value::Null);
    assert_eq!(
        (r["files"].as_u64(), r["bytes"].as_u64()),
        (Some(3), Some(5_000_000))
    );
    assert_eq!(r["skipped_files"], 1);
    assert_eq!(r["resumed"], true);
    assert_eq!(r["drive"], "/mnt/usb0");
    assert_eq!(r["console"], "0123456789abcdef");
    assert_eq!(
        (r["started_at_ms"].as_u64(), r["ended_at_ms"].as_u64()),
        (Some(1000), Some(61000))
    );
    assert_eq!(r["console_line"], "apply: 3 files");
    assert_eq!(r["slow_drive_switch"], true);
    assert_eq!(r["settle_ms"], 1200);
    assert_eq!(r["unswept_peak"], 30);
    assert_eq!(r["engine_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(r["why"]["dominant"], "receiver_bound");
}

#[test]
fn a_cancelled_job_is_recorded_as_cancelled() {
    let r = build_record(
        Uuid::new_v4(),
        "file",
        &failed_state(None, "transfer_cancelled"),
        None,
        5,
    );
    assert_eq!(r["result"], "cancelled");
    assert_eq!(r["code"], "transfer_cancelled");
    assert_eq!(r["why"]["dominant"], "unmeasured");
}

#[test]
fn a_stalled_job_carries_the_stall_code() {
    let r = build_record(
        Uuid::new_v4(),
        "file",
        &failed_state(None, "no durable progress for 60s; giving up"),
        Some(&ava1_notes()),
        5,
    );
    assert_eq!(r["result"], "failed");
    assert_eq!(r["code"], "ava1_stalled");
    assert!(r["message"]
        .as_str()
        .unwrap()
        .contains("no durable progress"));
}

#[test]
fn a_cross_device_refusal_carries_its_code_and_the_console_message() {
    let mut s = failed_state(Some("ava1_commit_cross_device"), "x");
    s["error_detail"] = "the destination is on another storage device".into();
    let r = build_record(Uuid::new_v4(), "dir", &s, None, 5);
    assert_eq!(r["code"], "ava1_commit_cross_device");
    assert_eq!(r["message"], "the destination is on another storage device");
}

#[test]
fn the_record_holds_no_home_path_no_address_and_no_source_path() {
    let mut s = failed_state(
        None,
        "open 7z /Users/yunpengl/Games/Secret Game.7z: connect 192.168.86.99:9021 refused; also C:\\Users\\bob\\x.zip and /home/alice/y",
    );
    s["dest"] = "/data/homebrew/My Private Game Folder".into();
    s["src"] = "/Users/yunpengl/Games/Secret".into();
    let mut notes = ava1_notes();
    notes["console_line"] = "from 10.0.0.5 saved /Users/yunpengl/x".into();
    let r = build_record(Uuid::new_v4(), "7z", &s, Some(&notes), 5);
    let text = serde_json::to_string(&r).unwrap();
    for banned in [
        "yunpengl",
        "/Users",
        "/home",
        "bob",
        "alice",
        "192.168",
        "10.0.0.5",
        "Secret",
        "Private Game",
    ] {
        assert!(!text.contains(banned), "{banned} leaked: {text}");
    }
    assert_eq!(r["drive"], "/data");
}

#[test]
fn scrub_removes_the_home_folder_and_addresses_and_keeps_the_rest() {
    let s = scrub_with(
        "read /Users/me/a/My File.bin; copy /opt/x from 1.2.3.4:80",
        "/Users/me",
    );
    assert_eq!(s, "read <local path>; copy /opt/x from <ip>:80");
    assert_eq!(
        scrub_with("D:\\Users\\Me\\a.zip: bad", ""),
        "<local path>: bad"
    );
    assert_eq!(
        scrub_with("v1.2.3 and 1.2.3.4.5 stay", ""),
        "v1.2.3 and 1.2.3.4.5 stay"
    );
    assert_eq!(scrub_with("999.1.1.1 stays", ""), "999.1.1.1 stays");
    assert_eq!(
        scrub_with("in /srv/me/data: failed", "/srv/me"),
        "in <local path>: failed"
    );
    assert_eq!(
        scrub_with("C:\\Users\\bob", "C:\\Users\\bob"),
        "<local path>"
    );
    assert_eq!(
        scrub_with("open \"/home/al/x y\" failed", ""),
        "open \"<local path>\" failed"
    );
    assert!(scrub_with(&"x".repeat(5000), "").chars().count() <= MAX_TEXT + 1);
}

#[test]
fn interpretation_for_each_dominant_share() {
    let shares = |credit: f64, source: f64, receiver: f64, bn: &str| {
        json!({"ticks": 20, "credit_starved_pct": credit, "source_starved_pct": source,
               "receiver_bound_pct": receiver, "receiver_bottleneck": bn})
    };
    let none = json!({});
    let r = interpret(
        &shares(75.0, 5.0, 71.0, "console drive"),
        &json!({"slow_drive_switch": true}),
        "done",
    );
    assert_eq!(r["dominant"], "receiver_bound");
    assert_eq!(r["pct"], 71.0);
    let t = r["text"].as_str().unwrap();
    assert!(t.starts_with("receiver-bound 71 %"), "{t}");
    assert!(t.contains("drive") && t.contains("sequential"), "{t}");
    let r = interpret(&shares(60.0, 0.0, 60.0, "console workers"), &none, "done");
    assert!(r["text"].as_str().unwrap().contains("workers"));
    let r = interpret(&shares(10.0, 64.0, 0.0, "none"), &none, "done");
    assert_eq!(r["dominant"], "source_starved");
    assert!(r["text"]
        .as_str()
        .unwrap()
        .starts_with("source-starved 64 %"));
    // Credit starvation counts only what is not already receiver-bound.
    let r = interpret(&shares(55.0, 5.0, 5.0, "none"), &none, "done");
    assert_eq!(r["dominant"], "credit_starved");
    assert_eq!(r["pct"], 50.0);
    assert!(r["text"]
        .as_str()
        .unwrap()
        .starts_with("credit-starved 50 %"));
    // Not the console's doing (it reported no limit of its own): the link is named.
    assert!(r["text"].as_str().unwrap().contains("network link"));
    assert!(!r["text"].as_str().unwrap().contains("memory"));
    let r = interpret(&shares(10.0, 10.0, 0.0, "none"), &none, "done");
    assert_eq!(r["dominant"], "network");
    let r = interpret(&json!({"ticks": 0}), &none, "failed");
    assert_eq!(r["dominant"], "unmeasured");
    assert_eq!(
        interpret(&Value::Null, &none, "done")["dominant"],
        "unmeasured"
    );
}

#[test]
fn rotation_keeps_the_newest_two_hundred() {
    let dir = tmp("rotate");
    let mut ids = Vec::new();
    for i in 0..(MAX_SUMMARIES + 5) {
        let id = Uuid::new_v4();
        write_record(&dir, id, &json!({"n": i})).unwrap();
        // Distinct mtimes without sleeping: set them explicitly.
        let f = std::fs::File::options()
            .write(true)
            .open(dir.join(format!("{id}.json")))
            .unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000 + i as u64))
            .unwrap();
        ids.push(id);
    }
    rotate(&dir, MAX_SUMMARIES);
    let left = record_files(&dir);
    assert_eq!(left.len(), MAX_SUMMARIES);
    for id in &ids[..5] {
        assert!(read_record(&dir, *id).is_none(), "the oldest are gone");
    }
    assert!(read_record(&dir, ids[5]).is_some());
    assert!(read_record(&dir, *ids.last().unwrap()).is_some());
    // Stray files in the folder are never counted or removed.
    std::fs::write(dir.join("notes.txt"), b"x").unwrap();
    rotate(&dir, 1);
    assert!(dir.join("notes.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_is_newest_first_and_limited() {
    let dir = tmp("list");
    for i in 0..5u64 {
        let id = Uuid::new_v4();
        write_record(&dir, id, &json!({"n": i})).unwrap();
        let f = std::fs::File::options()
            .write(true)
            .open(dir.join(format!("{id}.json")))
            .unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(100 + i))
            .unwrap();
    }
    let got: Vec<u64> = list_records(&dir, 3)
        .iter()
        .map(|v| v["n"].as_u64().unwrap())
        .collect();
    assert_eq!(got, [4, 3, 2]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_tagged_jobs_end_writes_its_record_once_and_untagged_jobs_write_nothing() {
    let dir = tmp("onstate");
    with_test_dir(&dir, || {
        let id = Uuid::new_v4();
        tag(id, "file");
        let notes = Arc::new(LiveNotes::default());
        *notes.telemetry.lock().unwrap() = Some(ava1_notes());
        hold_notes(id, notes);
        on_state(id, &json!({"status": "running"}));
        assert!(
            read_record(&dir, id).is_none(),
            "a running job has no record"
        );
        on_state(id, &done_state());
        let r = read_record(&dir, id).expect("written at the end");
        assert_eq!(r["kind"], "file");
        assert_eq!(r["console"], "0123456789abcdef");
        // A second terminal write (a panic guard after success) does not rewrite it.
        std::fs::remove_file(dir.join(format!("{id}.json"))).unwrap();
        on_state(id, &failed_state(None, "late"));
        assert!(read_record(&dir, id).is_none());
        let other = Uuid::new_v4();
        on_state(other, &done_state());
        assert!(read_record(&dir, other).is_none(), "untagged: no record");
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_api_returns_one_summary_and_the_newest_list() {
    use axum::response::IntoResponse;
    let dir = tmp("api");
    let id = Uuid::new_v4();
    let rec = build_record(id, "file", &done_state(), Some(&ava1_notes()), 5);
    write_record(&dir, id, &rec).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    with_test_dir(&dir, || {
        rt.block_on(async {
            let resp = summary_handler(AxumPath(id.to_string()))
                .await
                .into_response();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            let v: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["job_id"], id.to_string());
            let resp = summary_handler(AxumPath(Uuid::new_v4().to_string()))
                .await
                .into_response();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
            let resp = summary_handler(AxumPath("nope".into()))
                .await
                .into_response();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
            let resp = summaries_handler(Query(ListQuery { limit: Some(5) }))
                .await
                .into_response();
            let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            let v: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["summaries"].as_array().unwrap().len(), 1);
        })
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn metrics_render_prometheus_text() {
    let m = Metrics::default();
    m.record("dir", &json!({"result": "done", "bytes": 100}));
    m.record("dir", &json!({"result": "done", "bytes": 50}));
    m.record(
        "file",
        &json!({"result": "failed", "code": "ava1_stalled", "bytes": 0}),
    );
    m.record("file", &json!({"result": "cancelled"}));
    let t = m.render();
    assert!(t.contains("# TYPE ps5upload_jobs_total counter"), "{t}");
    assert!(
        t.contains("ps5upload_jobs_total{kind=\"dir\",result=\"done\"} 2"),
        "{t}"
    );
    assert!(
        t.contains("ps5upload_jobs_total{kind=\"file\",result=\"failed\"} 1"),
        "{t}"
    );
    assert!(
        t.contains("ps5upload_jobs_total{kind=\"file\",result=\"cancelled\"} 1"),
        "{t}"
    );
    assert!(t.contains("ps5upload_job_bytes_total 150"), "{t}");
    assert!(t.contains("ps5upload_job_stalls_total 1"), "{t}");
    assert!(t
        .lines()
        .all(|l| l.is_empty() || l.starts_with('#') || l.contains(' ')));
}
