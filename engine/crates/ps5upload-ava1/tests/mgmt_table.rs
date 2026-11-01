//! The core crate's method table agrees with the tracked checklist (`MGMT_METHODS.md`), which
//! agrees with the schema: a drifted number would send a call to the wrong handler.

use std::collections::HashMap;

use ps5upload_core::mgmt::m;

#[test]
fn the_method_table_matches_the_checklist() {
    let md = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../protocol/ps5upload-ava1/MGMT_METHODS.md"
    ))
    .unwrap();
    // method number -> its checklist name, for rows whose method cell is `N `name``; and the
    // subset that replaced a retired frame (the first cell ends in the frame's number).
    let mut rows: HashMap<u16, String> = HashMap::new();
    let mut replaced: HashMap<u16, String> = HashMap::new();
    for line in md.lines() {
        let c: Vec<&str> = line.split('|').map(str::trim).collect();
        if c.len() < 6 {
            continue;
        }
        let mut it = c[2].splitn(2, ' ');
        let (Some(id), Some(rest)) = (it.next().and_then(|n| n.parse::<u16>().ok()), it.next())
        else {
            continue;
        };
        if rest.starts_with('`') && rest.ends_with('`') && !rest.contains(' ') {
            let name = rest.trim_matches('`').to_string();
            rows.insert(id, name.clone());
            if c[1]
                .rsplit(' ')
                .next()
                .and_then(|n| n.parse::<u16>().ok())
                .is_some()
            {
                replaced.insert(id, name);
            }
        }
    }
    // Every method in the core table has a checklist row.
    for x in m::ALL {
        assert!(
            rows.contains_key(&x.id),
            "{} (method {}) has no checklist row",
            x.label,
            x.id
        );
    }
    // Every row that replaced a retired frame (except the data-plane methods the typed helpers
    // own) has an entry in the table.
    for (id, name) in &replaced {
        if matches!(id, 1 | 16 | 17 | 18) {
            continue;
        }
        assert!(
            m::ALL.iter().any(|x| x.id == *id),
            "checklist method {id} ({name}) is missing from the table"
        );
    }
    assert!(m::ALL.len() >= 95);
}

#[test]
fn the_ids_the_gate_and_the_converters_key_on_are_the_schema_numbers() {
    use ps5upload_ava1::gen;
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
    assert_eq!(m::FILES_PREFS_GET.id, gen::METHOD_FILES_PREFS_GET);
    assert_eq!(m::FILES_PREFS_SET.id, gen::METHOD_FILES_PREFS_SET);
    assert_eq!(m::FILES_HANDOFF_PUT.id, gen::METHOD_FILES_HANDOFF_PUT);
    assert_eq!(m::FILES_HANDOFF_TAKE.id, gen::METHOD_FILES_HANDOFF_TAKE);
    assert_eq!(m::MINION_TILE.id, gen::METHOD_MINION_TILE);
    assert_eq!(m::VINPUT_CAPS.id, gen::METHOD_VINPUT_CAPS);
    assert_eq!(m::VINPUT_SESSION.id, gen::METHOD_VINPUT_SESSION);
    assert_eq!(m::VINPUT_REPORT.id, gen::METHOD_VINPUT_REPORT);
    assert_eq!(m::CLEANUP_SCAN.id, gen::METHOD_CLEANUP_SCAN);
    assert_eq!(m::CLEANUP_STATUS.id, gen::METHOD_CLEANUP_STATUS);
    assert_eq!(m::CLEANUP_APPLY.id, gen::METHOD_CLEANUP_APPLY);
    assert_eq!(m::CLEANUP_APPLY_STATUS.id, gen::METHOD_CLEANUP_APPLY_STATUS);
    assert_eq!(m::CLEANUP_CANCEL.id, gen::METHOD_CLEANUP_CANCEL);
    assert_eq!(m::SMB_SHARES_LIST.id, gen::METHOD_SMB_SHARES_LIST);
    assert_eq!(m::SMB_SHARES_SAVE.id, gen::METHOD_SMB_SHARES_SAVE);
    assert_eq!(m::SMB_SHARES_REMOVE.id, gen::METHOD_SMB_SHARES_REMOVE);
    // The core's feature detect (an older helper) keys on the schema's unknown-method status.
    assert_eq!(
        ps5upload_core::console_files::ERR_UNKNOWN_METHOD,
        gen::ERR_UNKNOWN_METHOD
    );
}
