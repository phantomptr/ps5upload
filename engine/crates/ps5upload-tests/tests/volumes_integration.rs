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
