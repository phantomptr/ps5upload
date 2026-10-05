//! The core crate's method table agrees with the tracked checklist (`MGMT_METHODS.md`), which
//! agrees with the schema: a drifted number would send a call to the wrong handler.

use std::collections::HashMap;

use ps5upload_core::mgmt::m;

#[test]
fn the_method_table_matches_the_checklist() {
    let md = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../protocol/ava1/MGMT_METHODS.md"
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
