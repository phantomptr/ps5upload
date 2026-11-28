//! ps5upload's AVA1 application schema (protocol/ps5upload-ava1/app.toml): its bodies round-trip,
//! its method numbers stay out of the protocol's ranges and never repeat, and its sizes fit the
//! protocol's RPC caps.
use ava1::wire::Message;
use ps5upload_ava1::gen::*;

const J: [u8; 16] = [7; 16];

fn fs_list() -> FsList {
    FsList {
        path: "/data".into(),
        offset: 256,
        limit: 256,
    }
}

fn fs_list_result() -> FsListResult {
    FsListResult {
        entries: vec![
            FsEntry {
                name: "a.bin".into(),
                kind: ENTRY_FILE,
                size: 3,
                mtime: Some(1_700_000_000),
                mode: Some(0o644),
            },
            FsEntry {
                name: "sub".into(),
                kind: ENTRY_DIR,
                size: 0,
                mtime: None,
                mode: None,
            },
        ],
        total_scanned: 2,
        more: 1,
    }
}

fn fs_write_chunk() -> FsWrite {
    FsWrite {
        path: "/data/a".into(),
        offset: 49_152,
        flags: FSW_AT_OFFSET | FSW_COMMIT | FSW_OVERWRITE,
        data: vec![1, 2, 3],
        mode: Some(0o600),
    }
}

#[test]
fn management_structs_round_trip() {
    let st = NodeStatus {
        version: "5.42.0".into(),
        ps5_kernel: "13.60".into(),
        instance_id: 9,
        started_at_unix: 1_700_000_000,
        command_count: 4,
        startup_reason: 1,
        ucred_elevated: 1,
        max_transfer_streams: 4,
        fan_threshold: 70,
        fan_reapply_sec: 30,
        prior_instance: Some("clean".into()),
        usbnet: Some(r#"{"ip":"192.168.1.50","port":9120,"mbps":2500,"chip":"RTL8156B"}"#.into()),
    };
    assert_eq!(NodeStatus::decode(&st.to_bytes().unwrap()).unwrap(), st);
    let l = fs_list();
    assert_eq!(FsList::decode(&l.to_bytes().unwrap()).unwrap(), l);
    let r = fs_list_result();
    assert_eq!(FsListResult::decode(&r.to_bytes().unwrap()).unwrap(), r);
    let p = FsPath {
        path: "/data/x".into(),
    };
    assert_eq!(FsPath::decode(&p.to_bytes().unwrap()).unwrap(), p);
    let s = FsStat {
        kind: ENTRY_DIR,
        size: 0,
        mtime: 5,
        mode: 0o755,
        dev: 0x1_0000_0001,
    };
    assert_eq!(FsStat::decode(&s.to_bytes().unwrap()).unwrap(), s);
    let m = FsMkdir {
        path: "/data/a/b".into(),
        mode: 0o777,
        parents: 1,
    };
    assert_eq!(FsMkdir::decode(&m.to_bytes().unwrap()).unwrap(), m);
    let rn = FsRename {
        from: "/data/a".into(),
        to: "/data/b".into(),
        overwrite: 0,
    };
    assert_eq!(FsRename::decode(&rn.to_bytes().unwrap()).unwrap(), rn);
    let c = FsChmod {
        path: "/data/a".into(),
        mode: 0o644,
    };
    assert_eq!(FsChmod::decode(&c.to_bytes().unwrap()).unwrap(), c);
    let rd = FsRead {
        path: "/data/a".into(),
        offset: 1 << 40,
        len: 48 * 1024,
        flags: FSR_UNSAFE,
    };
    assert_eq!(FsRead::decode(&rd.to_bytes().unwrap()).unwrap(), rd);
    let rr = FsReadResult {
        data: vec![7; 100],
        eof: 1,
    };
    assert_eq!(FsReadResult::decode(&rr.to_bytes().unwrap()).unwrap(), rr);
    let w = fs_write_chunk();
    assert_eq!(FsWrite::decode(&w.to_bytes().unwrap()).unwrap(), w);
    let j = JobRun {
        job_id: J,
        op: JOB_OP_DELETE,
        args: b"{\"path\":\"/data/x\"}".to_vec(),
    };
    assert_eq!(JobRun::decode(&j.to_bytes().unwrap()).unwrap(), j);
}

#[test]
fn every_method_constant_is_unique_and_below_65535() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let mut seen = std::collections::BTreeMap::new();
    for (file, app) in [
        ("third_party/ava1/schema/ava1.toml", false),
        ("protocol/ps5upload-ava1/app.toml", true),
    ] {
        let src = std::fs::read_to_string(format!("{root}/{file}")).unwrap();
        let mut name = String::new();
        for line in src.lines() {
            if let Some(n) = line.strip_prefix("name = \"METHOD_") {
                name = format!("METHOD_{}", n.split('"').next().unwrap());
            } else if let Some(v) = line.strip_prefix("value = ") {
                if !name.is_empty() {
                    let v: u32 = v.trim().parse().unwrap();
                    assert!((1..65_535).contains(&v), "{name} = {v}");
                    if app {
                        assert!(
                            (4..=15).contains(&v) || v >= 32,
                            "{name} = {v} is in the protocol's range"
                        );
                    }
                    if let Some(prev) = seen.insert(v, name.clone()) {
                        panic!("{name} and {prev} are both method {v}");
                    }
                    name.clear();
                }
            } else if line.starts_with("name = ") {
                name.clear();
            }
        }
    }
    // Pins: the numbers the C and Rust dispatchers and the engine seam rely on.
    for (n, v) in [
        ("node.status", 4),
        ("job.run", 20),
        ("fs.list", 33),
        ("fs.rename", 36),
        ("rp.devices", 141),
    ] {
        let c = format!("METHOD_{}", n.to_uppercase().replace('.', "_"));
        assert_eq!(seen.get(&v), Some(&c));
    }
    assert!(seen.len() >= 106, "{} methods", seen.len());
}

#[test]
fn fs_write_flags_are_distinct_bits() {
    let f = [
        FSW_APPEND,
        FSW_AT_OFFSET,
        FSW_COMMIT,
        FSW_CREATE,
        FSW_OVERWRITE,
    ];
    for (i, a) in f.iter().enumerate() {
        assert_eq!(a.count_ones(), 1);
        for b in &f[i + 1..] {
            assert_eq!(a & b, 0);
        }
    }
}

#[test]
fn the_size_constants_agree_with_the_rpc_caps() {
    // MgmtText overhead is 6 bytes, 13 with `more`; FsReadResult is data + 7.
    assert_eq!(RPC_TEXT_MAX as usize, ava1::server::RPC_REPLY_MAX - 16);
    assert_eq!(FS_READ_MAX, RPC_TEXT_MAX);
    let worst = MgmtText {
        body: vec![b'x'; RPC_TEXT_MAX as usize],
        more: Some(1),
    }
    .to_bytes()
    .unwrap();
    assert!(worst.len() <= ava1::server::RPC_REPLY_MAX);
    let r = FsReadResult {
        data: vec![0; FS_READ_MAX as usize],
        eof: 1,
    }
    .to_bytes()
    .unwrap();
    assert!(r.len() <= ava1::server::RPC_REPLY_MAX);
    // The largest chunk plus a maximal path and header fits the 56 KiB request cap.
    let w = FsWrite {
        path: "p".repeat(1024),
        offset: u64::MAX,
        flags: u32::MAX,
        data: vec![0; FSW_CHUNK_MAX as usize],
        mode: Some(0o644),
    }
    .to_bytes()
    .unwrap();
    assert!(w.len() <= ava1::server::RPC_REQUEST_MAX);
}
