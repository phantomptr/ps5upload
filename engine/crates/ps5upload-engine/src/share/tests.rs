//! Share end to end, in one process: the engine-side refusals with the page bypassed, the
//! protocol over an in-memory pair, and two engines' sides over a real WebRTC data channel on
//! loopback with a fake hub (as join.rs's internet tests do). No real hub, STUN server or PS5.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::to_bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde_json::{json, Value};

use super::manifest::tests::tmp;
use super::*;

async fn body(r: Response) -> (StatusCode, Value) {
    let s = r.status();
    let b = to_bytes(r.into_body(), 1 << 20).await.unwrap();
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

/// An app folder whose param.json says `title_id` / `content_id`, built as homebrew is.
fn app_folder(title_id: &str, content_id: &str) -> PathBuf {
    app_with_param(json!({
        "titleId": title_id,
        "contentId": content_id,
        "contentVersion": "01.000.000",
        "sdkVersion": "0x0000000000000000",
        "requiredSystemSoftwareVersion": "0x0000000000000000",
        "versionFileUri": "",
    }))
}

/// An app folder with this param.json.
fn app_with_param(param: Value) -> PathBuf {
    let d = tmp("app");
    std::fs::create_dir_all(d.join("sce_sys")).unwrap();
    std::fs::write(d.join("sce_sys/param.json"), param.to_string()).unwrap();
    std::fs::write(d.join("eboot.bin"), vec![7u8; 300_000]).unwrap();
    std::fs::create_dir_all(d.join("assets/deep")).unwrap();
    std::fs::write(
        d.join("assets/deep/data.bin"),
        (0..200_000u32).map(|i| i as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    d
}

/// A PS4 app folder with this param.sfo.
fn ps4_folder(sfo: Vec<u8>) -> PathBuf {
    let d = tmp("ps4app");
    std::fs::create_dir_all(d.join("sce_sys")).unwrap();
    std::fs::write(d.join("sce_sys/param.sfo"), sfo).unwrap();
    std::fs::write(d.join("eboot.bin"), vec![7u8; 1000]).unwrap();
    d
}

fn create_body(items: Value) -> ShareCreateBody {
    serde_json::from_value(json!({"from": "Sam", "title": "My App", "items": items})).unwrap()
}

/// The hub settings are one for the whole process: a test that needs them holds this while it
/// runs, so another test can't switch them under it.
static HUB: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Holds [`HUB`] with these settings.
async fn test_hub(url: &str, internet: bool) -> tokio::sync::MutexGuard<'static, ()> {
    let g = HUB.lock().await;
    crate::join::set_test_hub(url, internet);
    g
}

async fn make(items: Value) -> (StatusCode, Value) {
    body(create(HeaderMap::new(), Json(create_body(items))).await).await
}

#[tokio::test]
async fn a_retail_app_is_refused_by_the_engine_whatever_the_page_sent() {
    use super::retail::tests as rt;
    let _hub = test_hub("ws://127.0.0.1:9/ws", false).await;
    // ASTRO BOT's own param.json: built by the store. The page says nothing about it.
    let retail = app_with_param(rt::astro_bot());
    let (s, v) = make(json!([{"kind": "app", "path": retail}])).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["code"], "retail");
    assert_eq!(v["title_id"], "PPSA21564");
    assert_eq!(
        v["error"],
        "This is a retail game (PPSA21564). PS5Upload only shares homebrew."
    );
    // The page claims a homebrew Title ID: the app's own build is what counts.
    let body: ShareCreateBody = serde_json::from_value(json!({
        "title_id": "PSUP00001",
        "items": [{"kind": "app", "path": retail}],
    }))
    .unwrap();
    let (s, _) = body_of(create(HeaderMap::new(), Json(body)).await).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Free retail (YouTube): refused.
    let yt = app_with_param(rt::youtube());
    let (s, _) = make(json!([{"kind": "app", "path": yt}])).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A PS4 retail game and its update, as folders.
    let (s, _) = make(json!([{"kind": "app", "path": ps4_folder(rt::ps4_retail())}])).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // An app with no param at all.
    let bare = tmp("bare");
    std::fs::write(bare.join("eboot.bin"), b"x").unwrap();
    let (s, v) = make(json!([{"kind": "app", "path": bare}])).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(
        v["error"],
        "PS5Upload can't confirm this is homebrew, so it can't share it."
    );
    // An archive can't be checked.
    let zip = tmp("zip").join("game.zip");
    std::fs::write(&zip, b"PK").unwrap();
    let (s, _) = make(json!([{"kind": "app", "path": zip}])).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // A path that can't be read: no build information.
    let body: ShareCreateBody = serde_json::from_value(json!({
        "title_id": "FAKE77031",
        "items": [{"kind": "app", "path": "/nonexistent/app"}],
    }))
    .unwrap();
    let (s, _) = body_of(create(HeaderMap::new(), Json(body)).await).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // The check route says the same, and makes nothing.
    let (_, v) = body_of(
        check(Json(create_body(json!([
            {"kind": "app", "path": retail},
            {"kind": "note", "text": "hi"},
            {"kind": "app"},
        ]))))
        .await,
    )
    .await;
    assert_eq!(v["items"][0]["ok"], false);
    assert_eq!(v["items"][0]["code"], "retail");
    assert_eq!(v["items"][1]["ok"], true);
    assert_eq!(v["items"][2]["ok"], false);
    // Homebrew that borrows a store's Title ID or publisher code is shared: Playden,
    // ProsperoStore, a CUSA-numbered PS4 homebrew.
    let borrowed_ps4 = rt::sfo(&[
        (
            "CONTENT_ID",
            rt::SfoField::Text("UP0001-CUSA99999_00-HOMEBREWAPP00000"),
        ),
        ("SYSTEM_VER", rt::SfoField::Int(0)),
        ("TITLE_ID", rt::SfoField::Text("CUSA99999")),
    ]);
    for (app, tid) in [
        (app_with_param(rt::playden()), "FAKE77031"),
        (
            app_with_param(rt::prospero_store("PPSA99000", "PROSPEROSTORE000")),
            "PPSA99000",
        ),
        (ps4_folder(rt::ps4_homebrew()), "BREW00083"),
        (ps4_folder(borrowed_ps4), "CUSA99999"),
    ] {
        let body: ShareCreateBody = serde_json::from_value(json!({
            "title_id": tid,
            "items": [{"kind": "app", "path": app}],
        }))
        .unwrap();
        let (s, v) = body_of(create(HeaderMap::new(), Json(body)).await).await;
        assert_eq!(s, StatusCode::OK, "{tid}: {v}");
        assert_eq!(v["items"][0]["title_id"], tid);
        assert_eq!(v["items"][0]["build"]["read"], true);
        assert_eq!(v["items"][0]["build"]["marks"], json!([]));
        let _ = cancel(axum::extract::Path(v["id"].as_str().unwrap().to_string())).await;
    }
    // A homebrew app is made into a share, with its files.
    let hb = app_folder("PSUP00001", "IV0000-PSUP00001_00-HOMEBREW00000000");
    let (s, v) = make(json!([{"kind": "app", "path": hb}])).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["items"][0]["title_id"], "PSUP00001");
    assert_eq!(v["items"][0]["files"], 3);
    let _ = cancel(axum::extract::Path(v["id"].as_str().unwrap().to_string())).await;
    // A save of a retail game is allowed: player data.
    let save = tmp("save").join("PPSA01234 save shared.zip");
    std::fs::write(&save, b"PK zip").unwrap();
    let body: ShareCreateBody = serde_json::from_value(json!({
        "title_id": "PPSA01234",
        "items": [{"kind": "save", "path": save}],
    }))
    .unwrap();
    let (s, v) = body_of(create(HeaderMap::new(), Json(body)).await).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let _ = cancel(axum::extract::Path(v["id"].as_str().unwrap().to_string())).await;
}

/// Packages and images in the Collection are judged by the PARAM inside them.
#[test]
fn a_package_is_judged_by_its_param() {
    use super::retail::tests as rt;
    let dir = crate::inspect::test_fixtures::scratch();
    let retail =
        crate::inspect::test_fixtures::write_cnt_pkg_sfo(dir.join("r.pkg"), &rt::ps4_retail());
    let b = local_build(&retail, "pkg");
    assert!(b.read);
    assert!(check_app(&b).is_err());
    assert_eq!(b.title_id, "CUSA00341");
    let hb =
        crate::inspect::test_fixtures::write_cnt_pkg_sfo(dir.join("h.pkg"), &rt::ps4_homebrew());
    assert!(check_app(&local_build(&hb, "pkg")).is_ok());
    // A PS5 package whose PARAM can't be read: refused, can't tell.
    let fih = crate::inspect::test_fixtures::write_fih_without_param();
    let r = check_app(&local_build(&fih, "pkg")).unwrap_err();
    assert_eq!(
        r.message,
        "PS5Upload can't confirm this is homebrew, so it can't share it."
    );
}

/// The receiving side reads the sender's build verdict; an older sender's offer has none.
#[test]
fn the_receiver_checks_the_offered_build() {
    let items = wire::offered(&json!({"items": [
        {"id": "i1", "kind": "app", "name": "A", "title_id": "PPSA21564",
         "build": {"read": true, "marks": ["sdkVersion"]}},
        {"id": "i2", "kind": "app", "name": "B", "title_id": "FAKE77031",
         "build": {"read": true, "marks": []}},
        {"id": "i3", "kind": "app", "name": "C", "title_id": "PPSA99000"},
        {"id": "i4", "kind": "app", "name": "D", "build": {"read": false, "marks": []}},
        {"id": "i5", "kind": "save", "name": "E", "title_id": "PPSA21564"},
    ]}));
    assert_eq!(
        items[0].refused.as_deref(),
        Some("This is a retail game (PPSA21564). PS5Upload only shares homebrew.")
    );
    assert_eq!(items[1].refused, None);
    assert_eq!(items[2].refused, None);
    assert!(items[3]
        .refused
        .as_deref()
        .unwrap()
        .contains("can't confirm"));
    assert_eq!(items[4].refused, None);
}

async fn body_of(r: Response) -> (StatusCode, Value) {
    body(r).await
}

/// Waits until the receive's state is `want` (10 s).
async fn until_rx(sh: &Arc<Mutex<RxShared>>, want: &str) {
    for _ in 0..400 {
        if sh.lock().unwrap().state == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("receive never got to {want}: {:?}", sh.lock().unwrap());
}

/// Waits until share `id` has a request in `state`; its id.
async fn until_request(id: &str, state: &str) -> u64 {
    for _ in 0..400 {
        if let Some(r) = with_share(id, |e| {
            e.m.requests
                .iter()
                .find(|r| r.state.word() == state)
                .map(|r| r.rid)
        })
        .flatten()
        {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no request {state} on {id}");
}

async fn decide_on(id: &str, rid: u64, allow: bool) -> StatusCode {
    decide(
        axum::extract::Path(id.to_string()),
        Json(ShareDecideBody {
            request: rid,
            allow,
        }),
    )
    .await
    .status()
}

fn same_tree(a: &Path, b: &Path) {
    let (fa, _) = manifest::walk_local(a).unwrap();
    let (fb, _) = manifest::walk_local(b).unwrap();
    assert_eq!(
        fa.iter().map(|f| (&f.path, f.size)).collect::<Vec<_>>(),
        fb.iter().map(|f| (&f.path, f.size)).collect::<Vec<_>>()
    );
    for f in &fa {
        assert_eq!(
            std::fs::read(a.join(&f.path)).unwrap(),
            std::fs::read(b.join(&f.path)).unwrap(),
            "{}",
            f.path
        );
    }
}

/// A share made directly in the registry (no hub), its room.
async fn share_of(items: Value, one_use: bool) -> (String, String) {
    let _hub = test_hub("ws://127.0.0.1:9/ws", false).await;
    let mut b = create_body(items);
    b.one_use = Some(one_use);
    let staging = tmp("staging");
    let built = build_all(&b, &staging).map_err(|e| e.1.message).unwrap();
    let v = register(&b, built, None, None).unwrap();
    (
        v["id"].as_str().unwrap().to_string(),
        v["room"].as_str().unwrap().to_string(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invite_accept_allow_then_the_files_and_nothing_before() {
    let app = app_folder("PSUP00001", "IV0000-PSUP00001_00-HOMEBREW00000000");
    let (id, room) = share_of(
        json!([
            {"kind": "app", "path": app},
            {"kind": "note", "text": "Have fun"},
            {"kind": "controls", "data": {"remap": {"cross": "circle"}}},
        ]),
        true,
    )
    .await;
    let (sock, link) = wire::mem_pair();
    tokio::spawn(wire::sender_session(sock, Arc::new(Registry), None));
    let sh = Arc::new(Mutex::new(RxShared::default()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sh2 = sh.clone();
    let room2 = room.clone();
    let task = tokio::spawn(async move { wire::receive(link, &room2, "Alex", sh2, &mut rx).await });
    until_rx(&sh, "offered").await;
    {
        let s = sh.lock().unwrap();
        assert_eq!(s.from, "Sam");
        assert_eq!(s.items.len(), 3);
        assert_eq!(s.items[0].name, "My App");
        assert!(s.items[0].size > 400_000);
    }
    // Nothing on the sender's side until the friend accepts.
    assert!(with_share(&id, |e| e.m.requests.is_empty()).unwrap());
    let dest = tmp("dest");
    tx.send(RxCmd::Accept {
        items: vec!["i1".into(), "i2".into()],
        dest: dest.clone(),
    })
    .unwrap();
    let rid = until_request(&id, "asking").await;
    until_rx(&sh, "asking").await;
    // Still nothing written before the Allow.
    assert!(std::fs::read_dir(&dest).unwrap().next().is_none());
    assert_eq!(decide_on(&id, rid, true).await, StatusCode::OK);
    assert_eq!(task.await.unwrap(), wire::RxEnd::Done);
    until_request(&id, "done").await;
    let s = sh.lock().unwrap();
    assert_eq!(s.state, "done");
    let app_dir = &s.received.iter().find(|r| r.0 == "i1").unwrap().1;
    same_tree(&app, app_dir);
    let note_dir = &s.received.iter().find(|r| r.0 == "i2").unwrap().1;
    assert_eq!(
        std::fs::read_to_string(note_dir.join("note.txt")).unwrap(),
        "Have fun"
    );
    assert!(
        s.received.iter().all(|r| r.0 != "i3"),
        "an item not accepted"
    );
    // One-use: the link is used.
    assert_eq!(with_share(&id, |e| e.m.state).unwrap(), ShareState::Used);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decline_and_a_cancel_and_a_used_link() {
    let app = app_folder("PSUP00002", "");
    let (id, room) = share_of(json!([{"kind": "app", "path": app}]), true).await;
    // Declined.
    let (sock, link) = wire::mem_pair();
    tokio::spawn(wire::sender_session(sock, Arc::new(Registry), None));
    let sh = Arc::new(Mutex::new(RxShared::default()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (sh2, room2) = (sh.clone(), room.clone());
    let t = tokio::spawn(async move { wire::receive(link, &room2, "A", sh2, &mut rx).await });
    until_rx(&sh, "offered").await;
    tx.send(RxCmd::Accept {
        items: vec!["i1".into()],
        dest: tmp("d1"),
    })
    .unwrap();
    let rid = until_request(&id, "asking").await;
    assert_eq!(decide_on(&id, rid, false).await, StatusCode::OK);
    t.await.unwrap();
    assert_eq!(sh.lock().unwrap().state, "declined");
    // The receiver cancels while asking.
    let (sock, link) = wire::mem_pair();
    tokio::spawn(wire::sender_session(sock, Arc::new(Registry), None));
    let sh = Arc::new(Mutex::new(RxShared::default()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (sh2, room2) = (sh.clone(), room.clone());
    let t = tokio::spawn(async move { wire::receive(link, &room2, "B", sh2, &mut rx).await });
    until_rx(&sh, "offered").await;
    tx.send(RxCmd::Accept {
        items: vec!["i1".into()],
        dest: tmp("d2"),
    })
    .unwrap();
    let rid_b = until_request(&id, "asking").await;
    tx.send(RxCmd::Cancel).unwrap();
    t.await.unwrap();
    assert_eq!(sh.lock().unwrap().state, "cancelled");
    for _ in 0..200 {
        if with_share(&id, |e| e.m.request(rid_b).unwrap().state).unwrap() == ReqState::Cancelled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        with_share(&id, |e| e.m.request(rid_b).unwrap().state).unwrap(),
        ReqState::Cancelled
    );
    // The sender cancels the share: a new friend is refused.
    let _ = cancel(axum::extract::Path(id.clone())).await;
    let (sock, link) = wire::mem_pair();
    tokio::spawn(wire::sender_session(sock, Arc::new(Registry), None));
    let sh = Arc::new(Mutex::new(RxShared::default()));
    let (_tx, mut rx) = mpsc::unbounded_channel();
    let (sh2, room2) = (sh.clone(), room.clone());
    wire::receive(link, &room2, "C", sh2, &mut rx).await;
    let s = sh.lock().unwrap();
    assert_eq!(s.state, "refused");
    assert_eq!(s.error.as_deref(), Some("cancelled"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sender_cancels_mid_transfer_and_the_receiver_stops() {
    let big = tmp("big");
    std::fs::create_dir_all(big.join("sce_sys")).unwrap();
    std::fs::write(
        big.join("sce_sys/param.json"),
        json!({"titleId": "BREW00001"}).to_string(),
    )
    .unwrap();
    std::fs::write(big.join("huge.bin"), vec![1u8; 64 * 1024 * 1024]).unwrap();
    let (id, room) = share_of(json!([{"kind": "app", "path": big}]), true).await;
    let (sock, link) = wire::mem_pair();
    tokio::spawn(wire::sender_session(sock, Arc::new(Registry), None));
    let sh = Arc::new(Mutex::new(RxShared::default()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (sh2, room2) = (sh.clone(), room.clone());
    let t = tokio::spawn(async move { wire::receive(link, &room2, "A", sh2, &mut rx).await });
    until_rx(&sh, "offered").await;
    let dest = tmp("d");
    tx.send(RxCmd::Accept {
        items: vec!["i1".into()],
        dest: dest.clone(),
    })
    .unwrap();
    let rid = until_request(&id, "asking").await;
    decide_on(&id, rid, true).await;
    until_rx(&sh, "receiving").await;
    let _ = cancel(axum::extract::Path(id.clone())).await;
    let end = tokio::time::timeout(Duration::from_secs(10), t)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end, wire::RxEnd::Over);
    assert_eq!(sh.lock().unwrap().state, "cancelled");
    assert!(sh.lock().unwrap().bytes_done < 64 * 1024 * 1024);
    // The 64 MiB file and what arrived of it.
    let _ = std::fs::remove_dir_all(&big);
    let _ = std::fs::remove_dir_all(&dest);
}

/// A receiver that asks for what it was not given: every such request is refused, and nothing
/// outside the share is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_accepted_manifest_is_served() {
    let app = app_folder("PSUP00003", "");
    let secret = tmp("secret");
    std::fs::write(secret.join("passwords.txt"), b"hunter2").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(secret.join("passwords.txt"), app.join("link.txt")).unwrap();
    let (id, room) = share_of(
        json!([{"kind": "app", "path": app}, {"kind": "note", "text": "n"}]),
        false,
    )
    .await;
    let (sock, mut link) = wire::mem_pair();
    tokio::spawn(wire::sender_session(sock, Arc::new(Registry), None));
    use wire::Link;
    link.raw(json!({"hello": {"v": 1, "room": room, "name": "Mallory"}}))
        .await;
    let offer = link.recv().await.unwrap();
    assert!(matches!(offer, wire::In::Text(ref t) if t.contains("\"offer\"")));
    // A get before the Allow is not answered with data.
    link.raw(json!({"get": {"n": 1, "item": "i1", "path": "eboot.bin", "offset": 0}}))
        .await;
    link.raw(json!({"accept": {"items": ["i2"]}})).await;
    let rid = until_request(&id, "asking").await;
    decide_on(&id, rid, true).await;
    let mut allowed = None;
    while let Some(m) = link.recv().await {
        if let wire::In::Text(t) = m {
            if t.contains("\"allowed\"") {
                allowed = Some(t);
                break;
            }
            assert!(!t.contains("\"eof\""));
        } else {
            panic!("data before the Allow");
        }
    }
    let allowed: Value = serde_json::from_str(&allowed.unwrap()).unwrap();
    let files = allowed["allowed"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "only the accepted note");
    for (n, item, path) in [
        (2, "i1", "eboot.bin"),            // an item not accepted
        (3, "i2", "../../etc/passwd"),     // traversal
        (4, "i2", "/etc/passwd"),          // absolute
        (5, "i2", "note.txt/../note.txt"), // traversal that lands on the list
        (6, "i2", "other.txt"),            // not on the list
        (7, "i1", "link.txt"),             // a link, never listed
        (8, "i9", "note.txt"),             // no such item
    ] {
        link.raw(json!({"get": {"n": n, "item": item, "path": path, "offset": 0}}))
            .await;
        match link.recv().await.unwrap() {
            wire::In::Text(t) => {
                let v: Value = serde_json::from_str(&t).unwrap();
                assert_eq!(v["error"]["n"], n, "{t}");
                assert!(
                    ["not_in_manifest", "unsafe_path"]
                        .contains(&v["error"]["why"].as_str().unwrap()),
                    "{t}"
                );
            }
            wire::In::Binary(_) => panic!("{path} was served"),
        }
    }
    // The note itself is served.
    link.raw(json!({"get": {"n": 9, "item": "i2", "path": "note.txt", "offset": 0}}))
        .await;
    match link.recv().await.unwrap() {
        wire::In::Binary(b) => {
            let (n, off, data) = wire::parse_data(&b).unwrap();
            assert_eq!((n, off, data), (9, 0, &b"n"[..]));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_transfer_resumes_with_its_token_and_a_one_use_link_takes_no_one_else() {
    let app = app_folder("PSUP00004", "");
    std::fs::write(
        app.join("big.bin"),
        (0..3_000_000u32)
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let (id, room) = share_of(json!([{"kind": "app", "path": app}]), true).await;
    let (sock, mut link) = wire::mem_pair();
    let sess = tokio::spawn(wire::sender_session(sock, Arc::new(Registry), None));
    use wire::Link;
    link.raw(json!({"hello": {"v": 1, "room": room, "name": "A"}}))
        .await;
    link.recv().await.unwrap();
    link.raw(json!({"accept": {"items": ["i1"]}})).await;
    let rid = until_request(&id, "asking").await;
    decide_on(&id, rid, true).await;
    let token = loop {
        if let Some(wire::In::Text(t)) = link.recv().await {
            if t.contains("\"allowed\"") {
                let v: Value = serde_json::from_str(&t).unwrap();
                break v["allowed"]["token"].as_str().unwrap().to_string();
            }
        }
    };
    // Part of big.bin arrives, then the connection drops.
    link.raw(json!({"get": {"n": 1, "item": "i1", "path": "big.bin", "offset": 0}}))
        .await;
    let first = link.recv().await.unwrap();
    let wire::In::Binary(b) = first else { panic!() };
    let got = wire::parse_data(&b).unwrap().2.len();
    let dest = tmp("resume");
    let dir = dest.join("My App");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("big.bin.part"), &b[wire::HEADER..]).unwrap();
    drop(link);
    let _ = tokio::time::timeout(Duration::from_secs(5), sess).await;
    assert_eq!(
        with_share(&id, |e| e.m.request(rid).unwrap().state).unwrap(),
        ReqState::Allowed,
        "kept for the resume"
    );
    // Someone new: the link is used.
    let (sock, mut other) = wire::mem_pair();
    tokio::spawn(wire::sender_session(sock, Arc::new(Registry), None));
    other
        .raw(json!({"hello": {"v": 1, "room": room, "name": "B"}}))
        .await;
    match other.recv().await.unwrap() {
        wire::In::Text(t) => assert!(t.contains("used"), "{t}"),
        _ => panic!(),
    }
    // The same friend carries on: the part is kept, the rest arrives.
    let sh = Arc::new(Mutex::new(RxShared {
        state: "reconnecting",
        token: Some(token),
        dest: Some(dest.clone()),
        accepted: vec!["i1".into()],
        items: vec![wire::Offered {
            id: "i1".into(),
            kind: "app".into(),
            name: "My App".into(),
            title_id: Some("PSUP00004".into()),
            format: Some("folder".into()),
            size: manifest::walk_local(&app)
                .unwrap()
                .0
                .iter()
                .map(|f| f.size)
                .sum(),
            files: 4,
            refused: None,
        }],
        ..RxShared::default()
    }));
    let (sock, link) = wire::mem_pair();
    tokio::spawn(wire::sender_session(sock, Arc::new(Registry), None));
    let (_tx, mut rx) = mpsc::unbounded_channel();
    let end = wire::receive(link, &room, "A", sh.clone(), &mut rx).await;
    assert_eq!(end, wire::RxEnd::Done, "{:?}", sh.lock().unwrap());
    same_tree(&app, &dir);
    assert!(got > 0);
}

/// Two engines' sides over a real WebRTC data channel (loopback, the fake hub introducing them):
/// a homebrew folder and a save, from the link to the files on disk, with the history and the
/// activity on both sides.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_engines_share_a_homebrew_folder_and_a_save_over_webrtc() {
    let hub = crate::join_net::tests::FakeHub::start().await;
    let _hub = test_hub(&hub.url, true).await;
    let app = app_folder("LAPY10010", "IV0000-LAPY10010_00-HOMEBREW00000000");
    // A shared-save file as Share a save makes it (save_copy.rs): <TITLE>/<image>/files.
    let save_dir = tmp("savezip");
    let zip_path = save_dir.join("PPSA01234 save shared.zip");
    {
        let src = save_dir.join("src");
        std::fs::create_dir_all(src.join("PPSA01234/sdimg_sce_sdmemory")).unwrap();
        std::fs::write(
            src.join("PPSA01234/sdimg_sce_sdmemory/save.dat"),
            vec![9u8; 50_000],
        )
        .unwrap();
        std::fs::write(
            src.join("PPSA01234").join(crate::save_backup::SHARE_MANIFEST),
            json!({"kind": crate::save_copy::SHARE_KIND, "title_id": "PPSA01234", "images": ["sdimg_sce_sdmemory"]}).to_string(),
        )
        .unwrap();
        ps5upload_core::save_archive::zip_folder(&src, "PPSA01234", &zip_path).unwrap();
    }
    let (s, v) = make(json!([
        {"kind": "app", "path": app},
        {"kind": "save", "path": zip_path, "name": "My App save"},
    ]))
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    // The room on the hub, then the link.
    let mut url = String::new();
    for _ in 0..200 {
        let (_, v) = body(get_one(axum::extract::Path(id.clone())).await).await;
        if v["works"] == "anywhere" && v["hub_state"] == "hosted" {
            url = v["url"].as_str().unwrap().to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(url.starts_with(crate::join_net::SHARE_PAGE), "{url}");
    assert!(!url.contains("k="), "no secret in the link");
    // The friend's engine opens it.
    let (_, r) = body(
        receive_open(Json(ShareOpenBody {
            link: url,
            name: Some("Alex".into()),
        }))
        .await,
    )
    .await;
    let rid_rx = r["id"].as_str().unwrap().to_string();
    let rx_view = || async {
        body(receive_get(axum::extract::Path(rid_rx.clone())).await)
            .await
            .1
    };
    let mut view = Value::Null;
    for _ in 0..400 {
        view = rx_view().await;
        if view["state"] == "offered" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(view["state"], "offered", "{view}");
    assert_eq!(view["from"], "Sam");
    assert_eq!(view["items"].as_array().unwrap().len(), 2);
    let dest = tmp("rtc-dest");
    let (s, _) = body(
        receive_accept(
            axum::extract::Path(rid_rx.clone()),
            Json(ShareAcceptBody {
                items: vec!["i1".into(), "i2".into()],
                dest: Some(dest.to_string_lossy().into_owned()),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // The sender sees "Alex wants: My App, My App save".
    let req = until_request(&id, "asking").await;
    let (_, sv) = body(get_one(axum::extract::Path(id.clone())).await).await;
    assert_eq!(sv["requests"][0]["name"], "Alex");
    assert_eq!(
        sv["requests"][0]["wants_names"],
        json!(["My App", "My App save"])
    );
    assert_eq!(decide_on(&id, req, true).await, StatusCode::OK);
    for _ in 0..800 {
        view = rx_view().await;
        if matches!(view["state"].as_str(), Some("done" | "failed")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(view["state"], "done", "{view}");
    let rec = view["received"].as_array().unwrap();
    let app_dir = PathBuf::from(
        rec.iter().find(|r| r["item"] == "i1").unwrap()["path"]
            .as_str()
            .unwrap(),
    );
    same_tree(&app, &app_dir);
    let save_got = PathBuf::from(
        rec.iter().find(|r| r["item"] == "i2").unwrap()["path"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(
        std::fs::read(save_got.join("PPSA01234 save shared.zip")).unwrap(),
        std::fs::read(&zip_path).unwrap()
    );
    // The received save opens as a shared save.
    let unz = tmp("unz");
    ps5upload_core::save_archive::unzip_strict(
        &save_got.join("PPSA01234 save shared.zip"),
        &unz,
        "PPSA01234",
    )
    .unwrap();
    assert_eq!(
        crate::save_copy::shared_parts(&unz, "PPSA01234")
            .unwrap()
            .len(),
        1
    );
    // Both sides: done, in the history.
    until_request(&id, "done").await;
    let (_, all) = body(list().await).await;
    let h = all["history"].as_array().unwrap();
    assert!(
        h.iter()
            .any(|e| e["dir"] == "sent" && e["name"] == "Alex" && e["state"] == "done"),
        "{all}"
    );
    assert!(
        h.iter()
            .any(|e| e["dir"] == "received" && e["name"] == "Sam" && e["state"] == "done"),
        "{all}"
    );
    assert!(view["dest"]
        .as_str()
        .is_some_and(|d| Path::new(d).join("share.json").exists()));
    let _ = cancel(axum::extract::Path(id)).await;
}

/// The app opening a link the hub can't reach falls back to the home network's socket.
#[tokio::test]
async fn a_link_with_only_a_home_network_hint_is_read() {
    let l =
        client::parse_link("http://192.168.1.10:19113/share#room=AbCdEfGhIjKlMnOpQrStUv").unwrap();
    assert!(l.hub.is_none());
    assert_eq!(
        client::lan_ws(l.lan.as_deref().unwrap()).as_deref(),
        Some("ws://192.168.1.10:19113/api/share/ws")
    );
}

#[test]
fn only_an_installed_apps_own_folder_reads_the_consoles_appmeta_copy() {
    // An installed app: its folder's name is the console's, so appmeta stands for its PARAM.
    assert_eq!(appmeta_title_id("/user/app/PPSA01234"), Some("PPSA01234"));
    assert_eq!(appmeta_title_id("/user/app/PPSA01234/"), Some("PPSA01234"));
    assert_eq!(
        appmeta_title_id("/mnt/ext0/user/app/PPSA01234"),
        Some("PPSA01234")
    );
    assert_eq!(
        appmeta_title_id("/system_ex/app/PPSA01234"),
        Some("PPSA01234")
    );
    // A folder the user named after a homebrew (a retail game with its sce_sys taken out) borrows
    // nothing: it has no PARAM, and is refused as unknown.
    assert_eq!(appmeta_title_id("/data/homebrew/FAKE77031"), None);
    assert_eq!(appmeta_title_id("/mnt/usb0/FAKE77031"), None);
    assert_eq!(appmeta_title_id("/mnt/a/b/user/app/FAKE77031"), None);
    assert_eq!(appmeta_title_id("/mnt//user/app/FAKE77031"), None);
    assert_eq!(appmeta_title_id("/user/app/../x"), None);
    assert_eq!(appmeta_title_id("FAKE77031"), None);
}

// ── A receive against a sender that goes quiet, lies about sizes, or lists names alike ───────────

/// A sender that offers one 10-byte file, allows it, takes the `get` and then says nothing. Holds
/// the connection open until `hold` is dropped.
async fn silent_sender(mut sock: wire::MemSocket, hold: tokio::sync::oneshot::Receiver<()>) {
    use crate::join::GuestSocket;
    use axum::extract::ws::Message;
    async fn next(sock: &mut wire::MemSocket) -> Value {
        loop {
            match sock.recv().await {
                Some(Ok(Message::Text(t))) => {
                    break serde_json::from_str::<Value>(t.as_str()).unwrap()
                }
                Some(_) => continue,
                None => panic!("the receiver left"),
            }
        }
    }
    assert!(next(&mut sock).await.get("hello").is_some());
    let offer = json!({"offer": {"from": "S", "title": "t", "items": [
        {"id": "i1", "kind": "file", "name": "thing", "size": 10, "files": 1}
    ]}});
    sock.send(wire::text(offer)).await.unwrap();
    assert!(next(&mut sock).await.get("accept").is_some());
    let allowed = json!({"allowed": {"token": "tok", "files": [
        {"item": "i1", "path": "a.bin", "size": 10}
    ]}});
    sock.send(wire::text(allowed)).await.unwrap();
    assert!(next(&mut sock).await.get("get").is_some());
    let _ = hold.await;
}

/// Cancel used to be looked at only between the sender's messages: with none coming, never.
#[tokio::test]
async fn cancel_ends_a_receive_whose_sender_went_quiet() {
    let (sock, link) = wire::mem_pair();
    let (hold, held) = tokio::sync::oneshot::channel();
    tokio::spawn(silent_sender(sock, held));
    let sh = Arc::new(Mutex::new(RxShared::default()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sh2 = sh.clone();
    let t = tokio::spawn(async move { wire::receive(link, "room", "A", sh2, &mut rx).await });
    until_rx(&sh, "offered").await;
    let dest = tmp("quiet");
    tx.send(RxCmd::Accept {
        items: vec!["i1".into()],
        dest: dest.clone(),
    })
    .unwrap();
    until_rx(&sh, "receiving").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    tx.send(RxCmd::Cancel).unwrap();
    let end = tokio::time::timeout(Duration::from_secs(5), t)
        .await
        .expect("Cancel was not heard while the sender was silent")
        .unwrap();
    assert_eq!(end, wire::RxEnd::Over);
    assert_eq!(sh.lock().unwrap().state, "cancelled");
    drop(hold);
    let _ = std::fs::remove_dir_all(&dest);
}

/// With no Cancel either, a silent sender is a lost connection after the idle wait.
#[tokio::test]
async fn a_receive_gives_up_on_a_sender_silent_for_too_long() {
    let (sock, link) = wire::mem_pair();
    let (hold, held) = tokio::sync::oneshot::channel();
    tokio::spawn(silent_sender(sock, held));
    let sh = Arc::new(Mutex::new(RxShared::default()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sh2 = sh.clone();
    let t = tokio::spawn(async move {
        wire::receive_with(link, "room", "A", sh2, &mut rx, Duration::from_millis(300)).await
    });
    until_rx(&sh, "offered").await;
    let dest = tmp("idle");
    tx.send(RxCmd::Accept {
        items: vec!["i1".into()],
        dest: dest.clone(),
    })
    .unwrap();
    let end = tokio::time::timeout(Duration::from_secs(5), t)
        .await
        .expect("a silent sender held the receive")
        .unwrap();
    assert_eq!(end, wire::RxEnd::Lost);
    drop(hold);
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn a_listing_whose_sizes_wrap_is_more_than_the_offer() {
    assert_eq!(wire::listed_total([4, 6], 10), Some(10));
    // A little slack for a file that grew.
    assert_eq!(wire::listed_total([10, 1 << 20], 10), Some(10 + (1 << 20)));
    assert_eq!(wire::listed_total([10, (1 << 20) + 2], 10), None);
    // 2^64 + 2 bytes: the wrapped sum was 2, and passed.
    assert_eq!(wire::listed_total([u64::MAX, 3], 10), None);
    assert_eq!(wire::listed_total([u64::MAX / 2 + 1; 2], u64::MAX), None);
}

#[test]
fn a_long_name_keeps_its_extension_and_two_alike_stay_two() {
    let long = "x".repeat(300);
    let a = wire::safe_name(&format!("{long}-disc1.pkg"));
    assert!(a.ends_with(".pkg") && a.len() <= wire::NAME_MAX, "{a}");
    // Cut on a character, not in one.
    let jp = wire::safe_name(&format!("{}.pkg", "ゲ".repeat(200)));
    assert!(jp.ends_with(".pkg") && jp.len() <= wire::NAME_MAX);
    assert_eq!(wire::safe_name("Game v1.02.pkg"), "Game v1.02.pkg");
    assert_eq!(wire::safe_name("a:b?.txt"), "a_b_.txt");
    // A Windows device name, whatever its extension or case, is never a name to write to.
    assert_eq!(wire::safe_name("NUL.pkg"), "_NUL.pkg");
    assert_eq!(wire::safe_name("con"), "_con");
    assert_eq!(wire::safe_name("COM1.bin"), "_COM1.bin");
    assert_eq!(wire::safe_name("console.pkg"), "console.pkg");

    // Under 200 bytes nothing is cut: names that differ after their 80th character stay apart.
    let listed = |path: String| wire::Listed {
        item: "i1".into(),
        path,
        size: 5,
    };
    let dir = PathBuf::from("dest").join("thing");
    let dirs = vec![("i1".to_string(), dir.clone())];
    let p = "p".repeat(90);
    let plan = wire::plan_targets(
        vec![
            listed(format!("{p}-part1.bin")),
            listed(format!("{p}-part2.bin")),
            // Alike once cut to the limit.
            listed(format!("{long}-1.bin")),
            listed(format!("{long}-2.bin")),
            // One file on Windows and macOS.
            listed("Data/Save.dat".into()),
            listed("data/save.dat".into()),
            // Alike once made safe.
            listed("a*b.txt".into()),
            listed("a?b.txt".into()),
        ],
        &dirs,
    )
    .unwrap();
    let keys: std::collections::HashSet<String> = plan
        .iter()
        .map(|(_, t)| t.to_string_lossy().to_lowercase())
        .collect();
    assert_eq!(keys.len(), plan.len(), "two files share a target: {plan:?}");
    for (f, t) in &plan {
        let name = t.file_name().unwrap().to_str().unwrap();
        assert!(name.len() <= wire::NAME_MAX + 5, "{name}");
        let ext = f.path.rsplit('.').next().unwrap();
        assert!(name.ends_with(&format!(".{ext}")), "{name} lost .{ext}");
    }
    assert_eq!(plan[0].1, dir.join(format!("{p}-part1.bin")));
    assert_eq!(plan[7].1, dir.join("a_b (2).txt"));
    // Still refused: a path outside its item, an item not accepted.
    assert_eq!(
        wire::plan_targets(vec![listed("../x".into())], &dirs).unwrap_err(),
        wire::PlanRefused::Unsafe
    );
    assert_eq!(
        wire::plan_targets(vec![listed("x".into())], &[]).unwrap_err(),
        wire::PlanRefused::NotAccepted
    );
}

#[test]
fn a_left_share_is_swept_only_once_no_share_can_still_be_live() {
    assert_eq!(LEFT_SHARE_AGE, MAX_TTL + Duration::from_secs(24 * 3600));
    assert!(LEFT_SHARE_AGE >= Duration::from_secs(8 * 24 * 3600));
}
