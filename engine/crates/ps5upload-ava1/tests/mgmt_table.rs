//! The core crate's method table agrees with the tracked checklist (`MGMT_METHODS.md`), which
//! agrees with the schema: a drifted number would send a call to the wrong handler.

use std::collections::HashMap;

use ps5upload_core::mgmt::m;

#[test]
fn the_method_table_matches_the_checklist_row_for_row() {
    let md = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../protocol/ava1/MGMT_METHODS.md"
    ))
    .unwrap();
    // frame number -> method id, for rows that map to one plain method.
    let mut rows: HashMap<u16, u16> = HashMap::new();
    for line in md.lines() {
        let c: Vec<&str> = line.split('|').map(str::trim).collect();
        if c.len() < 6 {
            continue;
        }
        let Some(frame) = c[1].rsplit(' ').next().and_then(|n| n.parse::<u16>().ok()) else {
            continue;
        };
        let mut it = c[2].splitn(2, ' ');
        let (Some(id), Some(rest)) = (it.next().and_then(|n| n.parse::<u16>().ok()), it.next())
        else {
            continue;
        };
        if rest.starts_with('`') && rest.ends_with('`') && !rest.contains(' ') {
            rows.insert(frame, id);
        }
    }
    let mut checked = 0;
    for x in m::ALL {
        let Some((req, _)) = x.ftx2 else { continue };
        let want = rows
            .get(&(req as u16))
            .unwrap_or_else(|| panic!("{} (frame {}) has no checklist row", x.label, req as u16));
        assert_eq!(
            *want, x.id,
            "{}: method number differs from the checklist",
            x.label
        );
        checked += 1;
    }
    // Every plain row (except the data-plane methods the typed helpers own) has an entry.
    for (frame, id) in rows {
        if matches!(id, 1 | 16 | 17 | 18) {
            continue;
        }
        assert!(
            m::ALL.iter().any(|x| x.id == id),
            "checklist method {id} (frame {frame}) is missing from the table"
        );
    }
    assert!(checked >= 95);
}

/// The ack frame of every method is pinned: FTX2 answers with it and a wrong one fails the call.
/// Frame numbers are the payload's (`runtime.c` dispatch); an ack is the request + 1 except the
/// Remote Play frames, which ack with themselves or with status (`remoteplay.rs`).
#[test]
fn every_ack_frame_is_pinned() {
    for x in m::ALL {
        let Some((req, ack)) = x.ftx2 else { continue };
        let (r, a) = (req as u16, ack as u16);
        let expect = match r {
            188 | 189 => 189, // RemotePlayRequest and Status ack with RemotePlayStatus
            248..=250 => r,   // readiness, enable, devices ack with their own frame
            _ => r + 1,
        };
        assert_eq!(a, expect, "{}: request {r} acks with {a}", x.label);
    }
}

#[test]
fn the_ids_the_gate_and_the_converters_key_on_are_the_schema_numbers() {
    use ava1::gen;
    assert_eq!(m::NODE_STATUS.id, gen::METHOD_NODE_STATUS);
    assert_eq!(m::FS_LIST.id, gen::METHOD_FS_LIST);
    assert_eq!(m::FS_STAT.id, gen::METHOD_FS_STAT);
    assert_eq!(m::FS_FREESPACE.id, gen::METHOD_FS_FREESPACE);
    assert_eq!(m::FS_MKDIR.id, gen::METHOD_FS_MKDIR);
    assert_eq!(m::FS_RENAME.id, gen::METHOD_FS_RENAME);
    assert_eq!(m::FS_CHMOD.id, gen::METHOD_FS_CHMOD);
    assert_eq!(m::FS_READ.id, gen::METHOD_FS_READ);
    assert_eq!(m::FS_WRITE.id, gen::METHOD_FS_WRITE);
    assert_eq!(m::HW_INFO.id, gen::METHOD_HW_INFO);
    assert_eq!(m::LOG_KLOG.id, gen::METHOD_LOG_KLOG);
    assert_eq!(m::LOG_SYSLOG.id, gen::METHOD_LOG_SYSLOG);
}
