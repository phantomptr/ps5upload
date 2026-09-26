//! Integration tests for FS_LIST_VOLUMES against the in-process mock server.
//!
//! These exercise:
//!   - normal path: fake volume list parses correctly into VolumeList
//!   - VolumeList::find helper
//!   - empty list handling
//!   - overridden MockState propagates through the protocol

mod mock_server;
use mock_server::MockServer;

use ps5upload_core::volumes::list_volumes;

#[test]
fn list_volumes_returns_default_fixtures() {
    let srv = MockServer::start();
    let result = list_volumes(&srv.addr).expect("list_volumes should succeed");

    // Default fixture defines /data and /ext0.
    assert_eq!(result.volumes.len(), 2);
    let data = result.find("/data").expect("/data present");
    assert_eq!(data.fs_type, "ufs");
    assert!(data.writable);
    assert_eq!(data.total_bytes, 800_000_000_000);
    let ext0 = result.find("/ext0").expect("/ext0 present");
    assert_eq!(ext0.free_bytes, 900_000_000_000);
}

#[test]
fn list_volumes_respects_state_override() {
    let srv = MockServer::start();
    {
        let mut st = srv.state.lock().unwrap();
        st.volumes_json = r#"{"volumes":[
            {"path":"/usb0","fs_type":"exfat","total_bytes":64000000000,"free_bytes":1000000000,"writable":false}
        ]}"#
        .to_string();
    }
    let result = list_volumes(&srv.addr).expect("list_volumes should succeed");
    assert_eq!(result.volumes.len(), 1);
    let usb = result.find("/usb0").expect("/usb0 present");
    assert_eq!(usb.fs_type, "exfat");
    assert!(!usb.writable, "readonly flag must round-trip");
    assert!(result.find("/data").is_none());
}

#[test]
fn list_volumes_empty() {
    let srv = MockServer::start();
    {
        let mut st = srv.state.lock().unwrap();
        st.volumes_json = r#"{"volumes":[]}"#.to_string();
    }
    let result = list_volumes(&srv.addr).expect("list_volumes should succeed on empty list");
    assert!(result.volumes.is_empty());
}

/// Pins the 2.2.51 payload contract: a `.ffpkg` mounted at a user-chosen
/// path (e.g. `/data/homebrew/PPSA17599`) surfaces in the FS_LIST_VOLUMES
/// response with its `source_image` populated from the payload's
/// `.src` tracker — and the engine round-trips that field cleanly. The
/// pre-2.2.51 payload bug filtered such non-prefixed paths out before
/// the tracker check; this integration test catches a regression that
/// re-introduces a payload-side path filter while leaving the wire shape
/// unchanged.
#[test]
fn list_volumes_surfaces_user_chosen_mount_with_tracker() {
    let srv = MockServer::start();
    {
        let mut st = srv.state.lock().unwrap();
        st.volumes_json = r#"{"volumes":[
            {"path":"/data","mount_from":"/dev/ssd0.user","fs_type":"nullfs","total_bytes":800000000000,"free_bytes":500000000000,"writable":true,"is_placeholder":false},
            {"path":"/data/homebrew/PPSA17599","mount_from":"/dev/lvd0","fs_type":"ufs","total_bytes":50000000000,"free_bytes":0,"writable":true,"is_placeholder":false,"source_image":"/data/homebrew/PPSA17599.ffpkg"}
        ]}"#.to_string();
    }
    let result = list_volumes(&srv.addr).expect("list_volumes should succeed");
    let mount = result
        .find("/data/homebrew/PPSA17599")
        .expect("user-chosen mount path is surfaced");
    assert_eq!(mount.source_image, "/data/homebrew/PPSA17599.ffpkg");
    assert_eq!(mount.fs_type, "ufs");
    assert!(
        mount.writable,
        "writable flag should round-trip from MNT_RDONLY=0",
    );
    // Library scan reads source_image off the volume; back-compat default
    // for older payloads is "" (no tracker recorded).
    let data = result.find("/data").expect("/data present");
    assert_eq!(data.source_image, "", "non-ours mounts have empty source");
}
