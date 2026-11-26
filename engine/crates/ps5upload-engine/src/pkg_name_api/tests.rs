//! The package name routes on a real package: built here, renamed through the router, read back
//! by the full verifier; refused while in use; put back when the check fails or the engine dies.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ps5_dump_forge_patch as patch;
use ps5upload_fpkg::build::{self, BuildRequest};
use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::*;
use crate::image_edit_in_place::{Claim, Reading};

const NAME: &str = "A Game-example.invalid-abc";

/// A folder of its own, removed when dropped (a failed test too).
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = crate::test_tmp::temp_path(format!(
            "ps5upload-pkg-name-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A small PS5 fake package named [`NAME`] in two languages, with a third that has its own.
fn package(dir: &Path) -> PathBuf {
    let source = dir.join("game");
    let put = |path: &str, data: &[u8]| {
        let full = source.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, data).unwrap();
    };
    let param = json!({
        "applicationCategoryType": 0,
        "attributePub": 0,
        "contentId": "UP0000-PPSA01234_00-TESTGAME00000000",
        "contentVersion": "01.001.000",
        "localizedParameters": {
            "defaultLanguage": "en-US",
            "en-US": { "titleName": NAME },
            "fr-FR": { "titleName": NAME },
            "ja-JP": { "titleName": "テストゲーム" }
        },
        "titleId": "PPSA01234"
    });
    put("eboot.bin", &[7u8; 4096]);
    put("data/file.bin", &vec![0xAB; 200 * 1024]);
    put(
        "sce_sys/param.json",
        serde_json::to_string_pretty(&param).unwrap().as_bytes(),
    );
    put("sce_sys/icon0.png", &[0x89; 2048]);
    put("sce_sys/icon0.dds", &[0x44; 4096]);
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let request = BuildRequest {
        time: Some((1_700_000_000, 0)),
        ..BuildRequest::new(&source, &out)
    };
    build::build(&request, &mut |_| {}).unwrap().path
}

fn state() -> PkgInstallStateHandle {
    Arc::new(PkgInstallState::default())
}

fn query(path: &Path) -> String {
    let mut out = String::from("/api/pkg/name?path=");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

async fn answer(request: Request<Body>) -> (StatusCode, Value) {
    let resp = crate::pkg_install::router(state())
        .oneshot(request)
        .await
        .unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

async fn get(path: &Path) -> (StatusCode, Value) {
    answer(
        Request::builder()
            .uri(query(path))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn post(body: Value) -> (StatusCode, Value) {
    answer(
        Request::builder()
            .method("POST")
            .uri("/api/pkg/name")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

fn no_journal(pkg: &Path) {
    assert!(
        !patch::journal_path(pkg).exists(),
        "the undo journal is gone"
    );
    assert!(!patch::identity_path(pkg).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_is_renamed_through_the_routes() {
    let dir = TempDir::new("routes");
    let pkg = package(&dir.0);
    let len = std::fs::metadata(&pkg).unwrap().len();

    let (status, names) = get(&pkg).await;
    assert_eq!(status, StatusCode::OK, "{names}");
    assert_eq!(names["platform"], "ps5");
    assert_eq!(names["name"], NAME);
    assert_eq!(names["refusal"], Value::Null);
    assert_eq!(names["other_names"], 1);
    assert_eq!(names["max_bytes"], 127);
    assert_eq!(names["entries"].as_array().unwrap().len(), 3);
    // The package's own room for a new name, for the field to count against before it asks.
    let room = |which: &str| names["room"][which].as_u64().expect("the room is a number");
    assert!(room("default_name") >= NAME.len() as u64 && room("default_name") <= 127);
    assert!(room("all_languages") >= 1 && room("all_languages") <= room("default_name"));

    let path = pkg.to_string_lossy().into_owned();
    let (status, done) = post(json!({ "path": path, "name": " A Game " })).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["previous"], NAME);
    assert_eq!(done["name"], "A Game");
    assert_eq!(done["changed"], 2);
    assert!(done["written_bytes"].as_u64().unwrap() > 0);
    assert_eq!(done.get("warning"), None);
    no_journal(&pkg);
    assert_eq!(std::fs::metadata(&pkg).unwrap().len(), len);

    let (_, names) = get(&pkg).await;
    assert_eq!(names["name"], "A Game");
    assert_eq!(names["other_names"], 1, "ja-JP keeps its own");
    let report =
        ps5upload_fpkg::verify::verify_streaming(&pkg, DEFAULT_PASSCODE, &mut |_, _| {}).unwrap();
    assert!(report.ok(), "{report}");
    assert_eq!(ps5upload_pkg::parse_pkg(&pkg).unwrap().title, "A Game");

    // The same name again changes nothing; every language on request.
    let (status, done) = post(json!({ "path": path, "name": "A Game" })).await;
    assert_eq!((status, &done["changed"]), (StatusCode::OK, &json!(0)));
    assert_eq!(done["written_bytes"], 0);
    let (status, done) =
        post(json!({ "path": path, "name": "Every Language", "all_languages": true })).await;
    assert_eq!((status, &done["changed"]), (StatusCode::OK, &json!(3)));
    let (_, names) = get(&pkg).await;
    assert_eq!(names["other_names"], 0);
    no_journal(&pkg);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_in_use_is_not_renamed() {
    let dir = TempDir::new("busy");
    let pkg = package(&dir.0);
    let before = std::fs::read(&pkg).unwrap();
    let body = json!({ "path": pkg.to_string_lossy(), "name": "Other" });

    // A build or an upload reads it.
    let reading = Reading::take(&pkg).unwrap();
    let (status, why) = post(body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{why}");
    assert!(why["error"].as_str().unwrap().contains("reading"), "{why}");
    drop(reading);

    // Another change of it runs: neither read nor changed meanwhile.
    let claim = Claim::take(&pkg).unwrap();
    assert_eq!(post(body.clone()).await.0, StatusCode::CONFLICT);
    assert_eq!(get(&pkg).await.0, StatusCode::CONFLICT);
    drop(claim);

    // Another program holds it open and locked.
    let held = std::fs::File::open(&pkg).unwrap();
    held.lock().unwrap();
    let (status, why) = post(body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{why}");
    assert!(
        why["error"]
            .as_str()
            .unwrap()
            .contains("open in another program"),
        "{why}"
    );
    drop(held);

    assert_eq!(std::fs::read(&pkg).unwrap(), before, "never touched");
    no_journal(&pkg);
    assert_eq!(post(body).await.0, StatusCode::OK, "free again");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_install_served_from_the_package_refuses_the_rename() {
    let dir = TempDir::new("served");
    let pkg = package(&dir.0);
    let state = state();
    let request = RenameRequest {
        path: pkg.to_string_lossy().into_owned(),
        name: "Other".into(),
        all_languages: false,
    };
    let len = std::fs::metadata(&pkg).unwrap().len();
    let session = crate::pkg_install::tests::dummy_session(vec![(pkg.clone(), len)]);
    state.sessions.lock().unwrap().insert("s1".into(), session);
    let (code, why) = rename(&state, &request).unwrap_err();
    assert_eq!(code, StatusCode::CONFLICT);
    assert!(why.contains("an install of this package"), "{why}");
    no_journal(&pkg);
    // Quiet for an hour and not ended (a download the PS5 holds or paused): still refused, so a
    // resumed install never reads renamed bytes. It used to be free after 90 seconds.
    {
        let mut sessions = state.sessions.lock().unwrap();
        let s = sessions.get_mut("s1").unwrap();
        let hour_ago = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 3600;
        s.created_at_unix = hour_ago;
        s.last_activity_unix = hour_ago;
    }
    let (code, why) = rename(&state, &request).unwrap_err();
    assert_eq!(code, StatusCode::CONFLICT);
    assert!(why.contains("remove it from the queue"), "{why}");
    // Once the install has ended the package is free.
    state
        .sessions
        .lock()
        .unwrap()
        .get_mut("s1")
        .unwrap()
        .cancelled = true;
    assert_eq!(rename(&state, &request).unwrap().name, "Other");
}

#[test]
fn a_change_that_does_not_check_out_is_undone() {
    let dir = TempDir::new("undone");
    let pkg = package(&dir.0);
    let before = std::fs::read(&pkg).unwrap();
    let failed = crate::image_edit_in_place::patch_checked(
        &pkg,
        || {
            let plan = pkg_name::plan(&pkg, "Other", false).unwrap();
            let writes = plan
                .patches
                .iter()
                .map(|p| Write {
                    offset: p.offset,
                    data: Data::Bytes(p.data.clone()),
                    live: true,
                })
                .collect();
            Ok((writes, plan))
        },
        |plan| {
            // The change is on disk when the check runs.
            assert_eq!(pkg_name::read(&pkg).unwrap().name, "Other");
            pkg_name::check(&pkg, plan).unwrap();
            Err("the check says no".to_string())
        },
    );
    let Err(refusal) = failed else {
        panic!("a change that does not check out is not kept");
    };
    assert!(!refusal.busy);
    assert!(
        refusal.reason.contains("the check says no")
            && refusal.reason.contains("put back as it was"),
        "{}",
        refusal.reason
    );
    assert_eq!(std::fs::read(&pkg).unwrap(), before);
    no_journal(&pkg);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_the_engine_died_in_is_undone_on_the_next_open() {
    let dir = TempDir::new("crash");
    let pkg = package(&dir.0);
    let before = std::fs::read(&pkg).unwrap();
    let plan = pkg_name::plan(&pkg, "Other", false).unwrap();
    let writes: Vec<Write> = plan
        .patches
        .iter()
        .map(|p| Write {
            offset: p.offset,
            data: Data::Bytes(p.data.clone()),
            live: true,
        })
        .collect();
    {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pkg)
            .unwrap();
        // Written and synced, then the engine "dies": nothing commits or rolls back.
        let applied = patch::apply(
            &mut file,
            &pkg,
            &writes,
            &mut |_, _, _| Ok(()),
            &AtomicBool::new(false),
            patch::Syncs::STD,
        )
        .unwrap();
        drop(applied);
    }
    assert!(patch::journal_path(&pkg).exists());
    assert_ne!(std::fs::read(&pkg).unwrap(), before);

    let (status, names) = get(&pkg).await;
    assert_eq!(status, StatusCode::OK, "{names}");
    assert_eq!(names["name"], NAME, "as it was before the rename");
    assert_eq!(std::fs::read(&pkg).unwrap(), before);
    no_journal(&pkg);
}

#[tokio::test(flavor = "multi_thread")]
async fn what_can_not_be_renamed_says_why() {
    let dir = TempDir::new("refused");
    let pkg = package(&dir.0);
    let folder = dir.0.to_string_lossy().into_owned();
    for (path, why) in [
        (
            "remote://nas/games/a.pkg".to_string(),
            "has to be on this computer",
        ),
        (
            "ps5:///data/a.pkg".to_string(),
            "has to be on this computer",
        ),
        (format!("{folder}/out/../out/a.pkg"), "must not hold"),
        (format!("{folder}/game/eboot.bin"), "only a .pkg or .fpkg"),
        (format!("{folder}/missing.pkg"), "missing.pkg"),
        ("  ".to_string(), "no package was named"),
    ] {
        let (status, body) = post(json!({ "path": path, "name": "X" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert!(
            body["error"].as_str().unwrap().contains(why),
            "{path}: {body}"
        );
        assert_eq!(get(Path::new(&path)).await.0, StatusCode::BAD_REQUEST);
    }
    // A folder named like a package.
    let fake = dir.0.join("folder.pkg");
    std::fs::create_dir_all(&fake).unwrap();
    let (status, body) = get(&fake).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("is not a file"));

    // A name that can't be stored.
    let path = pkg.to_string_lossy().into_owned();
    let (status, body) = post(json!({ "path": path, "name": "two\nlines" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("control character"));

    // An official package: its name is shown as refused, and a rename says why.
    let mut bytes = std::fs::read(&pkg).unwrap();
    bytes[5] = 0x80;
    std::fs::write(&pkg, &bytes).unwrap();
    let (status, names) = get(&pkg).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names["refusal"]["code"], "retail");
    let (status, body) = post(json!({ "path": path, "name": "X" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("official package"),
        "{body}"
    );
    assert_eq!(std::fs::read(&pkg).unwrap(), bytes);
    no_journal(&pkg);
}
